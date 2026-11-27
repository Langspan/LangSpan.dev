use super::*;

/// How often a waiting hook asks whether the holder is gone.
const WAIT_POLL: Duration = Duration::from_millis(20);

/// A wait for a current holder that ended with the holder gone.
struct Waited {
    waited: Duration,
    /// The holder the wait saw last.
    holder: Option<String>,
}

enum WaitEnd {
    /// The policy is `refuse`: refused at once, as without a policy.
    NotWaited,
    /// The holder is gone and the files are as they were: ask again.
    Released(Waited),
    /// Refused, or no decision, after the wait.
    Decided(Reply),
}

impl Session<'_> {
    pub(super) fn pre(
        &mut self,
        ids: &Identities,
        operation: &langspan_core::ToolIdentity,
        keys: Vec<String>,
        (edits, contents): Writes,
        snapshot_unknown: bool,
        break_check: BreakCheck,
    ) -> Reply {
        // Placing an edit parses the file; it starts only while more than
        // half the budget remains, so the admission itself keeps its time.
        let (base, regions, overwrites) = located_files(
            self.root,
            &keys,
            edits,
            self.deadline
                .checked_sub(self.config.budget / 2)
                .unwrap_or(self.deadline),
            Some((&self.config.state_dir, &ids.session_id)),
        );
        let admit = |request_id: String| {
            self.call(
                request_id,
                Command::AdmitTool {
                    session_id: ids.session_id.clone(),
                    paths: keys.clone(),
                    ttl_ms: self.config.ttl_ms,
                    operation: operation.clone(),
                    base: base.clone(),
                    regions: regions.clone(),
                    overwrites: overwrites.clone(),
                },
            )
        };
        let attached = self.attach(ids);
        // A write that renames a name other worktrees' uncommitted files
        // use is held once, before it takes an admission (issue #213).
        let rename_note = match (&attached, break_check) {
            (Ok(()), BreakCheck::Now) => self.rename_hold(ids, &keys, edits, contents),
            _ => None,
        };
        if let Some(Ok(held)) = rename_note {
            return held;
        }
        let mut admitted = attached.and_then(|()| admit(ids.admit.clone()));
        // The same call delivered again after a wait that admitted it: its
        // first request replays the conflict that started the wait, so ask
        // again under the request the wait ended with (issue #244).
        if let Ok(Response {
            outcome: Outcome::Conflict { .. },
            replayed: true,
            ..
        }) = &admitted
        {
            let after =
                crate::wait::admission_of(&self.config.state_dir, &ids.operation_id, &ids.admit);
            if after != ids.admit {
                admitted = admit(after);
            }
        }
        // A current holder, in a workspace whose policy waits for one: wait
        // for its admission to end, then ask again under a second request
        // ID, with the base unchanged (ADR 0036).
        let mut waited: Option<Waited> = None;
        if let Ok(Response {
            outcome: Outcome::Conflict { holders },
            replayed: false,
            ..
        }) = &admitted
        {
            match self.wait_for_holders(ids, operation, &keys, &base, &regions, holders) {
                WaitEnd::NotWaited => {}
                WaitEnd::Decided(reply) => return reply,
                WaitEnd::Released(wait) => {
                    admitted = admit(crate::wait::admission_id(&ids.admit));
                    waited = Some(wait);
                }
            }
        }
        let mut reply = match admitted {
            Err(failure) => failure_reply(failure),
            Ok(response) => match response.outcome {
                Outcome::Admitted { lease } if response.lease_valid == Some(true) => {
                    let mut detail = if lease.base_unknown.is_empty() {
                        format!("admission {}; source version current", lease.lease_id)
                    } else {
                        // No read of these was recorded, so nothing was compared.
                        format!(
                            "admission {}; source version not compared for {} file(s)",
                            lease.lease_id,
                            lease.base_unknown.len()
                        )
                    };
                    if snapshot_unknown && !lease.base_unknown.is_empty() {
                        // The versions the admission was asked with, so the
                        // record is exactly what was on disk when it was
                        // decided. A replayed admission records them again
                        // under the same request ID, which changes nothing.
                        let seen: Vec<_> = base
                            .into_iter()
                            .filter(|file| lease.base_unknown.contains(&file.path))
                            .collect();
                        detail.push_str(&match self.record_source(
                            ids,
                            seen,
                            langspan_core::SourceOrigin::Snapshot,
                        ) {
                            Ok(0) => "; no version could be read to record".into(),
                            Ok(count) => format!(
                                "; {count} recorded as seen now, so later edits are compared"
                            ),
                            Err(failure) => {
                                format!("; not recorded as seen: {}", failure_reply(failure).detail)
                            }
                        });
                    }
                    // A write that will land: tell it, before its result,
                    // about calls a peer's signature change breaks.
                    let mut reply = Reply::none("admitted", detail);
                    let alert = match break_check {
                        BreakCheck::Off => None,
                        BreakCheck::Now => self.break_alert(ids, &keys, edits, contents),
                    };
                    if let Some(alert) = alert {
                        add_alert(&mut reply, "PreToolUse", alert);
                    }
                    if let Some(wait) = &waited {
                        // The post-tool run observes this admission.
                        if let Err(why) =
                            crate::wait::mark(&self.config.state_dir, &ids.operation_id)
                        {
                            reply
                                .detail
                                .push_str(&format!("; post-tool run not told of the wait: {why}"));
                        }
                        reply.detail.push_str(&format!(
                            "; after waiting {} ms for the holder",
                            wait.waited.as_millis()
                        ));
                    }
                    reply
                }
                Outcome::Admitted { lease } => Reply::none(
                    "unknown",
                    format!(
                        "admission {} is no longer current; no decision",
                        lease.lease_id
                    ),
                ),
                Outcome::Conflict { holders } if !response.replayed => match &waited {
                    // Another holder took the file between the wait's end and
                    // this admission.
                    Some(wait) => deny_after_wait(&holders, &regions, wait.waited),
                    None => deny(&holders, &regions),
                },
                // A stale or unread refusal is about this call's own input,
                // refused once: the same call delivered again (same request,
                // same input) gets the same refusal. A retry is a new call.
                Outcome::StaleBase { stale } => deny_stale(&stale, &ids.session_id, |path| {
                    crate::shell::change_note(&self.config.state_dir, self.root, path)
                }),
                Outcome::UnreadLines { lines } => deny_unread(&lines),
                Outcome::Conflict { .. } => Reply::none(
                    "unknown",
                    "historical conflict; retry as a new tool attempt",
                ),
                other => Reply::none("unknown", format!("unexpected outcome {other:?}")),
            },
        };
        if let Some(wait) = waited {
            let result = match reply.coverage {
                "admitted" => Some(langspan_core::WaitResult::Admitted),
                "denied" => Some(langspan_core::WaitResult::RefusedConflict),
                "denied_stale" => Some(langspan_core::WaitResult::RefusedStale),
                "denied_unread" => Some(langspan_core::WaitResult::RefusedUnread),
                // No decision: nothing to record.
                _ => None,
            };
            if let Some(result) = result {
                self.record_wait(ids, operation, &keys, &wait, result, Vec::new());
            }
        }
        if let Some(Err(note)) = rename_note {
            reply.detail.push_str(&format!("; {note}"));
        }
        reply
    }

    /// Wait for the holders of `keys` to end their admissions, within the
    /// workspace's wait and half of what is left of this run's budget, then
    /// compare each file's version on disk with `base`. Nothing waits where
    /// the policy is `refuse` (the default).
    fn wait_for_holders(
        &self,
        ids: &Identities,
        operation: &langspan_core::ToolIdentity,
        keys: &[String],
        base: &[langspan_core::ObservedFile],
        regions: &[langspan_core::PathRegions],
        holders: &[Conflict],
    ) -> WaitEnd {
        let started = Instant::now();
        let wait_ms = match self.call(
            "read".into(),
            Command::Policy {
                workspace_id: self.config.workspace_id.clone(),
            },
        ) {
            Ok(Response {
                outcome: Outcome::Policy { rows, .. },
                ..
            }) => rows
                .iter()
                .find(|row| row.case == langspan_core::PolicyCase::Conflict)
                .map_or(0, |row| row.wait_ms),
            // Refused at once, as without a policy.
            _ => 0,
        };
        if wait_ms == 0 {
            return WaitEnd::NotWaited;
        }
        let left = self.deadline.saturating_duration_since(started);
        let until = started + Duration::from_millis(u64::from(wait_ms)).min(left / 2);
        let mut holders = holders.to_vec();
        let mut holder = holders.first().map(|holder| holder.session_id.clone());
        while !holders.is_empty() {
            let now = Instant::now();
            if now >= until {
                break;
            }
            std::thread::sleep(WAIT_POLL.min(until - now));
            match self.call(
                "read".into(),
                Command::Holders {
                    session_id: ids.session_id.clone(),
                    paths: keys.to_vec(),
                    base: base.to_vec(),
                    regions: regions.to_vec(),
                },
            ) {
                Ok(Response {
                    outcome: Outcome::Holders { holders: now },
                    ..
                }) => {
                    holders = now;
                    if let Some(first) = holders.first() {
                        holder = Some(first.session_id.clone());
                    }
                }
                Ok(other) => {
                    return WaitEnd::Decided(Reply::none(
                        "unknown",
                        format!("unexpected outcome {:?} while waiting", other.outcome),
                    ));
                }
                Err(failure) => {
                    // As for coordination unavailable at any other time: no
                    // decision.
                    let mut reply = failure_reply(failure);
                    reply.detail = format!(
                        "{} after waiting {} ms for the holder",
                        reply.detail,
                        started.elapsed().as_millis()
                    );
                    eprintln!(
                        "{}",
                        json!({"event":"wait_unavailable", "coverage":reply.coverage, "waited_ms":started.elapsed().as_millis() as u64})
                    );
                    return WaitEnd::Decided(reply);
                }
            }
        }
        let wait = Waited {
            waited: started.elapsed(),
            holder,
        };
        if !holders.is_empty() {
            let reply = deny_after_wait(&holders, regions, wait.waited);
            self.record_wait(
                ids,
                operation,
                keys,
                &wait,
                langspan_core::WaitResult::RefusedConflict,
                Vec::new(),
            );
            return WaitEnd::Decided(reply);
        }
        // The holder is gone. An edit planned against a version that is no
        // longer on disk is refused, naming what changed.
        let changed: Vec<langspan_core::StaleFile> = base
            .iter()
            .filter_map(|file| {
                let now = file_state(self.root, &file.path)?;
                (now != file.state).then(|| langspan_core::StaleFile {
                    path: file.path.clone(),
                    read: file.state.clone(),
                    current: now,
                    changed_by: wait.holder.clone(),
                })
            })
            .collect();
        let wait = Waited {
            holder: wait.holder,
            waited: started.elapsed(),
        };
        if changed.is_empty() {
            return WaitEnd::Released(wait);
        }
        let mut reply = deny_stale_with(
            &changed,
            &ids.session_id,
            |path| crate::shell::change_note(&self.config.state_dir, self.root, path),
            &format!(
                "These files changed while this edit waited {} ms for the session holding them.",
                wait.waited.as_millis()
            ),
        );
        reply.detail = "file changed while this edit waited for its holder".into();
        let now = changed
            .into_iter()
            .map(|file| langspan_core::ObservedFile {
                path: file.path,
                state: file.current,
            })
            .collect();
        self.record_wait(
            ids,
            operation,
            keys,
            &wait,
            langspan_core::WaitResult::RefusedStale,
            now,
        );
        WaitEnd::Decided(reply)
    }

    /// Record how a wait ended. A failure leaves the wait unrecorded and is
    /// logged; the decision stands.
    fn record_wait(
        &self,
        ids: &Identities,
        operation: &langspan_core::ToolIdentity,
        keys: &[String],
        wait: &Waited,
        result: langspan_core::WaitResult,
        changed: Vec<langspan_core::ObservedFile>,
    ) {
        if let Err(failure) = self.call(
            crate::wait::record_id(&ids.admit),
            Command::RecordWait {
                session_id: ids.session_id.clone(),
                paths: keys.to_vec(),
                holder_session_id: wait.holder.clone(),
                waited_ms: wait.waited.as_millis() as u64,
                result,
                changed,
                input_digest: Some(operation.input_digest.clone()),
            },
        ) {
            let diagnostic = failure_reply(failure);
            eprintln!(
                "{}",
                json!({"event":"wait_unrecorded", "coverage":diagnostic.coverage, "detail":diagnostic.detail})
            );
        }
    }

    /// The break check of a write about to land (ADR 0029): the calls it
    /// writes, in files of a supported language, checked by the daemon
    /// against interface notices from other live sessions. `None` when the
    /// write has no call to check. Otherwise the alert, if any, and a note
    /// for the hook's diagnostic. It starts only while a quarter of the
    /// budget remains, and any failure is a note, never a decision.
    pub(super) fn break_alert(
        &self,
        ids: &Identities,
        keys: &[String],
        edits: &[(String, crate::regions::Edit)],
        contents: &[(String, String)],
    ) -> Option<(Option<String>, String)> {
        match self.written_calls(keys, edits, contents) {
            Err(note) => Some((None, note)),
            Ok(calls) if calls.is_empty() => None,
            Ok(calls) => Some(self.checked_calls(ids, calls)),
        }
    }

    /// The calls a write puts in its files of a supported language, at
    /// most `MAX_CALL_SITES`. `Err` is a note: too little of the budget left.
    fn written_calls(
        &self,
        keys: &[String],
        edits: &[(String, crate::regions::Edit)],
        contents: &[(String, String)],
    ) -> Result<Vec<langspan_core::CallSite>, String> {
        let start_by = self.break_start_by();
        let mut calls = Vec::new();
        for key in keys.iter().take(BREAK_FILES) {
            if langspan_graph::Language::for_path(key).is_none() {
                continue;
            }
            let Some((content, lines)) = self.written_text(key, edits, contents) else {
                continue;
            };
            if Instant::now() >= start_by {
                return Err(BREAK_LATE.into());
            }
            let room = langspan_core::MAX_CALL_SITES - calls.len();
            calls.extend(crate::breaks::call_sites(
                key,
                &content,
                lines.as_ref(),
                room,
            ));
            if calls.len() >= langspan_core::MAX_CALL_SITES {
                break;
            }
        }
        Ok(calls)
    }

    /// The whole text a write leaves in `key`, and for an edit of an
    /// existing file the lines it writes. `None` when it cannot be built.
    pub(super) fn written_text(
        &self,
        key: &str,
        edits: &[(String, crate::regions::Edit)],
        contents: &[(String, String)],
    ) -> Option<(String, Option<std::collections::BTreeSet<u32>>)> {
        match contents.iter().find(|(path, _)| path == key) {
            Some((_, content)) => Some((content.clone(), None)),
            None => edits
                .iter()
                .find(|(path, _)| path == key)
                .and_then(|(_, edit)| {
                    if let crate::regions::Edit::Write(content) = edit {
                        return Some((content.clone(), None));
                    }
                    let bytes = file_content(self.root, key)??;
                    let text = std::str::from_utf8(&bytes).ok()?;
                    let (content, lines) = match edit {
                        crate::regions::Edit::Replace(list) => crate::breaks::replaced(text, list)?,
                        crate::regions::Edit::Hunks(hunks) => crate::breaks::patched(text, hunks)?,
                        crate::regions::Edit::Write(_) => return None,
                    };
                    Some((content, Some(lines)))
                }),
        }
    }

    /// The break check starts only while a quarter of the budget remains.
    fn break_start_by(&self) -> Instant {
        self.deadline
            .checked_sub(self.config.budget / 4)
            .unwrap_or(self.deadline)
    }

    /// The daemon's check of `calls` against other live sessions' interface
    /// notices: the alert, if any, and a note for the diagnostic.
    fn checked_calls(
        &self,
        ids: &Identities,
        calls: Vec<langspan_core::CallSite>,
    ) -> (Option<String>, String) {
        if Instant::now() >= self.break_start_by() {
            return (None, BREAK_LATE.into());
        }
        let checked = self.call(
            wire_id("break-check", &[self.source, &ids.operation_id]),
            Command::CheckCalls {
                session_id: ids.session_id.clone(),
                calls,
                renamed: Vec::new(),
            },
        );
        match checked.map(|response| response.outcome) {
            Ok(Outcome::CallBreaks { ready: false, .. }) => (
                None,
                "break check skipped: the source index is not ready".into(),
            ),
            Ok(Outcome::CallBreaks { breaks, .. }) if breaks.is_empty() => {
                (None, "break check: no call breaks".into())
            }
            Ok(Outcome::CallBreaks { breaks, .. }) => match crate::breaks::render(&breaks) {
                Some(text) => (
                    Some(text),
                    format!("break alert for {} call(s)", breaks.len()),
                ),
                None => (None, "break alert did not fit".into()),
            },
            Ok(other) => (None, format!("break check: unexpected outcome {other:?}")),
            Err(failure) => (
                None,
                format!("break check unavailable: {}", failure_reply(failure).detail),
            ),
        }
    }
}

