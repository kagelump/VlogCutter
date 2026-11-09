//! `Catalog` methods for recording, applying, and reading typed project operations.

use super::*;

impl Catalog {
    /// Tombstones project membership without touching the asset identity,
    /// original locations, or operation history. This is deliberately a
    /// synced operation instead of a SQL delete: peers must not replay an old
    /// add operation and silently bring a rolled-back import back into view.
    pub fn remove_project_asset_membership(
        &self,
        project_id: &str,
        asset_ids: &[String],
    ) -> Result<()> {
        let operations = asset_ids
            .iter()
            .map(|asset_id| {
                Ok(NewOperation {
                    aggregate_type: "projectAsset",
                    aggregate_id: asset_id.clone(),
                    operation_type: PROJECT_ASSET_MEMBERSHIP_SET,
                    base_revision: None,
                    payload: serde_json::to_value(MembershipPayload { present: false })?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        self.commit_local(project_id, operations)
    }

    pub(crate) fn restore_project_asset_membership(
        &self,
        project_id: &str,
        asset_id: &str,
    ) -> Result<()> {
        self.commit_local(
            project_id,
            vec![NewOperation {
                aggregate_type: "projectAsset",
                aggregate_id: asset_id.to_string(),
                operation_type: PROJECT_ASSET_MEMBERSHIP_SET,
                base_revision: None,
                payload: serde_json::to_value(MembershipPayload { present: true })?,
            }],
        )
    }
    /// Seeds the stable local node identity supplied by an installation adapter.
    /// A catalog that has already acknowledged operations cannot be reassigned.
    pub fn set_local_node_id(&self, node_id: &str) -> Result<()> {
        validate_id("node ID", node_id)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = transaction
            .query_row(
                "SELECT node_id FROM catalog_identity WHERE singleton_id = 1",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        match existing {
            Some(existing) if existing != node_id => {
                let operations: i64 = transaction.query_row(
                    "SELECT COUNT(*) FROM project_operations",
                    [],
                    |row| row.get(0),
                )?;
                if operations != 0 {
                    bail!("catalog node identity is already in use: {existing}");
                }
                transaction.execute(
                    "UPDATE catalog_identity SET node_id = ?1 WHERE singleton_id = 1",
                    [node_id],
                )?;
                transaction.execute(
                    "UPDATE projects SET origin_node_id=?1 WHERE origin_node_id=?2",
                    params![node_id, existing],
                )?;
                transaction.execute(
                    "DELETE FROM project_asset_local_locations WHERE node_id=?1",
                    [existing],
                )?;
            }
            Some(_) => {}
            None => {
                transaction.execute(
                    "INSERT INTO catalog_identity(singleton_id, node_id, created_at) VALUES (1, ?1, ?2)",
                    params![node_id, now()],
                )?;
            }
        }
        backfill_local_locations(&transaction, node_id)?;
        transaction.commit()?;
        Ok(())
    }

    pub fn local_node_id(&self) -> Result<Option<String>> {
        Ok(self
            .connection()?
            .query_row(
                "SELECT node_id FROM catalog_identity WHERE singleton_id = 1",
                [],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn create_note_revision(&self, asset_id: &str, text: &str) -> Result<Asset> {
        let current = self.get_asset(asset_id)?;
        if current.notes == text {
            return Ok(current);
        }
        let operation = NewOperation {
            aggregate_type: "projectAsset",
            aggregate_id: asset_id.to_string(),
            operation_type: NOTE_REVISION_CREATED,
            base_revision: None,
            payload: serde_json::to_value(NotePayload {
                revision_id: format!("note_{}", Uuid::new_v4().simple()),
                text: text.to_string(),
            })?,
        };
        self.commit_local(&current.project_id, vec![operation])?;
        self.get_asset(asset_id)
    }

    pub fn add_label(&self, asset_id: &str, label: &str) -> Result<Asset> {
        let current = self.get_asset(asset_id)?;
        let normalized = normalize_labels(&[label.to_string()])?;
        let Some(label) = normalized.into_iter().next() else {
            return Ok(current);
        };
        if current
            .labels
            .iter()
            .any(|value| value.eq_ignore_ascii_case(&label))
        {
            return Ok(current);
        }
        if current.labels.len() >= 24 {
            bail!("invalid labels: an asset can have at most 24 labels");
        }
        let operation = label_add_operation(asset_id, label)?;
        self.commit_local(&current.project_id, vec![operation])?;
        self.get_asset(asset_id)
    }

    pub fn remove_label(&self, asset_id: &str, label: &str) -> Result<Asset> {
        let current = self.get_asset(asset_id)?;
        let Some(display) = current
            .labels
            .iter()
            .find(|value| value.eq_ignore_ascii_case(label.trim()))
        else {
            return Ok(current);
        };
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        reconcile_asset_baseline(&transaction, &current.project_id, asset_id)?;
        let tags = active_label_tags(&transaction, asset_id, &label_key(display))?;
        if tags.is_empty() {
            transaction.commit()?;
            return self.get_asset(asset_id);
        }
        let operation = NewOperation {
            aggregate_type: "projectAsset",
            aggregate_id: asset_id.to_string(),
            operation_type: LABEL_REMOVED,
            base_revision: None,
            payload: serde_json::to_value(LabelRemovePayload {
                normalized_label: label_key(display),
                observed_add_tags: tags,
            })?,
        };
        commit_local_in_transaction(&transaction, &current.project_id, vec![operation])?;
        transaction.commit()?;
        self.get_asset(asset_id)
    }

    pub fn set_removed(&self, asset_id: &str, removed: bool) -> Result<Asset> {
        let current = self.get_asset(asset_id)?;
        if current.removed == removed {
            return Ok(current);
        }
        self.commit_local(
            &current.project_id,
            vec![NewOperation {
                aggregate_type: "projectAsset",
                aggregate_id: asset_id.to_string(),
                operation_type: REMOVED_SET,
                base_revision: None,
                payload: serde_json::to_value(RemovedPayload { removed })?,
            }],
        )?;
        self.get_asset(asset_id)
    }

    pub fn set_rotation(&self, asset_id: &str, rotation_degrees: u16) -> Result<Asset> {
        if !matches!(rotation_degrees, 0 | 90 | 180 | 270) {
            bail!("invalid rotation: {rotation_degrees}; expected 0, 90, 180, or 270 degrees");
        }
        let current = self.get_asset(asset_id)?;
        if current.rotation_degrees == rotation_degrees {
            return Ok(current);
        }
        self.commit_local(
            &current.project_id,
            vec![NewOperation {
                aggregate_type: "projectAsset",
                aggregate_id: asset_id.to_string(),
                operation_type: ROTATION_SET,
                base_revision: None,
                payload: serde_json::to_value(RotationPayload { rotation_degrees })?,
            }],
        )?;
        self.get_asset(asset_id)
    }

    /// Persists an explicit human orientation decision through the same
    /// revisioned operation stream as other asset judgments.
    pub fn decide_orientation(
        &self,
        asset_id: &str,
        expected_revision: u64,
        decision: OrientationDecision,
    ) -> Result<Asset> {
        self.decide_orientation_batch(&[(asset_id.to_string(), expected_revision, decision)])?
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("orientation decision returned no asset"))
    }

    /// Confirms a batch atomically. Every revision is checked under the same
    /// IMMEDIATE transaction before any operation is appended.
    pub fn decide_orientation_batch(
        &self,
        decisions: &[(String, u64, OrientationDecision)],
    ) -> Result<Vec<Asset>> {
        use std::collections::BTreeSet;
        ensure!(!decisions.is_empty(), "orientation decision batch is empty");
        ensure!(
            decisions.len() <= 200,
            "orientation decision batch exceeds 200 assets"
        );
        let mut ids = BTreeSet::new();
        let mut prepared = Vec::with_capacity(decisions.len());
        let mut project_id = None;
        for (asset_id, expected_revision, decision) in decisions {
            ensure!(
                ids.insert(asset_id),
                "duplicate asset in orientation decision batch: {asset_id}"
            );
            validate_orientation_decision(&decision.status, decision.rotation_degrees)?;
            let asset = self.get_asset(asset_id)?;
            if let Some(expected_project) = project_id.as_deref() {
                ensure!(
                    asset.project_id == expected_project,
                    "orientation decision batch spans projects"
                );
            } else {
                project_id = Some(asset.project_id.clone());
            }
            prepared.push((asset_id, *expected_revision, decision));
        }
        let project_id = project_id.expect("nonempty decisions");
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        for (asset_id, expected, _) in &prepared {
            let actual = current_revision(&transaction, &project_id, "projectAsset", asset_id)?;
            ensure!(
                actual == *expected,
                "stale asset revision for {asset_id}: expected {expected}, found {actual}"
            );
        }
        let operations = prepared
            .into_iter()
            .map(|(asset_id, expected_revision, decision)| {
                Ok(NewOperation {
                    aggregate_type: "projectAsset",
                    aggregate_id: asset_id.clone(),
                    operation_type: ORIENTATION_DECISION_SET,
                    base_revision: Some(expected_revision),
                    payload: serde_json::to_value(OrientationDecisionPayload {
                        status: decision.status.clone(),
                        rotation_degrees: decision.rotation_degrees,
                    })?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        commit_local_in_transaction(&transaction, &project_id, operations)?;
        transaction.commit()?;
        decisions
            .iter()
            .map(|(asset_id, _, _)| self.get_asset(asset_id))
            .collect()
    }

    /// Compatibility adapter for the desktop's full replacement patch.
    pub(crate) fn update_asset_with_operations(
        &self,
        asset_id: &str,
        patch: &AssetPatch,
    ) -> Result<Asset> {
        if !matches!(patch.rotation_degrees, 0 | 90 | 180 | 270) {
            bail!(
                "invalid rotation: {}; expected 0, 90, 180, or 270 degrees",
                patch.rotation_degrees
            );
        }
        let labels = normalize_labels(&patch.labels)?;
        let current = self.get_asset(asset_id)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        reconcile_asset_baseline(&transaction, &current.project_id, asset_id)?;
        // A whole-patch write carries every judgment field, so a patch built
        // from a stale read does not merely fail to include another writer's
        // change — it actively reverses it, as a removal of the label or a
        // restoration of the old note. Refusing is the only safe answer; the
        // caller re-reads and decides what it still meant.
        if let Some(expected) = patch.expected_revision {
            let actual =
                current_revision(&transaction, &current.project_id, "projectAsset", asset_id)?;
            if actual != expected {
                bail!(
                    "{STALE_ASSET_REVISION}: expected revision {expected}, catalog is at {actual}"
                );
            }
        }
        let mut operations = Vec::new();
        if current.notes != patch.notes {
            operations.push(NewOperation {
                aggregate_type: "projectAsset",
                aggregate_id: asset_id.to_string(),
                operation_type: NOTE_REVISION_CREATED,
                base_revision: None,
                payload: serde_json::to_value(NotePayload {
                    revision_id: format!("note_{}", Uuid::new_v4().simple()),
                    text: patch.notes.clone(),
                })?,
            });
        }
        for label in &current.labels {
            if !labels
                .iter()
                .any(|wanted| wanted.eq_ignore_ascii_case(label))
            {
                let normalized_label = label_key(label);
                operations.push(NewOperation {
                    aggregate_type: "projectAsset",
                    aggregate_id: asset_id.to_string(),
                    operation_type: LABEL_REMOVED,
                    base_revision: None,
                    payload: serde_json::to_value(LabelRemovePayload {
                        observed_add_tags: active_label_tags(
                            &transaction,
                            asset_id,
                            &normalized_label,
                        )?,
                        normalized_label,
                    })?,
                });
            }
        }
        for label in labels {
            if !current
                .labels
                .iter()
                .any(|existing| existing.eq_ignore_ascii_case(&label))
            {
                operations.push(label_add_operation(asset_id, label)?);
            }
        }
        if current.removed != patch.removed {
            operations.push(NewOperation {
                aggregate_type: "projectAsset",
                aggregate_id: asset_id.to_string(),
                operation_type: REMOVED_SET,
                base_revision: None,
                payload: serde_json::to_value(RemovedPayload {
                    removed: patch.removed,
                })?,
            });
        }
        if current.rotation_degrees != patch.rotation_degrees {
            operations.push(NewOperation {
                aggregate_type: "projectAsset",
                aggregate_id: asset_id.to_string(),
                operation_type: ROTATION_SET,
                base_revision: None,
                payload: serde_json::to_value(RotationPayload {
                    rotation_degrees: patch.rotation_degrees,
                })?,
            });
        }
        if !operations.is_empty() {
            commit_local_in_transaction(&transaction, &current.project_id, operations)?;
        }
        transaction.commit()?;
        self.get_asset(asset_id)
    }

    pub(crate) fn add_project_asset(
        &self,
        project_id: &str,
        local_path: &str,
        payload: ProjectAssetAddedPayload,
    ) -> Result<()> {
        let asset_id = payload.id.clone();
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        commit_local_in_transaction(
            &transaction,
            project_id,
            vec![NewOperation {
                aggregate_type: "projectAsset",
                aggregate_id: asset_id.clone(),
                operation_type: PROJECT_ASSET_ADDED,
                base_revision: Some(0),
                payload: serde_json::to_value(payload)?,
            }],
        )?;
        let node_id = ensure_node(&transaction)?;
        transaction.execute(
            "INSERT INTO project_asset_local_locations(asset_id, node_id, local_path, created_at) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(asset_id, node_id, local_path) DO NOTHING",
            params![asset_id, node_id, local_path, now()],
        )?;
        transaction.execute(
            "UPDATE assets SET path=?2 WHERE id=?1",
            params![asset_id, local_path],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn add_portable_project_asset(
        &self,
        project_id: &str,
        payload: ProjectAssetAddedPayload,
    ) -> Result<Asset> {
        let asset_id = payload.id.clone();
        self.commit_local(
            project_id,
            vec![NewOperation {
                aggregate_type: "projectAsset",
                aggregate_id: asset_id.clone(),
                operation_type: PROJECT_ASSET_ADDED,
                base_revision: Some(0),
                payload: serde_json::to_value(payload)?,
            }],
        )?;
        self.get_asset(&asset_id)
    }

    pub fn link_project_asset_media(
        &self,
        asset_id: &str,
        media_object_id: &str,
        verification_state: MediaVerificationState,
    ) -> Result<Asset> {
        let current = self.get_asset(asset_id)?;
        if current.media_object_id.as_deref() == Some(media_object_id)
            && current.media_verification_state == verification_state
        {
            return Ok(current);
        }
        self.commit_local(
            &current.project_id,
            vec![NewOperation {
                aggregate_type: "projectAsset",
                aggregate_id: asset_id.to_string(),
                operation_type: PROJECT_ASSET_MEDIA_LINKED,
                base_revision: None,
                payload: serde_json::to_value(MediaLinkedPayload {
                    media_object_id: media_object_id.to_string(),
                    verification_state,
                })?,
            }],
        )?;
        self.get_asset(asset_id)
    }

    pub(crate) fn create_clock_event_operation(
        &self,
        project_id: &str,
        camera: &str,
        start_filename: &str,
        offset_seconds: i64,
        label: &str,
    ) -> Result<ClockEvent> {
        self.require_project(project_id)?;
        if camera.trim().is_empty() || start_filename.trim().is_empty() {
            bail!("camera and start filename cannot be empty");
        }
        let event = ClockEvent {
            id: format!("clock_{}", Uuid::new_v4().simple()),
            project_id: project_id.to_string(),
            camera: camera.to_string(),
            start_filename: start_filename.to_string(),
            offset_seconds,
            label: label.trim().to_string(),
            created_at: now(),
        };
        let revision = current_revision(&self.connection()?, project_id, "clock", camera)?;
        self.commit_local(
            project_id,
            vec![NewOperation {
                aggregate_type: "clock",
                aggregate_id: camera.to_string(),
                operation_type: CLOCK_EVENT_CREATED,
                base_revision: Some(revision),
                payload: serde_json::to_value(ClockPayload {
                    event_id: event.id.clone(),
                    camera: event.camera.clone(),
                    start_filename: event.start_filename.clone(),
                    offset_seconds,
                    label: event.label.clone(),
                })?,
            }],
        )?;
        Ok(event)
    }

    pub fn apply_operation(&self, operation: &ProjectOperation) -> Result<ApplyOperationResult> {
        validate_operation(operation)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if operation_exists(&transaction, &operation.operation_id)? {
            return Ok(ApplyOperationResult::Duplicate);
        }
        let result = insert_and_materialize(&transaction, operation)?;
        receive_hlc(&transaction, operation)?;
        transaction.commit()?;
        // A quarantined operation is retained and reported by
        // `quarantined_operations()`, but nothing would tell the user that
        // surface is non-empty. Record it after the commit so a diagnostic
        // failure can never roll back a successfully applied operation
        // (D01-T027).
        if result == ApplyOperationResult::Quarantined {
            let _ = self.record_quarantined_operation(
                &operation.project_id,
                &operation.operation_id,
                &operation.operation_type,
            );
        }
        Ok(result)
    }

    /// Applies one origin's contiguous operation page and advances its durable
    /// acknowledgement cursor in the same SQLite transaction.
    pub fn apply_operation_batch(
        &self,
        origin_node_id: &str,
        operations: &[ProjectOperation],
    ) -> Result<OperationBatchResult> {
        validate_id("origin node ID", origin_node_id)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut cursor = transaction
            .query_row(
                "SELECT committed_sequence FROM sync_origin_cursors WHERE origin_node_id=?1",
                [origin_node_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .unwrap_or(0) as u64;
        let mut quarantined: Vec<(String, String, String)> = Vec::new();
        let mut chronology_projects = BTreeSet::new();
        let mut result = OperationBatchResult {
            origin_node_id: origin_node_id.to_string(),
            committed_sequence: cursor,
            applied: 0,
            duplicates: 0,
            conflicts: 0,
            quarantined: 0,
        };
        for operation in operations {
            validate_operation(operation)?;
            if operation.origin_node_id != origin_node_id {
                bail!("operation origin does not match batch origin");
            }
            if operation_affects_chronology_projection(&operation.operation_type) {
                chronology_projects.insert(operation.project_id.clone());
            }
            if operation.origin_sequence <= cursor {
                if !operation_matches_acknowledged_sequence(&transaction, operation)? {
                    bail!(
                        "operation sequence {} is already acknowledged with a different operation",
                        operation.origin_sequence
                    );
                }
                result.duplicates += 1;
                continue;
            }
            let expected = cursor + 1;
            if operation.origin_sequence != expected {
                bail!(
                    "operation sequence gap for {origin_node_id}: expected {expected}, received {}",
                    operation.origin_sequence
                );
            }
            match insert_and_materialize(&transaction, operation)? {
                ApplyOperationResult::Applied => result.applied += 1,
                ApplyOperationResult::Duplicate => result.duplicates += 1,
                ApplyOperationResult::Conflict => result.conflicts += 1,
                ApplyOperationResult::Quarantined => {
                    result.quarantined += 1;
                    quarantined.push((
                        operation.project_id.clone(),
                        operation.operation_id.clone(),
                        operation.operation_type.clone(),
                    ));
                }
            }
            receive_hlc(&transaction, operation)?;
            cursor = operation.origin_sequence;
        }
        // Facts, their derived asset projection, and the durable origin cursor
        // advance together. Otherwise a projection failure after commit could
        // acknowledge the operation permanently while leaving queue reads on
        // the old best_time/order. Including duplicate chronology operations
        // also makes an idempotent retry repair a projection left by an older
        // build that did not yet own this invariant.
        for project_id in chronology_projects {
            Self::project_current_chronology_in_transaction(&transaction, &project_id)?;
        }
        transaction.execute(
            "INSERT INTO sync_origin_cursors(origin_node_id, committed_sequence, updated_at) VALUES (?1, ?2, ?3) ON CONFLICT(origin_node_id) DO UPDATE SET committed_sequence=excluded.committed_sequence, updated_at=excluded.updated_at",
            params![origin_node_id, cursor as i64, now()],
        )?;
        result.committed_sequence = cursor;
        transaction.commit()?;
        // Recorded after the commit, so a diagnostic failure can never roll
        // back a successfully applied batch (D01-T027).
        for (project_id, operation_id, operation_type) in quarantined {
            let _ = self.record_quarantined_operation(&project_id, &operation_id, &operation_type);
        }
        Ok(result)
    }

    pub fn operation_cursors(&self) -> Result<std::collections::BTreeMap<String, u64>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            r#"
            SELECT origin_node_id, MAX(sequence) FROM (
                SELECT origin_node_id, committed_sequence AS sequence FROM sync_origin_cursors
                UNION ALL
                SELECT origin_node_id, origin_sequence AS sequence FROM project_operations
            ) GROUP BY origin_node_id ORDER BY origin_node_id
            "#,
        )?;
        Ok(statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as u64))
            })?
            .collect::<rusqlite::Result<_>>()?)
    }

    pub fn peer_operation_acknowledgements(
        &self,
        peer_node_id: &str,
    ) -> Result<std::collections::BTreeMap<String, u64>> {
        validate_id("peer node ID", peer_node_id)?;
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT origin_node_id, committed_sequence FROM sync_peer_operation_acknowledgements WHERE peer_node_id=?1 ORDER BY origin_node_id",
        )?;
        Ok(statement
            .query_map([peer_node_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as u64))
            })?
            .collect::<rusqlite::Result<_>>()?)
    }

    pub fn record_peer_operation_acknowledgements(
        &self,
        peer_node_id: &str,
        cursors: &std::collections::BTreeMap<String, u64>,
    ) -> Result<()> {
        validate_id("peer node ID", peer_node_id)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let timestamp = now();
        for (origin_node_id, sequence) in cursors {
            validate_id("origin node ID", origin_node_id)?;
            let sequence = i64::try_from(*sequence)?;
            transaction.execute(
                "INSERT INTO sync_peer_operation_acknowledgements(peer_node_id,origin_node_id,committed_sequence,updated_at) VALUES(?1,?2,?3,?4) ON CONFLICT(peer_node_id,origin_node_id) DO UPDATE SET committed_sequence=MAX(committed_sequence,excluded.committed_sequence),updated_at=excluded.updated_at",
                params![peer_node_id, origin_node_id, sequence, timestamp],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn mark_sequence_recovery(
        &self,
        peer_origin_node_id: &str,
        server_origin_node_id: &str,
        manifest_watermark: u64,
    ) -> Result<SequenceRecoveryProjection> {
        validate_id("peer origin node ID", peer_origin_node_id)?;
        validate_id("server origin node ID", server_origin_node_id)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let contained = transaction.query_row(
            "SELECT COALESCE(MAX(origin_sequence), 0) FROM project_operations WHERE origin_node_id=?1",
            [server_origin_node_id],
            |row| row.get::<_, i64>(0),
        )? as u64;
        let timestamp = now();
        transaction.execute(
            r#"
            INSERT INTO operation_sequence_recovery(
                peer_origin_node_id, server_origin_node_id, manifest_watermark,
                contained_watermark, state, created_at, updated_at
            ) VALUES(?1, ?2, ?3, ?4, 'awaiting', ?5, ?5)
            ON CONFLICT(peer_origin_node_id) DO UPDATE SET
                server_origin_node_id=excluded.server_origin_node_id,
                manifest_watermark=excluded.manifest_watermark,
                contained_watermark=excluded.contained_watermark,
                reported_watermark=NULL, state='awaiting', divergence_start=NULL,
                divergence_end=NULL, decision=NULL, updated_at=excluded.updated_at
            "#,
            params![
                peer_origin_node_id,
                server_origin_node_id,
                i64::try_from(manifest_watermark)?,
                i64::try_from(contained)?,
                timestamp,
            ],
        )?;
        let projection = load_sequence_recovery(&transaction, peer_origin_node_id)?;
        transaction.commit()?;
        Ok(projection)
    }

    pub fn reconcile_sequence_recovery(
        &self,
        peer_origin_node_id: &str,
        reported_server_sequence: u64,
    ) -> Result<Option<SequenceRecoveryProjection>> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(current) = load_sequence_recovery_optional(&transaction, peer_origin_node_id)?
        else {
            transaction.commit()?;
            return Ok(None);
        };
        if current.state == SequenceRecoveryState::Completed {
            transaction.commit()?;
            return Ok(Some(current));
        }
        let contained_watermark = transaction.query_row(
            "SELECT COALESCE(MAX(origin_sequence), 0) FROM project_operations WHERE origin_node_id=?1",
            [&current.server_origin_node_id],
            |row| row.get::<_, i64>(0),
        )? as u64;
        if contained_watermark != current.contained_watermark {
            transaction.execute(
                "UPDATE operation_sequence_recovery SET contained_watermark=?2 WHERE peer_origin_node_id=?1",
                params![peer_origin_node_id, i64::try_from(contained_watermark)?],
            )?;
        }
        let timestamp = now();
        if reported_server_sequence > contained_watermark {
            transaction.execute(
                "UPDATE operation_sequence_recovery SET reported_watermark=?2,state='divergent',divergence_start=?3,divergence_end=?2,updated_at=?4 WHERE peer_origin_node_id=?1",
                params![
                    peer_origin_node_id,
                    i64::try_from(reported_server_sequence)?,
                    i64::try_from(contained_watermark.saturating_add(1))?,
                    timestamp,
                ],
            )?;
        } else {
            let safe = current
                .manifest_watermark
                .max(contained_watermark)
                .max(reported_server_sequence);
            transaction.execute(
                "INSERT INTO operation_origin_sequences(origin_node_id,last_sequence) VALUES(?1,?2) ON CONFLICT(origin_node_id) DO UPDATE SET last_sequence=MAX(last_sequence,excluded.last_sequence)",
                params![current.server_origin_node_id, i64::try_from(safe)?],
            )?;
            transaction.execute(
                "UPDATE operation_sequence_recovery SET reported_watermark=?2,state='completed',divergence_start=NULL,divergence_end=NULL,updated_at=?3 WHERE peer_origin_node_id=?1",
                params![peer_origin_node_id, i64::try_from(reported_server_sequence)?, timestamp],
            )?;
        }
        let projection = load_sequence_recovery(&transaction, peer_origin_node_id)?;
        transaction.commit()?;
        Ok(Some(projection))
    }

    pub fn accept_sequence_divergence(
        &self,
        peer_origin_node_id: &str,
        decision: &str,
    ) -> Result<SequenceRecoveryProjection> {
        if decision != "accept_allocator_advance" {
            bail!("sequence recovery decision is not recognized");
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = load_sequence_recovery(&transaction, peer_origin_node_id)?;
        if current.state != SequenceRecoveryState::Divergent {
            bail!("sequence recovery does not have a divergence to accept");
        }
        let reported = current
            .reported_watermark
            .ok_or_else(|| anyhow!("divergence is missing its reported watermark"))?;
        transaction.execute(
            "INSERT INTO operation_origin_sequences(origin_node_id,last_sequence) VALUES(?1,?2) ON CONFLICT(origin_node_id) DO UPDATE SET last_sequence=MAX(last_sequence,excluded.last_sequence)",
            params![current.server_origin_node_id, i64::try_from(reported)?],
        )?;
        transaction.execute(
            "UPDATE operation_sequence_recovery SET state='completed',decision=?2,divergence_start=NULL,divergence_end=NULL,updated_at=?3 WHERE peer_origin_node_id=?1",
            params![peer_origin_node_id, decision, now()],
        )?;
        let projection = load_sequence_recovery(&transaction, peer_origin_node_id)?;
        transaction.commit()?;
        Ok(projection)
    }

    pub fn sequence_recovery_state(&self) -> Result<Vec<SequenceRecoveryProjection>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT peer_origin_node_id FROM operation_sequence_recovery ORDER BY peer_origin_node_id",
        )?;
        let peers = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        peers
            .into_iter()
            .map(|peer| load_sequence_recovery(&connection, &peer))
            .collect()
    }

    pub fn list_operations(&self, project_id: &str) -> Result<Vec<ProjectOperation>> {
        self.require_project(project_id)?;
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT operation_id, project_id, origin_node_id, origin_sequence, hlc_wall_millis, hlc_counter, aggregate_type, aggregate_id, operation_type, payload_version, base_revision, payload_json FROM project_operations WHERE project_id = ?1 ORDER BY hlc_wall_millis, hlc_counter, origin_node_id, operation_id",
        )?;
        Ok(statement
            .query_map([project_id], operation_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn operation_diagnostics(&self, project_id: &str) -> Result<OperationDiagnostics> {
        self.require_project(project_id)?;
        let connection = self.connection()?;
        if !crate::table_exists(&connection, "project_operations")? {
            return Ok(OperationDiagnostics {
                project_id: project_id.to_string(),
                operation_count: 0,
                latest_local_sequence: 0,
                unresolved_conflicts: Vec::new(),
            });
        }
        let operation_count = connection.query_row(
            "SELECT COUNT(*) FROM project_operations WHERE project_id = ?1",
            [project_id],
            |row| row.get::<_, i64>(0),
        )? as u64;
        let node = connection
            .query_row(
                "SELECT node_id FROM catalog_identity WHERE singleton_id = 1",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        let latest_local_sequence = match node {
            Some(node) => connection.query_row(
                "SELECT COALESCE(last_sequence, 0) FROM operation_origin_sequences WHERE origin_node_id = ?1",
                [node],
                |row| row.get::<_, i64>(0),
            ).optional()?.unwrap_or(0) as u64,
            None => 0,
        };
        let mut statement = connection.prepare(
            "SELECT conflict_id, project_id, aggregate_type, aggregate_id, incoming_operation_id, active_operation_id, expected_base_revision, actual_revision, conflict_type, created_at FROM operation_conflicts WHERE project_id = ?1 AND resolved_at IS NULL ORDER BY created_at, conflict_id",
        )?;
        let unresolved_conflicts = statement
            .query_map([project_id], |row| {
                Ok(ConflictProjection {
                    conflict_id: row.get(0)?,
                    project_id: row.get(1)?,
                    aggregate_type: row.get(2)?,
                    aggregate_id: row.get(3)?,
                    incoming_operation_id: row.get(4)?,
                    active_operation_id: row.get(5)?,
                    expected_base_revision: row.get::<_, Option<i64>>(6)?.map(|value| value as u64),
                    actual_revision: row.get::<_, i64>(7)? as u64,
                    conflict_type: row.get(8)?,
                    created_at: row.get(9)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(OperationDiagnostics {
            project_id: project_id.to_string(),
            operation_count,
            latest_local_sequence,
            unresolved_conflicts,
        })
    }

    pub fn asset_revision_diagnostics(&self, asset_id: &str) -> Result<AssetRevisionDiagnostics> {
        let asset = self.get_asset(asset_id)?;
        let connection = self.connection()?;
        if !crate::table_exists(&connection, "note_revisions")? {
            return Ok(AssetRevisionDiagnostics {
                asset_id: asset_id.to_string(),
                materialized_revision: 0,
                note_revisions: vec![NoteRevisionProjection {
                    revision_id: format!("baseline_note_{asset_id}"),
                    operation_id: None,
                    text: asset.notes,
                    logical_time: HybridLogicalTime {
                        wall_millis: 0,
                        counter: 0,
                        node_id: "baseline".to_string(),
                    },
                    active: true,
                    created_at: String::new(),
                }],
            });
        }
        let materialized_revision =
            current_revision(&connection, &asset.project_id, "projectAsset", asset_id)?;
        let active_id = connection.query_row(
            "SELECT revision_id FROM note_revisions WHERE asset_id=?1 ORDER BY hlc_wall_millis DESC, hlc_counter DESC, origin_node_id DESC, revision_id DESC LIMIT 1",
            [asset_id], |row| row.get::<_, String>(0),
        ).optional()?;
        let mut statement = connection.prepare(
            "SELECT revision_id, operation_id, text, hlc_wall_millis, hlc_counter, origin_node_id, created_at FROM note_revisions WHERE asset_id=?1 ORDER BY hlc_wall_millis DESC, hlc_counter DESC, origin_node_id DESC, revision_id DESC",
        )?;
        let note_revisions = statement
            .query_map([asset_id], |row| {
                let revision_id: String = row.get(0)?;
                let node_id: String = row.get(5)?;
                Ok(NoteRevisionProjection {
                    active: active_id.as_deref() == Some(&revision_id),
                    revision_id,
                    operation_id: row.get(1)?,
                    text: row.get(2)?,
                    logical_time: HybridLogicalTime {
                        wall_millis: row.get(3)?,
                        counter: row.get::<_, i64>(4)? as u64,
                        node_id,
                    },
                    created_at: row.get(6)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(AssetRevisionDiagnostics {
            asset_id: asset_id.to_string(),
            materialized_revision,
            note_revisions,
        })
    }

    pub fn count_project_assets(&self, project_id: &str) -> Result<u64> {
        let connection = self.connection()?;
        let count: i64 = connection.query_row(
            "SELECT COUNT(*) FROM assets WHERE project_id = ?1 AND COALESCE(membership_removed, 0) = 0",
            [project_id],
            |row| row.get(0),
        )?;
        Ok(count as u64)
    }

    /// Resolves project membership from the replica itself. Callers must use
    /// this instead of a control-catalog projection when deduplicating an
    /// add-to-project command because that projection can legitimately lag.
    pub fn project_asset_for_media_object(
        &self,
        project_id: &str,
        media_object_id: &str,
    ) -> Result<Option<Asset>> {
        let connection = self.connection()?;
        let asset_id = connection
            .query_row(
                "SELECT id FROM assets WHERE project_id = ?1 AND media_object_id = ?2 AND COALESCE(membership_removed, 0) = 0 ORDER BY id LIMIT 1",
                params![project_id, media_object_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        drop(connection);
        asset_id.map(|id| self.get_asset(&id)).transpose()
    }

    /// Pages one project's assets by the stable `project_asset_id` order.
    /// Search is a case-folded substring match over this project's own
    /// filename, camera, active note, labels, and analysis description. A replica is per-project and
    /// bounded, so a substring match is acceptable here in a way it is not for
    /// the global library.
    pub fn list_project_asset_page(
        &self,
        project_id: &str,
        limit: u32,
        after: Option<&str>,
        search: Option<&str>,
    ) -> Result<ProjectAssetPage> {
        let connection = self.connection()?;
        let folded = search.map(|value| format!("%{}%", value.to_lowercase()));
        let mut statement = connection.prepare(
            r#"
            SELECT id, filename, camera, media_object_id, media_verification_state,
                   removed, rotation_degrees, labels_json, notes, duration_seconds, best_time,
                   description
            FROM assets
            WHERE project_id = ?1
              AND (?2 IS NULL OR id > ?2)
              AND (?3 IS NULL
                   OR LOWER(filename) LIKE ?3
                   OR LOWER(camera) LIKE ?3
                   OR LOWER(notes) LIKE ?3
                   OR LOWER(labels_json) LIKE ?3
                   OR LOWER(description) LIKE ?3)
            ORDER BY id ASC
            LIMIT ?4
            "#,
        )?;
        let mut items = statement
            .query_map(
                rusqlite::params![project_id, after, folded, i64::from(limit + 1)],
                |row| {
                    let labels_json: String = row.get(7)?;
                    let notes: String = row.get(8)?;
                    Ok(ProjectAssetItem {
                        project_asset_id: row.get(0)?,
                        filename: row.get(1)?,
                        camera: row.get(2)?,
                        media_object_id: row.get(3)?,
                        media_verification_state: row.get(4)?,
                        removed: row.get::<_, i64>(5)? != 0,
                        rotation_degrees: row.get(6)?,
                        labels: serde_json::from_str(&labels_json).unwrap_or_default(),
                        active_note: (!notes.is_empty()).then_some(notes),
                        duration_seconds: row.get(9)?,
                        best_time: row.get(10)?,
                        description: row
                            .get::<_, Option<String>>(11)?
                            .filter(|text| !text.trim().is_empty()),
                    })
                },
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        let has_more = items.len() > limit as usize;
        items.truncate(limit as usize);
        let next_cursor = has_more.then(|| {
            items
                .last()
                .expect("a page with more rows has a last item")
                .project_asset_id
                .clone()
        });
        Ok(ProjectAssetPage { items, next_cursor })
    }

    pub(crate) fn commit_local(
        &self,
        project_id: &str,
        operations: Vec<NewOperation>,
    ) -> Result<()> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        commit_local_in_transaction(&transaction, project_id, operations)?;
        transaction.commit()?;
        Ok(())
    }
}
