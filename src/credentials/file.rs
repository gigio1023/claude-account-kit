//! File backend for platforms without a Keychain.
//!
//! Claude Code stores its credential in `<config dir>/.credentials.json`, owner
//! readable only, and refuses to follow a symlink placed there. This backend
//! matches that: it never follows a symlink, never widens the permissions, and
//! replaces a file by renaming a freshly created one over it so a crash cannot
//! leave a half-written credential behind.

use crate::context::AppContext;
use crate::domain::AccountName;
use anyhow::{Context, Result, anyhow, bail};
use std::fs;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

pub(crate) fn describe(ctx: &AppContext) -> String {
    format!(
        "files, {} and {}",
        ctx.active_credential_path().display(),
        ctx.accounts_dir().display()
    )
}

/// Read the credential Claude Code currently uses.
///
/// The returned string is secret material. Callers should pass it directly to
/// another credential operation or to a short-lived in-memory comparison.
pub(crate) fn read_active(ctx: &AppContext) -> Result<String> {
    read_private(&ctx.active_credential_path()).with_context(|| {
        format!(
            "failed to read {}; log in with claude first",
            ctx.active_credential_path().display()
        )
    })
}

/// Replace the active Claude Code credential with a previously saved account.
pub(crate) fn write_active(ctx: &AppContext, credential: &str) -> Result<()> {
    ctx.ensure_claude_dir()?;
    write_private(&ctx.active_credential_path(), credential)
}

pub(crate) fn read_saved(ctx: &AppContext, name: &AccountName) -> Result<String> {
    let path = account_path(ctx, name);
    read_private(&path).with_context(|| format!("failed to read {}", path.display()))
}

pub(crate) fn write_saved(ctx: &AppContext, name: &AccountName, credential: &str) -> Result<()> {
    let dir = ctx.accounts_dir();
    fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;
    restrict_dir(&dir)?;
    write_private(&account_path(ctx, name), credential)
}

/// Remove a saved account credential, reporting whether one was there.
pub(crate) fn delete_saved(ctx: &AppContext, name: &AccountName) -> Result<bool> {
    let path = account_path(ctx, name);
    match fs::remove_file(&path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).with_context(|| format!("failed to remove {}", path.display())),
    }
}

fn account_path(ctx: &AppContext, name: &AccountName) -> PathBuf {
    ctx.accounts_dir().join(format!("{name}.json"))
}

fn read_private(path: &Path) -> Result<String> {
    refuse_symlink(path)?;
    Ok(fs::read_to_string(path)?.trim_end_matches('\n').to_string())
}

fn write_private(path: &Path, contents: &str) -> Result<()> {
    refuse_symlink(path)?;
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("path has no parent: {}", path.display()))?;
    let file_name = path
        .file_name()
        .ok_or_else(|| anyhow!("path has no file name: {}", path.display()))?;
    let temp = parent.join(format!("{}.tmp", file_name.to_string_lossy()));

    let mut file = create_private(&temp)
        .with_context(|| format!("failed to create {}", temp.display()))?;
    file.write_all(contents.as_bytes())
        .with_context(|| format!("failed to write {}", temp.display()))?;
    file.sync_all()
        .with_context(|| format!("failed to flush {}", temp.display()))?;
    drop(file);

    fs::rename(&temp, path).with_context(|| format!("failed to replace {}", path.display()))
}

/// Refuse to read or write through a symlink.
///
/// A credential path is a standing target for a symlink swap, and Claude Code
/// applies the same rule to its own file.
fn refuse_symlink(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            bail!("refusing to use {} because it is a symlink", path.display())
        }
        _ => Ok(()),
    }
}

#[cfg(unix)]
fn create_private(path: &Path) -> std::io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
}

/// Windows has no mode bits. The file inherits the user profile's access
/// control list, which is the same protection Claude Code's own file gets.
#[cfg(not(unix))]
fn create_private(path: &Path) -> std::io::Result<fs::File> {
    fs::File::create(path)
}

#[cfg(unix)]
fn restrict_dir(dir: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
        .with_context(|| format!("failed to restrict {}", dir.display()))
}

#[cfg(not(unix))]
#[allow(
    clippy::unnecessary_wraps,
    reason = "the signature has to match the unix version"
)]
fn restrict_dir(_dir: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_ctx(tag: &str) -> (AppContext, PathBuf) {
        let root = std::env::temp_dir().join(format!("cqr-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("app")).unwrap();
        fs::create_dir_all(root.join("claude")).unwrap();
        let ctx = AppContext {
            app_dir: root.join("app"),
            claude_dir: root.join("claude"),
            migration: None,
        };
        (ctx, root)
    }

    #[test]
    fn saves_reads_and_removes_a_credential() {
        let (ctx, root) = temp_ctx("roundtrip");
        let name = AccountName::parse("me@example.com").unwrap();

        write_saved(&ctx, &name, r#"{"token":"saved"}"#).unwrap();
        assert_eq!(read_saved(&ctx, &name).unwrap(), r#"{"token":"saved"}"#);

        write_active(&ctx, r#"{"token":"active"}"#).unwrap();
        assert_eq!(read_active(&ctx).unwrap(), r#"{"token":"active"}"#);

        // Replacing an existing credential must not leave the old bytes behind.
        write_saved(&ctx, &name, r#"{"token":"rotated"}"#).unwrap();
        assert_eq!(read_saved(&ctx, &name).unwrap(), r#"{"token":"rotated"}"#);

        delete_saved(&ctx, &name).unwrap();
        assert!(read_saved(&ctx, &name).is_err());
        // Removing an account that is already gone is not an error.
        delete_saved(&ctx, &name).unwrap();

        fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn keeps_credentials_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let (ctx, root) = temp_ctx("perms");
        let name = AccountName::parse("me@example.com").unwrap();
        write_saved(&ctx, &name, "{}").unwrap();
        write_active(&ctx, "{}").unwrap();

        let file_mode = |path: PathBuf| {
            fs::metadata(path).unwrap().permissions().mode() & 0o777
        };
        assert_eq!(file_mode(account_path(&ctx, &name)), 0o600);
        assert_eq!(file_mode(ctx.active_credential_path()), 0o600);
        assert_eq!(
            fs::metadata(ctx.accounts_dir()).unwrap().permissions().mode() & 0o777,
            0o700
        );

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn refuses_a_symlinked_credential_path() {
        let (_ctx, root) = temp_ctx("symlink");
        let target = root.join("real.json");
        fs::write(&target, "{}").unwrap();
        let link = root.join("link.json");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, &link).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(&target, &link).unwrap();

        assert!(read_private(&link).is_err());
        assert!(write_private(&link, "{}").is_err());
        assert_eq!(fs::read_to_string(&target).unwrap(), "{}");

        fs::remove_dir_all(&root).unwrap();
    }
}