/// Files of one write checked for call breaks, at most.
pub(super) const BREAK_FILES: usize = 8;
const BREAK_LATE: &str = "break check skipped: too little of the hook budget left";

/// Add a break check's note to the diagnostic and its alert, if any, to the
/// model's context of the reply for `event`, after any context the reply
/// already holds. Never a decision.
pub(super) fn add_alert(reply: &mut Reply, event: &str, (context, note): (Option<String>, String)) {
    reply.detail.push_str("; ");
    reply.detail.push_str(&note);
    let Some(context) = context else {
        return;
    };
    let held = serde_json::from_str::<Value>(&reply.stdout)
        .ok()
        .and_then(|value| {
            value["hookSpecificOutput"]["additionalContext"]
                .as_str()
                .map(str::to_owned)
        });
    let context = match held {
        Some(held) => format!("{held}\n\n{context}"),
        None => context,
    };
    reply.stdout = json!({
        "hookSpecificOutput": {
            "hookEventName": event,
            "additionalContext": context,
        }
    })
    .to_string();
}

type Located = (
    Vec<langspan_core::ObservedFile>,
    Vec<langspan_core::PathRegions>,
    Vec<langspan_core::WrittenLines>,
);

/// Each file's version on disk; for a file with a stated edit, where the
/// edit falls in that same version; and, given the state directory and this
/// session's ID, the lines the edit would change that other sessions wrote
/// recently ([ADR 0028](../docs/adr/0028-line-ownership.md)), from the line
/// journal's entries for these files only (ADR 0026). Each file
/// is read once, so all three describe the same bytes. For a file that
/// cannot be placed there are no regions (it is held whole) and the edit may
/// change any line. From `locate_until` on there are no regions and no
/// written lines: with too little time left the lines go unchecked, and a
/// check that cannot finish never refuses.
pub(crate) fn located_files(
    root: &Path,
    keys: &[String],
    edits: &[(String, crate::regions::Edit)],
    locate_until: Instant,
    journal: Option<(&Path, &str)>,
) -> Located {
    let journal = journal.filter(|_| Instant::now() < locate_until);
    let journal = journal.map(|(state_dir, session_id)| {
        (
            crate::journal::entries_for(state_dir, root, keys),
            session_id,
        )
    });
    let mut base = Vec::new();
    let mut regions = Vec::new();
    let mut overwrites: Vec<langspan_core::WrittenLines> = Vec::new();
    let now_ms = crate::shell::now_ms();
    for key in keys {
        let Some(content) = file_content(root, key) else {
            continue;
        };
        base.push(langspan_core::ObservedFile {
            path: key.clone(),
            state: version_of(content.as_deref()),
        });
        let in_time = Instant::now() < locate_until;
        let edit = edits
            .iter()
            .find(|(path, _)| path == key)
            .map(|(_, edit)| edit)
            .filter(|_| in_time);
        if let (Some(bytes), Some(edit)) = (&content, edit)
            && let Some(located) = crate::regions::locate(key, bytes, edit)
        {
            regions.push(located);
        }
        if let (Some(bytes), Some((entries, session_id)), true) = (&content, &journal, in_time) {
            let changed = edit
                .and_then(|edit| crate::lines::changes(Some(bytes), edit))
                .map(|changes| crate::lines::changed_ranges(&changes));
            overwrites.extend(crate::lines::overwritten(
                entries,
                session_id,
                key,
                bytes,
                changed.as_deref(),
                now_ms,
            ));
        }
    }
    overwrites.truncate(langspan_core::MAX_WRITTEN);
    (base, regions, overwrites)
}

