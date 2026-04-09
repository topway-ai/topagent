use crate::approval::ApprovalState;
use crate::behavior::{BehaviorContract, RunStateSnapshot};
use crate::context::ExecutionContext;
use crate::task_result::{TaskEvidence, TaskResult, VerificationCommand};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;

const MAX_ACTIVE_FILES: usize = 12;

pub(crate) struct AgentRunState {
    current_objective: Option<String>,
    changed_files: RefCell<Vec<String>>,
    active_files: RefCell<Vec<String>>,
    bash_history: RefCell<Vec<(String, String, i32)>>,
    run_baseline: RefCell<Option<RunBaseline>>,
}

struct RunBaseline {
    pre_existing_dirty: Vec<String>,
    pre_existing_hashes: HashMap<String, String>,
    pre_existing_unattributed: Vec<String>,
}

impl Default for AgentRunState {
    fn default() -> Self {
        Self {
            current_objective: None,
            changed_files: RefCell::new(Vec::new()),
            active_files: RefCell::new(Vec::new()),
            bash_history: RefCell::new(Vec::new()),
            run_baseline: RefCell::new(None),
        }
    }
}

impl AgentRunState {
    pub(crate) fn changed_files(&self) -> Vec<String> {
        self.changed_files.borrow().clone()
    }

    pub(crate) fn changed_file_count(&self) -> usize {
        self.changed_files.borrow().len()
    }

    pub(crate) fn reset(&mut self, workspace_root: &Path, instruction: &str) {
        self.current_objective = Some(instruction.to_string());
        self.changed_files.borrow_mut().clear();
        self.active_files.borrow_mut().clear();
        self.bash_history.borrow_mut().clear();
        self.capture_run_baseline(workspace_root);
    }

    pub(crate) fn track_changed_file(&self, tool_name: &str, args: &serde_json::Value) {
        if tool_name == "write" || tool_name == "edit" {
            if let Some(path) = args.get("path").and_then(|p| p.as_str()) {
                self.record_changed_file(path.to_string());
            }
        }
    }

    pub(crate) fn track_active_file(&self, tool_name: &str, args: &serde_json::Value) {
        let path = match tool_name {
            "read" | "write" | "edit" => args.get("path").and_then(|p| p.as_str()),
            _ => None,
        };
        let Some(path) = path else {
            return;
        };

        let mut active = self.active_files.borrow_mut();
        if let Some(existing) = active.iter().position(|entry| entry == path) {
            let entry = active.remove(existing);
            active.push(entry);
            return;
        }

        active.push(path.to_string());
        if active.len() > MAX_ACTIVE_FILES {
            let excess = active.len() - MAX_ACTIVE_FILES;
            active.drain(0..excess);
        }
    }

    pub(crate) fn record_bash_result(&self, command: String, output: String, exit_code: i32) {
        self.bash_history
            .borrow_mut()
            .push((command, output, exit_code));
    }

    pub(crate) fn track_inferred_changed_paths(&self, paths: &[String]) -> bool {
        let mut found_new_change = false;
        for path in paths {
            let normalized = path.trim();
            if normalized.is_empty() || normalized == "." || self.is_pre_existing_dirty(normalized)
            {
                continue;
            }
            let mut changed = self.changed_files.borrow_mut();
            if !changed.iter().any(|entry| entry == normalized) {
                changed.push(normalized.to_string());
                found_new_change = true;
            }
        }
        found_new_change
    }

    pub(crate) fn build_snapshot(
        &self,
        behavior: &BehaviorContract,
        ctx: &ExecutionContext,
        planning_required_now: bool,
    ) -> RunStateSnapshot {
        let mut blockers = Vec::new();
        if planning_required_now {
            blockers
                .push("Planning required before mutation-risk actions can continue.".to_string());
        }

        let mut pending_approvals = Vec::new();
        let mut recent_approval_decisions = Vec::new();
        if let Some(mailbox) = ctx.approval_mailbox() {
            for entry in mailbox.pending() {
                pending_approvals.push(entry.request.render_status_line(entry.state));
            }

            let mut resolved = mailbox
                .list()
                .into_iter()
                .filter(|entry| entry.state != ApprovalState::Pending)
                .collect::<Vec<_>>();
            resolved.sort_by_key(|entry| entry.resolved_at.or(Some(entry.request.created_at)));

            for entry in resolved
                .into_iter()
                .rev()
                .take(behavior.compaction.max_recent_approval_decisions)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
            {
                let mut line = entry.request.render_status_line(entry.state);
                if let Some(note) = entry.decision_note {
                    line.push_str(&format!(" ({note})"));
                }
                if matches!(
                    entry.state,
                    ApprovalState::Denied | ApprovalState::Expired | ApprovalState::Superseded
                ) {
                    blockers.push(format!(
                        "Approval {}: {}",
                        entry.state.label(),
                        entry.request.short_summary
                    ));
                }
                recent_approval_decisions.push(line);
            }
        }

        let mut active_files = self.active_files.borrow().clone();
        for changed in self.changed_files.borrow().iter() {
            if !active_files.contains(changed) {
                active_files.push(changed.clone());
            }
        }

        let changed_files = self.changed_files.borrow().clone();
        let mut proof_of_work_anchors = Vec::new();
        if !changed_files.is_empty() {
            proof_of_work_anchors.push(format!("changed files: {}", changed_files.join(", ")));
        }

        let mut verification_anchors = self
            .bash_history
            .borrow()
            .iter()
            .rev()
            .filter_map(|(command, _output, exit_code)| {
                behavior
                    .is_verification_command(command)
                    .then_some(format!("verification: {} (exit {})", command, exit_code))
            })
            .take(
                behavior
                    .compaction
                    .max_recent_proof_of_work_anchors
                    .saturating_sub(proof_of_work_anchors.len()),
            )
            .collect::<Vec<_>>();
        verification_anchors.reverse();
        let has_verification = !verification_anchors.is_empty();
        proof_of_work_anchors.extend(verification_anchors);

        if !changed_files.is_empty() && !has_verification {
            proof_of_work_anchors
                .push("Files were modified but no verification commands were run".to_string());
        }

        RunStateSnapshot {
            objective: self.current_objective.clone(),
            blockers,
            pending_approvals,
            recent_approval_decisions,
            active_files,
            proof_of_work_anchors,
            memory_context_loaded: ctx.memory_context().is_some(),
        }
    }

