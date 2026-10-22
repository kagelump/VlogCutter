//! Durable terminal-job event delivery. Targets are opaque names; execution
//! belongs to an external consumer, never to the server or a SQLite transaction.
use crate::{
    Clock, JobEngineError, JobEvent, JobId, open_read_only_connection, open_writable_connection,
};
use chrono::{Duration, SecondsFormat};
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use std::{path::PathBuf, sync::Arc};
use uuid::Uuid;

#[derive(Clone)]
pub struct JobCallbacks {
    pub(crate) database_path: PathBuf,
    pub(crate) clock: Arc<dyn Clock>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallbackSubscription {
    pub subscription_id: String,
    pub job_id: JobId,
    pub target: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallbackDelivery {
    pub delivery_id: String,
    pub sequence: u64,
    pub subscription_id: String,
    pub target: String,
    pub event: JobEvent,
    pub state: String,
    pub attempt_count: u32,
    pub available_at: String,
    pub lease_expires_at: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallbackClaim {
    pub delivery: CallbackDelivery,
    pub lease_token: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallbackPage {
    pub items: Vec<CallbackDelivery>,
    pub next_before: Option<u64>,
}

fn validate_target(target: &str) -> Result<(), JobEngineError> {
    if target.is_empty()
        || target.len() > 128
        || !target
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
    {
        return Err(JobEngineError::InvalidInput(
            "callback target must be a 1..128 byte identifier".into(),
        ));
    }
    Ok(())
}

impl JobCallbacks {
    /// Idempotent by (job, target). Registering after completion catches up
    /// with that job's latest terminal event in this same write transaction.
    pub fn subscribe(
        &self,
        job: &JobId,
        target: &str,
    ) -> Result<CallbackSubscription, JobEngineError> {
        validate_target(target)?;
        let mut c = open_writable_connection(&self.database_path)?;
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let state: String = tx
            .query_row(
                "SELECT state FROM jobs WHERE job_id=?1",
                [job.as_str()],
                |r| r.get(0),
            )
            .optional()?
            .ok_or_else(|| JobEngineError::NotFound(job.to_string()))?;
        let id = format!("callback_{}", Uuid::new_v4().simple());
        tx.execute("INSERT INTO job_callback_subscriptions(subscription_id,job_id,target) VALUES(?1,?2,?3) ON CONFLICT(job_id,target) DO NOTHING", params![id,job.as_str(),target])?;
        let subscription_id = tx.query_row(
            "SELECT subscription_id FROM job_callback_subscriptions WHERE job_id=?1 AND target=?2",
            params![job.as_str(), target],
            |r| r.get(0),
        )?;
        if matches!(state.as_str(), "succeeded" | "failed" | "canceled") {
            // Reuse the canonical event log, including jobs completed before
            // migration 11. Never construct an old event from mutable counters.
            let row = tx.query_row(
                "SELECT event_sequence,attempt_number,event_type,details_json,created_at FROM job_events WHERE job_id=?1 AND state=?2 ORDER BY event_sequence DESC LIMIT 1",
                params![job.as_str(),state],
                |r| Ok((r.get::<_,i64>(0)?,r.get::<_,Option<u32>>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?)),
            ).optional()?;
            let event = row.map(|(sequence,attempt,event_type,details,created_at)| -> Result<JobEvent,JobEngineError> {
                Ok(JobEvent { event_sequence:sequence as u64,job_id:job.clone(),attempt_number:attempt,event_type,state:serde_json::from_value(serde_json::Value::String(state))?,details:serde_json::from_str(&details)?,created_at })
            }).transpose()?;
            if let Some(event) = event {
                record_terminal(&tx, &event)?;
            }
        }
        tx.commit()?;
        Ok(CallbackSubscription {
            subscription_id,
            job_id: job.clone(),
            target: target.into(),
        })
    }

    pub fn list(
        &self,
        target: &str,
        limit: u32,
        before: Option<u64>,
    ) -> Result<CallbackPage, JobEngineError> {
        validate_target(target)?;
        if !(1..=200).contains(&limit) || before.is_some_and(|v| v > i64::MAX as u64) {
            return Err(JobEngineError::InvalidInput("invalid callback page".into()));
        }
        let c = open_read_only_connection(&self.database_path)?;
        let mut statement = c.prepare("SELECT delivery_id FROM job_callback_deliveries WHERE target=?1 AND (?2 IS NULL OR sequence<?2) ORDER BY sequence DESC LIMIT ?3")?;
        let ids = statement
            .query_map(params![target, before.map(|v| v as i64), limit + 1], |r| {
                r.get::<_, String>(0)
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut items = ids
            .iter()
            .take(limit as usize)
            .map(|id| load_delivery(&c, id))
            .collect::<Result<Vec<_>, _>>()?;
        let next_before = if ids.len() > limit as usize {
            items.last().map(|d| d.sequence)
        } else {
            None
        };
        Ok(CallbackPage {
            items: std::mem::take(&mut items),
            next_before,
        })
    }

    pub fn claim(
        &self,
        target: &str,
        lease_seconds: u32,
    ) -> Result<Option<CallbackClaim>, JobEngineError> {
        validate_target(target)?;
        if !(1..=3600).contains(&lease_seconds) {
            return Err(JobEngineError::InvalidInput(
                "callback lease must be 1..3600 seconds".into(),
            ));
        }
        let now = self.clock.now();
        let expires = (now + Duration::seconds(lease_seconds.into()))
            .to_rfc3339_opts(SecondsFormat::Millis, true);
        let now = now.to_rfc3339_opts(SecondsFormat::Millis, true);
        let mut c = open_writable_connection(&self.database_path)?;
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let id: Option<String> = tx.query_row("SELECT delivery_id FROM job_callback_deliveries WHERE target=?1 AND available_at<=?2 AND (state='pending' OR (state='leased' AND lease_expires_at<=?2)) ORDER BY sequence LIMIT 1",params![target,now],|r|r.get(0)).optional()?;
        let Some(id) = id else {
            return Ok(None);
        };
        let token = Uuid::new_v4().to_string();
        tx.execute("UPDATE job_callback_deliveries SET state='leased',attempt_count=attempt_count+1,lease_token=?1,lease_expires_at=?2 WHERE delivery_id=?3",params![token,expires,id])?;
        let delivery = load_delivery(&tx, &id)?;
        tx.commit()?;
        Ok(Some(CallbackClaim {
            delivery,
            lease_token: token,
        }))
    }

    /// Acknowledgement is idempotent for the successful lease. A stale worker
    /// cannot acknowledge or release a delivery reclaimed by another worker.
    pub fn settle(
        &self,
        id: &str,
        token: &str,
        succeeded: bool,
    ) -> Result<CallbackDelivery, JobEngineError> {
        let now = self
            .clock
            .now()
            .to_rfc3339_opts(SecondsFormat::Millis, true);
        let mut c = open_writable_connection(&self.database_path)?;
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current: Option<(String,String,Option<String>)> = tx.query_row("SELECT state,COALESCE(lease_token,''),lease_expires_at FROM job_callback_deliveries WHERE delivery_id=?1",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
        let Some((state, lease, expires)) = current else {
            return Err(JobEngineError::NotFound(id.into()));
        };
        if token.is_empty()
            || lease != token
            || !(state == "delivered" && succeeded
                || state == "leased" && expires.as_deref() > Some(now.as_str()))
        {
            return Err(JobEngineError::StaleLease);
        }
        if state != "delivered" {
            tx.execute("UPDATE job_callback_deliveries SET state=?1,lease_expires_at=NULL,available_at=?2 WHERE delivery_id=?3",params![if succeeded {"delivered"} else {"pending"},now,id])?;
        }
        let delivery = load_delivery(&tx, id)?;
        tx.commit()?;
        Ok(delivery)
    }
}

fn load_delivery(c: &rusqlite::Connection, id: &str) -> Result<CallbackDelivery, JobEngineError> {
    let row = c.query_row("SELECT sequence,subscription_id,target,event_json,state,attempt_count,available_at,lease_expires_at FROM job_callback_deliveries WHERE delivery_id=?1",[id],|r| Ok((r.get::<_,i64>(0)? as u64,r.get(1)?,r.get(2)?,r.get::<_,String>(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?)))?;
    Ok(CallbackDelivery {
        delivery_id: id.into(),
        sequence: row.0,
        subscription_id: row.1,
        target: row.2,
        event: serde_json::from_str(&row.3)?,
        state: row.4,
        attempt_count: row.5,
        available_at: row.6,
        lease_expires_at: row.7,
    })
}

/// Called inside the transaction that persists the terminal job event, for
/// success, exhausted retries, dependency failure and cancellation alike.
pub(crate) fn record_terminal(
    tx: &Transaction<'_>,
    event: &JobEvent,
) -> Result<(), JobEngineError> {
    if !event.state.is_terminal() {
        return Ok(());
    }
    let body = serde_json::to_string(event)?;
    tx.execute("INSERT INTO job_callback_deliveries(delivery_id,subscription_id,target,event_sequence,event_json,state,attempt_count,available_at) SELECT 'delivery_' || lower(hex(randomblob(16))),subscription_id,target,?1,?2,'pending',0,?3 FROM job_callback_subscriptions WHERE job_id=?4 ON CONFLICT(subscription_id,event_sequence) DO NOTHING",params![event.event_sequence as i64,body,event.created_at,event.job_id.as_str()])?;
    Ok(())
}

pub(crate) fn migrate(tx: &Transaction<'_>) -> anyhow::Result<()> {
    tx.execute_batch("CREATE TABLE job_callback_subscriptions(subscription_id TEXT PRIMARY KEY,job_id TEXT NOT NULL REFERENCES jobs(job_id),target TEXT NOT NULL CHECK(length(target) BETWEEN 1 AND 128),UNIQUE(job_id,target));
        CREATE TABLE job_callback_deliveries(sequence INTEGER PRIMARY KEY AUTOINCREMENT,delivery_id TEXT NOT NULL UNIQUE,subscription_id TEXT NOT NULL REFERENCES job_callback_subscriptions(subscription_id),target TEXT NOT NULL,event_sequence INTEGER NOT NULL REFERENCES job_events(event_sequence),event_json TEXT NOT NULL,state TEXT NOT NULL CHECK(state IN ('pending','leased','delivered')),attempt_count INTEGER NOT NULL DEFAULT 0,available_at TEXT NOT NULL,lease_token TEXT,lease_expires_at TEXT,UNIQUE(subscription_id,event_sequence));
        CREATE INDEX job_callback_pending ON job_callback_deliveries(target,state,available_at,sequence);")?;
    Ok(())
}
