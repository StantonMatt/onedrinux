//! Reading the explicit owner-only token file. Never logged or printed.

use std::path::Path;

/// A token file as `konedrivectl dev export-access-token` writes it: a regular file, not a
/// symlink, mode `0600`, owned by the user running this, holding one token.
pub fn read_token(path: &Path) -> Result<String, String> {
    use std::os::unix::fs::MetadataExt;
    let shown = path.display();
    let meta = std::fs::symlink_metadata(path).map_err(|e| format!("{shown}: {e}"))?;
    if !meta.file_type().is_file() {
        return Err(format!("{shown} is not a regular file"));
    }
    if meta.mode() & 0o777 != 0o600 {
        return Err(format!("{shown} is mode {:03o}; a token file must be 0600", meta.mode() & 0o777));
    }
    // SAFETY: geteuid has no preconditions and cannot fail.
    if meta.uid() != unsafe { libc::geteuid() } {
        return Err(format!("{shown} is not the file of the user running this"));
    }
    let token = std::fs::read_to_string(path).map_err(|e| format!("{shown}: {e}"))?;
    let token = token.trim();
    if token.is_empty() || token.contains(char::is_whitespace) {
        return Err(format!("{shown} does not hold one token"));
    }
    Ok(token.to_owned())
}
