use super::*;

impl Session<'_> {
    pub(super) fn observe(
        &mut self,
        ids: &Identities,
        operation: &langspan_core::ToolIdentity,
        keys: Vec<String>,
        result: ToolResult,
        files: Vec<langspan_core::ObservedFile>,
    ) -> Reply {
        if let Err(failure) = self.attach(ids) {
            return failure_reply(failure);
        }
        // The admission asked for after a wait, when the pre-tool run waited.
        let lease_id =
            crate::wait::admission_of(&self.config.state_dir, &ids.operation_id, &ids.admit);
        let observed = self.call(
            post_request_id(self.source, ids, result),
            Command::ObserveTool {
                session_id: ids.session_id.clone(),
                operation: operation.clone(),
                paths: keys,
                result,
                lease_id: Some(lease_id.clone()),
            },
        );
        let response = match observed {
            Ok(response) => response,
            Err(failure) => return failure_reply(failure),
        };
        let Outcome::Observed { observation } = &response.outcome else {
            return Reply::none("unknown", "unexpected observation outcome");
        };
        if result == ToolResult::Succeeded {
            // What the session just wrote is the version it now knows. A
            // failure to record it only leaves that file's base unknown.
            let files = files
                .into_iter()
                .filter(|file| observation.paths.contains(&file.path))
                .collect();
            let _ = self.record_source(ids, files, langspan_core::SourceOrigin::Write);
        }
        let relation = format!("{:?}", observation.lease_relation).to_lowercase();
        let coverage = match observation.lease_relation {
            langspan_core::LeaseRelation::Expired | langspan_core::LeaseRelation::Invalidated => {
                "late_observation"
            }
            langspan_core::LeaseRelation::NotAdmitted => "observed_unadmitted",
            langspan_core::LeaseRelation::ScopeMismatch
            | langspan_core::LeaseRelation::InputMismatch => "observed_mismatch",
            _ => "observed",
        };
        if response.lease_valid != Some(true)
            || observation.lease_relation != langspan_core::LeaseRelation::Current
        {
            return Reply::none(coverage, format!("observation; admission {relation}"));
        }
        match self.call(
            ids.complete.clone(),
            Command::Complete {
                session_id: ids.session_id.clone(),
                lease_id,
            },
        ) {
            Ok(_) => Reply::none(coverage, "observation; admission completed"),
            Err(failure) => {
                let mut reply = failure_reply(failure);
                reply.coverage = coverage;
                reply.detail = format!("observation recorded; completion: {}", reply.detail);
                reply
            }
        }
    }

    pub(super) fn record_source(
        &self,
        ids: &Identities,
        files: Vec<langspan_core::ObservedFile>,
        origin: langspan_core::SourceOrigin,
    ) -> Result<usize, Failure> {
        if files.is_empty() {
            return Ok(0);
        }
        let count = files.len();
        let kind = match origin {
            langspan_core::SourceOrigin::Read => "source-read",
            langspan_core::SourceOrigin::Write => "source-write",
            langspan_core::SourceOrigin::Refused => "source-refused",
            langspan_core::SourceOrigin::Snapshot => "source-snapshot",
        };
        self.call(
            wire_id(kind, &[self.source, &ids.session_id, &ids.operation_id]),
            Command::ObserveSource {
                session_id: ids.session_id.clone(),
                files,
                origin,
            },
        )?;
        Ok(count)
    }

    pub(super) fn read(&self, ids: &Identities, files: Vec<langspan_core::ObservedFile>) -> Reply {
        if let Err(failure) = self.attach(ids) {
            return failure_reply(failure);
        }
        match self.record_source(ids, files, langspan_core::SourceOrigin::Read) {
            Ok(0) => Reply::none(
                "unsupported",
                "file version could not be read within bounds",
            ),
            Ok(_) => Reply::none("read_recorded", "read version recorded"),
            Err(failure) => failure_reply(failure),
        }
    }
}

pub(super) fn observed_files(root: &Path, keys: &[String]) -> Vec<langspan_core::ObservedFile> {
    located_files(root, keys, &[], Instant::now(), None).0
}
