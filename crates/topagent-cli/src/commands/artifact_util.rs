use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

pub(crate) fn resolve_unique_artifact_path(
    dir: &Path,
    id: &str,
    extension: &str,
) -> Result<PathBuf> {
    let candidates = list_files(dir, extension)?;
    let needle = Path::new(id)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(id);
    let matches = candidates
        .into_iter()
        .filter(|path| {
            let filename = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default();
            let stem = path
                .file_stem()
                .and_then(|name| name.to_str())
                .unwrap_or_default();
            filename == needle
                || stem == needle
                || filename.starts_with(needle)
                || stem.starts_with(needle)
        })
        .collect::<Vec<_>>();

    match matches.as_slice() {
        [] => Err(anyhow::anyhow!("no artifact matched `{}`", id)),
        [path] => Ok(path.clone()),
        many => Err(anyhow::anyhow!(
            "artifact id `{}` is ambiguous: {}",
            id,
            many.iter()
                .map(|path| path.file_name().unwrap().to_string_lossy().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

pub(crate) fn list_files(dir: &Path, extension: &str) -> Result<Vec<PathBuf>> {
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut files = fs::read_dir(dir)
        .with_context(|| format!("failed to read {}", dir.display()))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some(extension))
        .collect::<Vec<_>>();
    files.sort();
    Ok(files)
}
