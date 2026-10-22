//! Windows path handling and the output directory rules shared with Android.

use super::args::error;
use super::host::storage;
use crate::manager::ExtensionManager;
use std::path::{Path, PathBuf};

/// Grants the parent directories of already selected media files for one
/// operation only, as `withMediaFiles` does on Android. Blank paths are skipped.
pub(super) fn with_media_files<T>(
    current: &ExtensionManager,
    paths: &[&str],
    operation: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    let (sources, data) = storage()?;
    let mut aliases = Vec::new();
    for path in paths.iter().filter(|path| !path.trim().is_empty()) {
        if !Path::new(path).is_absolute() {
            return Err("Media paths must be absolute".into());
        }
        let absolute = std::path::absolute(path).map_err(error)?;
        let parent = absolute.parent().ok_or("Media path has no directory")?;
        push_unique(
            &mut aliases,
            directory_aliases(&parent.to_string_lossy(), &sources, &data)?,
        );
    }
    let scope = current
        .environment()
        .grant_download_directories(aliases)
        .map_err(error)?;
    let result = operation();
    scope.release();
    result
}

/// Canonical and absolute spellings of an output directory, rejecting a
/// filesystem root and any overlap with extension storage.
pub(super) fn directory_aliases(
    value: &str,
    sources: &str,
    data: &str,
) -> Result<Vec<String>, String> {
    if !Path::new(value).is_absolute() {
        return Err("Output directories must be absolute".into());
    }
    let path = canonical(value)?;
    let overlaps = |other: &str| within(&path, other) || within(other, &path);
    if Path::new(&path).parent().is_none() || overlaps(sources) || overlaps(data) {
        return Err("Output directory overlaps extension storage".into());
    }
    let absolute = plain(std::path::absolute(value).map_err(error)?);
    let mut aliases = vec![path];
    push_unique(&mut aliases, vec![absolute]);
    Ok(aliases)
}

/// Like Java's `File.canonicalPath`: resolves existing paths and falls back to
/// the normalized absolute path for ones that do not exist yet.
pub(super) fn canonical(path: &str) -> Result<String, String> {
    let path = Path::new(path);
    let resolved = match std::fs::canonicalize(path) {
        Ok(resolved) => resolved,
        Err(_) => std::path::absolute(path).map_err(error)?,
    };
    Ok(plain(resolved))
}

/// Drops the `\\?\` prefix `canonicalize` adds to drive paths, so the result
/// matches the paths Dart sends and compares against.
pub(super) fn plain(path: PathBuf) -> String {
    let text = path.to_string_lossy().into_owned();
    match text.strip_prefix(r"\\?\") {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => rest.to_owned(),
        _ => text,
    }
}

/// Whether `path` equals or is inside `parent`. Windows paths are
/// case-insensitive.
fn within(path: &str, parent: &str) -> bool {
    Path::new(&path.to_lowercase()).starts_with(parent.to_lowercase())
}

pub(super) fn push_unique(target: &mut Vec<String>, values: Vec<String>) {
    for value in values {
        if !target.contains(&value) {
            target.push(value);
        }
    }
}
