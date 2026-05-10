use std::path::{Path, PathBuf};

pub fn command_exists(name: &str) -> bool {
    find_command(name).is_some()
}

pub fn find_command(name: &str) -> Option<PathBuf> {
    if name.trim().is_empty() || name.contains(std::path::MAIN_SEPARATOR) {
        let path = PathBuf::from(name);
        return executable_file(&path).then_some(path);
    }

    let path_var = std::env::var_os("PATH")?;
    find_command_in_paths(name, std::env::split_paths(&path_var))
}

fn find_command_in_paths<I>(name: &str, paths: I) -> Option<PathBuf>
where
    I: IntoIterator<Item = PathBuf>,
{
    for dir in paths {
        let candidate = dir.join(name);
        if executable_file(&candidate) {
            return Some(candidate);
        }

        #[cfg(windows)]
        {
            for ext in windows_path_exts() {
                let candidate = dir.join(format!("{name}{ext}"));
                if executable_file(&candidate) {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

fn executable_file(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }

    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(windows)]
fn windows_path_exts() -> Vec<String> {
    std::env::var("PATHEXT")
        .unwrap_or_else(|_| ".EXE;.BAT;.CMD;.COM".to_string())
        .split(';')
        .filter(|ext| !ext.trim().is_empty())
        .map(|ext| ext.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_executable_in_supplied_path_without_shell() {
        let temp = tempfile::tempdir().unwrap();
        let command_path = temp.path().join("topagent-test-command");
        std::fs::write(&command_path, "#!/bin/sh\nexit 0\n").unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(&command_path).unwrap().permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&command_path, permissions).unwrap();
        }

        let found = find_command_in_paths("topagent-test-command", vec![temp.path().to_path_buf()]);
        assert_eq!(found, Some(command_path));
    }

    #[test]
    fn ignores_non_executable_files_on_unix() {
        let temp = tempfile::tempdir().unwrap();
        let command_path = temp.path().join("not-executable");
        std::fs::write(&command_path, "nope").unwrap();

        #[cfg(unix)]
        assert!(find_command_in_paths("not-executable", vec![temp.path().to_path_buf()]).is_none());
    }
}
