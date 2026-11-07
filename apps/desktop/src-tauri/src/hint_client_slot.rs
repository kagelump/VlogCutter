//! Lifecycle of the WebSocket hint client.
//!
//! The hint client captures its endpoint and token when it starts and then
//! reconnects forever, so it can never notice that the user unpaired or paired
//! with a different server. This slot ties the running task to the settings it
//! was started with: whenever the desired settings differ from the running
//! ones (or vanish), the old task is aborted and, if settings remain, a fresh
//! one is started with the new credentials.
//!
//! WebSocket events are hints only; the durable sync state and polling stay
//! authoritative, so a stopped client degrades sync to the periodic tick and
//! nothing more.

use tokio::task::JoinHandle;

pub(crate) struct HintClientSlot<S> {
    running: Option<(S, JoinHandle<()>)>,
}

impl<S> Default for HintClientSlot<S> {
    fn default() -> Self {
        Self { running: None }
    }
}

impl<S: PartialEq> HintClientSlot<S> {
    /// Brings the running client in line with `desired`.
    ///
    /// - `None`: abort any running client.
    /// - the settings the live client was started with: leave it alone.
    /// - anything else (changed settings, or the client has finished): abort
    ///   the old task and call `start` with the new settings.
    pub(crate) fn sync(&mut self, desired: Option<S>, start: impl FnOnce(&S) -> JoinHandle<()>) {
        match desired {
            None => self.stop(),
            Some(settings) => {
                if let Some((running_with, handle)) = &self.running
                    && *running_with == settings
                    && !handle.is_finished()
                {
                    return;
                }
                self.stop();
                let handle = start(&settings);
                self.running = Some((settings, handle));
            }
        }
    }

    /// Aborts the running client, if any. Dropping a `JoinHandle` would only
    /// detach the task, so the abort is explicit.
    pub(crate) fn stop(&mut self) {
        if let Some((_, handle)) = self.running.take() {
            handle.abort();
        }
    }
}

impl<S> Drop for HintClientSlot<S> {
    fn drop(&mut self) {
        if let Some((_, handle)) = self.running.take() {
            handle.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    #[derive(PartialEq, Clone)]
    struct Creds {
        endpoint: &'static str,
        token: &'static str,
    }

    /// Sets a flag when the task is dropped, which happens when it is aborted.
    struct DropFlag(Arc<AtomicBool>);
    impl Drop for DropFlag {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    /// A stand-in client that loops forever like `ws_client_loop`, recording
    /// the credentials it was started with.
    fn fake_client(
        started: &Arc<Mutex<Vec<(&'static str, &'static str)>>>,
        creds: &Creds,
    ) -> (JoinHandle<()>, Arc<AtomicBool>) {
        started.lock().unwrap().push((creds.endpoint, creds.token));
        let dropped = Arc::new(AtomicBool::new(false));
        let guard = DropFlag(dropped.clone());
        let handle = tokio::spawn(async move {
            let _guard = guard;
            loop {
                tokio::time::sleep(Duration::from_secs(3600)).await;
            }
        });
        (handle, dropped)
    }

    async fn settle() {
        for _ in 0..5 {
            tokio::task::yield_now().await;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    const A: Creds = Creds {
        endpoint: "https://a.example",
        token: "token-a",
    };
    const B: Creds = Creds {
        endpoint: "https://b.example",
        token: "token-b",
    };

    #[tokio::test]
    async fn changed_credentials_abort_the_old_client_and_start_one_with_the_new() {
        let started = Arc::new(Mutex::new(Vec::new()));
        let mut flags = Vec::new();
        let mut slot = HintClientSlot::default();

        slot.sync(Some(A.clone()), |c| {
            let (h, f) = fake_client(&started, c);
            flags.push(f);
            h
        });
        settle().await;
        assert!(!flags[0].load(Ordering::SeqCst));

        slot.sync(Some(B.clone()), |c| {
            let (h, f) = fake_client(&started, c);
            flags.push(f);
            h
        });
        settle().await;

        assert!(
            flags[0].load(Ordering::SeqCst),
            "the old client was aborted, not detached"
        );
        assert!(!flags[1].load(Ordering::SeqCst));
        assert_eq!(
            *started.lock().unwrap(),
            vec![
                ("https://a.example", "token-a"),
                ("https://b.example", "token-b")
            ]
        );
    }

    #[tokio::test]
    async fn same_credentials_keep_the_running_client() {
        let started = Arc::new(Mutex::new(Vec::new()));
        let mut slot = HintClientSlot::default();
        for _ in 0..3 {
            slot.sync(Some(A.clone()), |c| fake_client(&started, c).0);
        }
        assert_eq!(started.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_rotated_token_for_the_same_endpoint_restarts_the_client() {
        let started = Arc::new(Mutex::new(Vec::new()));
        let mut slot = HintClientSlot::default();
        slot.sync(Some(A.clone()), |c| fake_client(&started, c).0);
        let rotated = Creds {
            endpoint: A.endpoint,
            token: "token-a2",
        };
        slot.sync(Some(rotated), |c| fake_client(&started, c).0);
        assert_eq!(started.lock().unwrap().len(), 2);
        assert_eq!(
            started.lock().unwrap()[1],
            ("https://a.example", "token-a2")
        );
    }

    #[tokio::test]
    async fn unpairing_aborts_the_client_and_starts_none() {
        let started = Arc::new(Mutex::new(Vec::new()));
        let mut dropped = None;
        let mut slot = HintClientSlot::default();
        slot.sync(Some(A.clone()), |c| {
            let (h, f) = fake_client(&started, c);
            dropped = Some(f);
            h
        });
        settle().await;
        slot.sync(None, |_| panic!("no client is started without settings"));
        settle().await;
        assert!(dropped.unwrap().load(Ordering::SeqCst));
        assert_eq!(started.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_finished_client_is_restarted_with_the_same_settings() {
        let mut slot = HintClientSlot::default();
        slot.sync(Some(A.clone()), |_| tokio::spawn(async {}));
        settle().await;
        let restarted = Arc::new(AtomicBool::new(false));
        let flag = restarted.clone();
        slot.sync(Some(A.clone()), |_| {
            flag.store(true, Ordering::SeqCst);
            tokio::spawn(async {})
        });
        assert!(restarted.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn dropping_the_slot_aborts_the_client() {
        let started = Arc::new(Mutex::new(Vec::new()));
        let mut dropped = None;
        let mut slot = HintClientSlot::default();
        slot.sync(Some(A.clone()), |c| {
            let (h, f) = fake_client(&started, c);
            dropped = Some(f);
            h
        });
        settle().await;
        drop(slot);
        settle().await;
        assert!(dropped.unwrap().load(Ordering::SeqCst));
    }
}
