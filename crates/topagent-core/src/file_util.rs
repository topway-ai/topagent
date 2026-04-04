use crate::{Error, Result};
use std::path::Path;
use std::process::Output;

pub fn is_likely_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(8192).any(|&b| b == 0)
}

pub fn read_text_file_with_limit(path: &Path, max_bytes: usize) -> Result<String> {
    let bytes = std::fs::read(path)
        .map_err(|e| Error::ToolFailed(format!("failed to read {}: {}", path.display(), e)))?;

    if is_likely_binary(&bytes) {
        return Err(Error::ReadFailed(format!(
            "binary/non-text file not supported by read tool: {}",
            path.display()
        )));
    }

    let original_size = bytes.len();

    if original_size > max_bytes {
        let truncated = truncate_to_utf8_boundary(&bytes, max_bytes);
        return Ok(format!(
            "[ReadTool] File truncated: {} bytes total, showing first {} bytes:\n{}\n\n[ReadTool] File continues... ({} bytes truncated)",
            original_size,
            truncated.len(),
            String::from_utf8_lossy(&truncated),
            original_size - truncated.len()
        ));
    }

    String::from_utf8(bytes).map_err(|_| {
        Error::ReadFailed(format!(
            "file is valid UTF-8 text but read failed: {}",
            path.display()
        ))
    })
}

pub fn read_text_file_for_edit(path: &Path, max_bytes: usize) -> Result<String> {
    let bytes = std::fs::read(path)
        .map_err(|e| Error::ToolFailed(format!("failed to read {}: {}", path.display(), e)))?;

    let file_size = bytes.len();

    if file_size > max_bytes {
        return Err(Error::EditFailed(format!(
            "file too large to edit safely: {} ({} bytes) exceeds limit of {} bytes. Consider splitting the file or increasing the edit size limit.",
            path.display(),
            file_size,
            max_bytes
        )));
    }

    if is_likely_binary(&bytes) {
        return Err(Error::EditFailed(format!(
            "binary/non-text file cannot be edited: {}",
            path.display()
        )));
    }

    String::from_utf8(bytes).map_err(|_| {
        Error::EditFailed(format!(
            "file is valid UTF-8 text but read failed: {}",
            path.display()
        ))
    })
}

fn truncate_to_utf8_boundary(bytes: &[u8], max_size: usize) -> Vec<u8> {
    if bytes.len() <= max_size {
        return bytes.to_vec();
    }

    let truncated = &bytes[..max_size];

    if let Some(valid_len) = find_last_valid_utf8_boundary(truncated) {
        return bytes[..valid_len].to_vec();
    }

    truncated.to_vec()
}

fn find_last_valid_utf8_boundary(bytes: &[u8]) -> Option<usize> {
    for i in (1..=4).take(bytes.len()) {
        let check_len = bytes.len() - i;
        if std::str::from_utf8(&bytes[..check_len]).is_ok() {
            return Some(check_len);
        }
    }
    None
}

pub fn atomic_write(path: &Path, content: &str) -> Result<()> {
    let parent = path.parent();
    if let Some(p) = parent {
        std::fs::create_dir_all(p).map_err(|e| {
            Error::ToolFailed(format!(
                "failed to create parent dir for {}: {}",
                path.display(),
                e
            ))
        })?;
    }

    let temp_path = path.with_extension(
        path.extension()
            .map(|e| format!("{}.tmp", e.to_string_lossy()))
            .unwrap_or_else(|| "tmp".to_string()),
    );

    std::fs::write(&temp_path, content)
        .map_err(|e| Error::ToolFailed(format!("failed to write temp file: {}", e)))?;

    std::fs::rename(&temp_path, path).map_err(|e| {
        let _ = std::fs::remove_file(&temp_path);
        Error::ToolFailed(format!(
            "failed to rename temp file to {}: {}",
            path.display(),
            e
        ))
    })
}

pub fn format_command_output_with_limit(output: Output, max_size: usize) -> String {
    let stdout_raw = &output.stdout;
    let stderr_raw = &output.stderr;
    let status = output.status;

    let stdout_len = stdout_raw.len();
    let stderr_len = stderr_raw.len();

    let mut stdout_truncated = false;
    let mut stderr_truncated = false;

    let stdout_bytes = if stdout_len > max_size {
        stdout_truncated = true;
        &stdout_raw[..max_size]
    } else {
        stdout_raw.as_slice()
    };

    let stderr_bytes = if stderr_len > max_size {
        stderr_truncated = true;
        &stderr_raw[..max_size]
    } else {
        stderr_raw.as_slice()
    };

    let stdout = String::from_utf8_lossy(stdout_bytes);
    let stderr = String::from_utf8_lossy(stderr_bytes);

    let mut result = String::new();
    if !stdout_raw.is_empty() {
        result.push_str("Output: ");
        result.push_str(&stdout);
        if stdout_truncated {
            result.push_str(&format!(
                "\n[Output truncated: {} bytes total, showing first {}]",
                stdout_len, max_size
            ));
        }
        result.push('\n');
    }
    if !stderr_raw.is_empty() {
        result.push_str("Stderr: ");
        result.push_str(&stderr);
        if stderr_truncated {
            result.push_str(&format!(
                "\n[Stderr truncated: {} bytes total, showing first {}]",
                stderr_len, max_size
            ));
        }
        result.push('\n');
    }
    result.push_str(&format!("Exit code: {}", status.code().unwrap_or(-1)));
    result
}