#[cfg(test)]
mod journal_lookup {
    use super::*;

    #[test]
    fn a_writes_admission_reads_no_journal_entry_of_another_path() {
        let base = std::env::temp_dir().join(format!("ls-hook-journal-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let (state, root) = (base.join("state"), base.join("root"));
        fs::create_dir_all(&root).unwrap();
        crate::local::private_directory(&state).unwrap();
        let others: Vec<String> = (0..20).map(|i| format!("m{i}.rs")).collect();
        for path in &others {
            fs::write(root.join(path), "x\n").unwrap();
        }
        for op in 0..10 {
            crate::journal::record(
                &state,
                &root,
                "peer",
                &others,
                crate::journal::Origin::Edit {
                    operation_id: &format!("op-{op}"),
                },
            )
            .unwrap();
        }
        assert_eq!(crate::journal::entries(&state, &root).len(), 200);
        crate::journal::entry_reads();
        fs::write(root.join("fresh.rs"), "y\n").unwrap();
        let keys = ["fresh.rs".to_owned()];
        let (base_files, _, overwrites) =
            located_files(&root, &keys, &[], Instant::now(), Some((&state, "me")));
        assert_eq!(base_files.len(), 1);
        assert!(overwrites.is_empty());
        assert_eq!(crate::journal::entry_reads(), 0);
        // A peer's file: its ten entries, no more.
        let later = Instant::now() + Duration::from_secs(60);
        located_files(&root, &others[..1], &[], later, Some((&state, "me")));
        assert_eq!(crate::journal::entry_reads(), 10);
        let _ = fs::remove_dir_all(&base);
    }

    /// Less than half the hook budget left: the lines are not looked up, so
    /// the admission is decided without them and never refused for them.
    #[test]
    fn past_the_locating_deadline_no_written_lines_are_sent() {
        let base = std::env::temp_dir().join(format!("ls-hook-late-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let (state, root) = (base.join("state"), base.join("root"));
        fs::create_dir_all(&root).unwrap();
        crate::local::private_directory(&state).unwrap();
        let keys = ["a.txt".to_owned()];
        fs::write(root.join("a.txt"), "old line\n").unwrap();
        crate::journal::before_edit(&state, "op-1", &root, &keys);
        fs::write(root.join("a.txt"), "peer's fresh line\n").unwrap();
        crate::journal::record(
            &state,
            &root,
            "peer",
            &keys,
            crate::journal::Origin::Edit {
                operation_id: "op-1",
            },
        )
        .unwrap();
        let later = Instant::now() + Duration::from_secs(60);
        let (_, _, overwrites) = located_files(&root, &keys, &[], later, Some((&state, "me")));
        assert_eq!(overwrites.len(), 1, "in time, the peer's line is found");
        let (files, _, overwrites) =
            located_files(&root, &keys, &[], Instant::now(), Some((&state, "me")));
        assert_eq!(files.len(), 1, "the version is still sent");
        assert!(overwrites.is_empty(), "{overwrites:?}");
        let _ = fs::remove_dir_all(&base);
    }
}
