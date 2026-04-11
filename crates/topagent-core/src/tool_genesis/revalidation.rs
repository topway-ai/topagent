use super::{script_sha256_for_path, GeneratedToolRuntimeGuard};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GeneratedToolRevalidationOutcome {
    Usable,
    NeedsRepair { reason: String },
    Invalid { reason: String },
}

pub(crate) fn revalidate_runtime_tool(
    guard: &GeneratedToolRuntimeGuard,
) -> GeneratedToolRevalidationOutcome {
    super::note_revalidation_scan();

    if !guard.manifest_path.exists() {
        return GeneratedToolRevalidationOutcome::Invalid {
            reason: "manifest.json is missing; recreate the tool before using it again".to_string(),
        };
    }
    if !guard.script_path.exists() {
        return GeneratedToolRevalidationOutcome::Invalid {
            reason: "missing script.sh".to_string(),
        };
    }

    match script_sha256_for_path(&guard.script_path) {
        Ok(current_hash) if current_hash == guard.expected_script_sha256 => {
            GeneratedToolRevalidationOutcome::Usable
        }
        Ok(_) => GeneratedToolRevalidationOutcome::NeedsRepair {
            reason: "script.sh changed after approval; repair or recreate the tool before using it"
                .to_string(),
        },
        Err(err) => GeneratedToolRevalidationOutcome::Invalid {
            reason: format!("failed to hash script.sh: {}", err),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn make_guard(temp: &TempDir, contents: &str) -> GeneratedToolRuntimeGuard {
        let tool_dir = temp.path().join(".topagent/tools/example");
        std::fs::create_dir_all(&tool_dir).unwrap();
        let manifest_path = tool_dir.join("manifest.json");
        let script_path = tool_dir.join("script.sh");
        std::fs::write(&manifest_path, "{}").unwrap();
        std::fs::write(&script_path, contents).unwrap();

        GeneratedToolRuntimeGuard {
            tool_name: "example".to_string(),
            manifest_path,
            script_path,
            expected_script_sha256: crate::tool_genesis::script_sha256_hex(contents.as_bytes()),
        }
    }

    #[test]
    fn test_revalidate_runtime_tool_reports_hash_drift_as_needs_repair() {
        let temp = TempDir::new().unwrap();
        let guard = make_guard(&temp, "echo original");
        std::fs::write(
            temp.path().join(".topagent/tools/example/script.sh"),
            "echo changed",
        )
        .unwrap();

        let outcome = revalidate_runtime_tool(&guard);
        assert_eq!(
            outcome,
            GeneratedToolRevalidationOutcome::NeedsRepair {
                reason:
                    "script.sh changed after approval; repair or recreate the tool before using it"
                        .to_string(),
            }
        );
    }

    #[test]
    fn test_revalidate_runtime_tool_reports_missing_script_as_invalid() {
        let temp = TempDir::new().unwrap();
        let guard = make_guard(&temp, "echo original");
        std::fs::remove_file(PathBuf::from(&guard.script_path)).unwrap();

        let outcome = revalidate_runtime_tool(&guard);
        assert_eq!(
            outcome,
            GeneratedToolRevalidationOutcome::Invalid {
                reason: "missing script.sh".to_string(),
            }
        );
    }
}