    pub(crate) fn build_task_result(
        &self,
        response: &str,
        workspace_root: &Path,
        behavior: &BehaviorContract,
        generated_tool_warnings: &[String],
    ) -> TaskResult {
        let files = self.changed_files.borrow().clone();
        let unattributed_files = self.unattributed_pre_existing_dirty_files(workspace_root);
        let baseline = self.run_baseline.borrow();
        let pre_existing = baseline
            .as_ref()
            .map_or(vec![], |b| b.pre_existing_dirty.clone());
        let labeled_files: Vec<String> = files
            .iter()
            .map(|f| {
                if pre_existing.contains(f) {
                    format!("{} (pre-existing dirty, changed again during this run)", f)
                } else {
                    f.clone()
                }
            })
            .collect();

        let diff_summary = if !files.is_empty() {
            Self::generate_diff_summary(workspace_root, &files)
        } else {
            String::new()
        };

        let mut evidence = TaskEvidence {
            files_changed: labeled_files,
            diff_summary,
            verification_commands_run: Vec::new(),
            unresolved_issues: Vec::new(),
            workspace_warnings: Vec::new(),
        };

        for (command, full_output, exit_code) in self.bash_history.borrow().iter() {
            if behavior.is_verification_command(command) {
                let succeeded = exit_code == &0;
                evidence
                    .verification_commands_run
                    .push(VerificationCommand {
                        command: command.clone(),
                        output: full_output.clone(),
                        exit_code: *exit_code,
                        succeeded,
                    });
            }
        }

        if !files.is_empty() && evidence.verification_commands_run.is_empty() {
            evidence
                .unresolved_issues
                .push("Files were modified but no verification commands were run".to_string());
        }

        if !unattributed_files.is_empty() {
            let details = unattributed_files
                .iter()
                .map(|file| {
                    format!(
                        "{} (pre-existing dirty file; baseline unavailable, run attribution uncertain)",
                        file
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            evidence
                .unresolved_issues
                .push(format!("Attribution uncertain: {}", details));
        }

        TaskResult::new(response.to_string())
            .with_files_changed(evidence.files_changed.clone())
            .with_diff_summary(evidence.diff_summary.clone())
            .with_verification_commands(evidence.verification_commands_run.clone())
            .with_unresolved_issues(evidence.unresolved_issues.clone())
            .with_workspace_warnings(generated_tool_warnings.to_vec())
    }

    pub(crate) fn reconcile_changed_files(&self, workspace_root: &Path) -> bool {
        let baseline = self.run_baseline.borrow();
        let pre_existing_dirty = baseline
            .as_ref()
            .map_or(vec![], |b| b.pre_existing_dirty.clone());
        let pre_existing_hashes = baseline
            .as_ref()
            .map_or(HashMap::new(), |b| b.pre_existing_hashes.clone());
        let current_dirty = Self::list_dirty_files(workspace_root);

        let mut changed = self.changed_files.borrow_mut();
        let mut found_new_change = false;

        for file in current_dirty {
            let was_pre_existing = pre_existing_dirty.contains(&file);

            if was_pre_existing {
                if let Some(baseline_hash) = pre_existing_hashes.get(&file) {
                    let current_hash = Self::compute_file_hash(&workspace_root.join(&file));
                    if current_hash.as_ref() != Some(baseline_hash) {
                        if !changed.contains(&file) {
                            changed.push(file.clone());
                        }
                        found_new_change = true;
                    }
                }
            } else {
                if !changed.contains(&file) {
                    changed.push(file.clone());
                }
                found_new_change = true;
            }
        }

        found_new_change
    }

    fn record_changed_file(&self, path: String) {
        if self.is_pre_existing_dirty(&path) {
            return;
        }
        let mut changed = self.changed_files.borrow_mut();
        if !changed.contains(&path) {
            changed.push(path);
        }
    }

    fn compute_file_hash(path: &Path) -> Option<String> {
        use std::collections::hash_map::DefaultHasher;
        use std::fs::File;
        use std::hash::{Hash, Hasher};
        use std::io::Read;

        let mut file = File::open(path).ok()?;
        let mut contents = Vec::new();
        file.read_to_end(&mut contents).ok()?;
        let mut hasher = DefaultHasher::new();
        contents.hash(&mut hasher);
        Some(format!("{:x}", hasher.finish()))
    }

    fn capture_run_baseline(&self, workspace_root: &Path) {
        let dirty = Self::list_dirty_files(workspace_root);
        let mut hashes = HashMap::new();
        let mut unattributed = Vec::new();

        for file in &dirty {
            let path = workspace_root.join(file);
            if let Some(hash) = Self::compute_file_hash(&path) {
                hashes.insert(file.clone(), hash);
            } else {
                unattributed.push(file.clone());
            }
        }

        *self.run_baseline.borrow_mut() = Some(RunBaseline {
            pre_existing_dirty: dirty,
            pre_existing_hashes: hashes,
            pre_existing_unattributed: unattributed,
        });
    }

    fn is_pre_existing_dirty(&self, path: &str) -> bool {
        let baseline = self.run_baseline.borrow();
        baseline
            .as_ref()
            .is_some_and(|b| b.pre_existing_dirty.iter().any(|file| file == path))
    }

    fn unattributed_pre_existing_dirty_files(&self, workspace_root: &Path) -> Vec<String> {
        let baseline = self.run_baseline.borrow();
        let Some(baseline) = baseline.as_ref() else {
            return Vec::new();
        };

        if baseline.pre_existing_unattributed.is_empty() {
            return Vec::new();
        }

        let current_dirty = Self::list_dirty_files(workspace_root);
        baseline
            .pre_existing_unattributed
            .iter()
            .filter(|file| current_dirty.contains(file))
            .cloned()
            .collect()
    }

    fn list_dirty_files(workspace_root: &Path) -> Vec<String> {
        let mut dirty = Vec::new();

        if let Ok(output) = std::process::Command::new("git")
            .args(["diff", "--name-only", "HEAD"])
            .current_dir(workspace_root)
            .output()
        {
            let stdout = String::from_utf8_lossy(&output.stdout);
            for line in stdout.lines() {
                let trimmed = line.trim();
                if !trimmed.is_empty() {
                    dirty.push(trimmed.to_string());
                }
            }
        }

        if let Ok(output) = std::process::Command::new("git")
            .args(["ls-files", "--others", "--exclude-standard"])
            .current_dir(workspace_root)
            .output()
        {
            let stdout = String::from_utf8_lossy(&output.stdout);
            for line in stdout.lines() {
                let trimmed = line.trim();
                if !trimmed.is_empty() && !dirty.contains(&trimmed.to_string()) {
                    dirty.push(trimmed.to_string());
                }
            }
        }

        dirty
    }

    fn generate_diff_summary(workspace_root: &Path, changed_files: &[String]) -> String {
        if changed_files.is_empty() {
            return String::new();
        }
        let mut summary_parts = Vec::new();
        for file in changed_files {
            let is_untracked = std::process::Command::new("git")
                .args(["ls-files", "--others", "--exclude-standard", file])
                .current_dir(workspace_root)
                .output()
                .map(|out| !String::from_utf8_lossy(&out.stdout).trim().is_empty())
                .unwrap_or(false);

            if is_untracked {
                if let Ok(content) = std::fs::read_to_string(workspace_root.join(file)) {
                    let line_count = content.lines().count();
                    summary_parts.push(format!("{}: {} lines added", file, line_count));
                } else {
                    summary_parts.push(format!("{}: (new file)", file));
                }
            } else {
                let output = std::process::Command::new("git")
                    .args(["diff", "--stat", file])
                    .current_dir(workspace_root)
                    .output();

                match output {
                    Ok(out) => {
                        let stdout = String::from_utf8_lossy(&out.stdout);
                        let stderr = String::from_utf8_lossy(&out.stderr);
                        if !stdout.trim().is_empty() {
                            summary_parts.push(stdout.to_string());
                        } else if !stderr.trim().is_empty() {
                            summary_parts.push(format!("{}: (no diff)", file));
                        } else {
                            summary_parts.push(format!("{}: (unchanged)", file));
                        }
                    }
                    Err(e) => {
                        summary_parts.push(format!("{}: (diff unavailable: {})", file, e));
                    }
                }
            }
        }
        summary_parts.join("\n")
    }
}
