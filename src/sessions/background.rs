//! The macOS launch agent that runs `sessions sync --background`.
//!
//! The agent runs at login, every interval, and whenever an account directory
//! appears under `claude-code-sessions`. The plist carries an ownership marker,
//! and this module refuses to replace or remove a plist or a loaded job with
//! the same label that lacks it. `launchctl` and `plutil` run with a fixed
//! environment, a time limit, and an output limit, and only the facts parsed
//! from their output are kept, because `launchctl print` lists a job's
//! environment.

use super::paths::{LAUNCH_AGENT_LABEL, SessionPaths};
use super::platform;
use super::staging::StagedFile;
use anyhow::{Context, Result, anyhow, bail};
use serde::Serialize;
use serde_json::Value;
use std::env;
use std::fmt::Write as _;
use std::fs;
use std::io::{self, ErrorKind, Read};
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const MARKER_KEY: &str = "ClaudeAccountKitManaged";
const MARKER: &str = "claude-account-kit/sessions/v1";
/// `launchctl` normally answers in milliseconds; a hang must not hold a sync.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_OUTPUT: usize = 64 * 1024;
const MAX_PLIST_BYTES: u64 = 64 * 1024;

#[derive(Clone, Debug, Default, Serialize)]
pub(super) struct BackgroundStatus {
    /// False anywhere but macOS.
    pub(super) supported: bool,
    /// This tool's plist is in `~/Library/LaunchAgents`.
    pub(super) installed: bool,
    /// launchd has a job with this tool's label.
    pub(super) loaded: bool,
    /// Seconds between runs, from the installed plist.
    pub(super) interval: Option<u64>,
    pub(super) pid: Option<i64>,
    pub(super) last_exit: Option<i64>,
}

/// The plist text, ready for `~/Library/LaunchAgents`.
pub(super) fn render_plist(program: &Path, interval: u32, paths: &SessionPaths) -> String {
    let program = escape(&program.to_string_lossy());
    let mut arguments: String = String::new();
    for root in &paths.additional_desktop_roots {
        let _ = writeln!(
            arguments,
            "\t\t<string>--additional-desktop-root</string>\n\t\t<string>{}</string>",
            escape(&root.to_string_lossy())
        );
    }
    let mut watches: String = String::new();
    for root in paths.desktop_roots() {
        let _ = writeln!(
            watches,
            "\t\t<string>{}</string>",
            escape(&root.join("claude-code-sessions").to_string_lossy())
        );
    }
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>{MARKER_KEY}</key>
	<string>{MARKER}</string>
	<key>Label</key>
	<string>{LAUNCH_AGENT_LABEL}</string>
	<key>ProcessType</key>
	<string>Background</string>
	<key>ProgramArguments</key>
	<array>
		<string>{program}</string>
		<string>sessions</string>
		<string>sync</string>
		<string>--background</string>
{arguments}	</array>
	<key>RunAtLoad</key>
	<true/>
	<key>StandardErrorPath</key>
	<string>/dev/null</string>
	<key>StandardOutPath</key>
	<string>/dev/null</string>
	<key>StartInterval</key>
	<integer>{interval}</integer>
	<key>Umask</key>
	<integer>{umask}</integer>
	<key>WatchPaths</key>
	<array>
{watches}	</array>
</dict>
</plist>
"#,
        umask = 0o077
    )
}

/// The absolute path of the running binary, which the agent will run.
pub(super) fn current_program() -> Result<std::path::PathBuf> {
    let program = env::current_exe().context("failed to find the running executable")?;
    if !program.is_absolute() {
        bail!("the running executable has no absolute path: {}", program.display());
    }
    Ok(program)
}

/// Install this tool's launch agent and load it in the user's GUI session.
pub(super) fn turn_on(paths: &SessionPaths, interval: u32) -> Result<BackgroundStatus> {
    require_macos()?;
    let program = current_program()?;
    let domain = user_domain()?;
    let target = format!("{domain}/{LAUNCH_AGENT_LABEL}");
    let existing = owned_plist(&paths.launch_agent)?;
    let loaded = launchctl(&["print", &target], true)?.is_some();
    if loaded && existing.is_none() {
        bail!(
            "launchd already has a job named {LAUNCH_AGENT_LABEL} without this tool's plist; \
             leaving it unchanged"
        );
    }
    let plist = render_plist(&program, interval, paths);
    let dir = paths
        .launch_agent
        .parent()
        .ok_or_else(|| anyhow!("the launch agent path has no directory"))?;
    fs::create_dir_all(dir).with_context(|| format!("failed to create {}", dir.display()))?;
    let staged = StagedFile::write(dir, plist.as_bytes())
        .with_context(|| format!("failed to write the plist in {}", dir.display()))?;
    staged
        .replace(&paths.launch_agent)
        .with_context(|| format!("failed to install {}", paths.launch_agent.display()))?;
    if loaded {
        launchctl(&["bootout", &target], true)?;
    }
    launchctl(&["enable", &target], false)?;
    let plist_path = paths.launch_agent.to_string_lossy();
    launchctl(&["bootstrap", &domain, &plist_path], false)?;
    status(paths)
}

/// Unload and remove this tool's launch agent; other jobs are never touched.
pub(super) fn turn_off(paths: &SessionPaths) -> Result<BackgroundStatus> {
    require_macos()?;
    let target = format!("{}/{LAUNCH_AGENT_LABEL}", user_domain()?);
    if owned_plist(&paths.launch_agent)?.is_none() {
        if launchctl(&["print", &target], true)?.is_some() {
            bail!(
                "launchd has a job named {LAUNCH_AGENT_LABEL} without this tool's plist; \
                 leaving it unchanged"
            );
        }
        return status(paths);
    }
    launchctl(&["bootout", &target], true)?;
    // The plist is checked again right before it goes, in case it changed.
    if owned_plist(&paths.launch_agent)?.is_some() {
        fs::remove_file(&paths.launch_agent)
            .with_context(|| format!("failed to remove {}", paths.launch_agent.display()))?;
        if let Some(dir) = paths.launch_agent.parent() {
            platform::sync_dir(dir).ok();
        }
    }
    status(paths)
}

/// Whether the agent is installed and loaded, and how its last run ended.
///
/// A plist with the label but without the marker reads as not installed
/// rather than as an error, so status never fails over a file it will not touch.
pub(super) fn status(paths: &SessionPaths) -> Result<BackgroundStatus> {
    if !cfg!(target_os = "macos") {
        return Ok(BackgroundStatus::default());
    }
    let mut status = BackgroundStatus {
        supported: true,
        ..BackgroundStatus::default()
    };
    if let Ok(Some(plist)) = owned_plist(&paths.launch_agent) {
        status.installed = true;
        status.interval = plist.interval;
    }
    let target = format!("{}/{LAUNCH_AGENT_LABEL}", user_domain()?);
    if let Some(output) = launchctl(&["print", &target], true)? {
        status.loaded = true;
        status.pid = field(&output, "pid");
        status.last_exit = field(&output, "last exit code");
    }
    Ok(status)
}

fn require_macos() -> Result<()> {
    if cfg!(target_os = "macos") {
        Ok(())
    } else {
        bail!("background sync uses a macOS launch agent; on this platform run `sessions sync` instead")
    }
}

/// What this tool reads back from its own plist.
struct OwnedPlist {
    interval: Option<u64>,
}

/// Read the plist at `path` if it is this tool's; refuse one that is not.
fn owned_plist(path: &Path) -> Result<Option<OwnedPlist>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.file_type().is_file() => {
            bail!("refusing to use {} because it is not a regular file", path.display())
        }
        Ok(metadata) if metadata.len() > MAX_PLIST_BYTES => {
            bail!("refusing to read {} because it is unusually large", path.display())
        }
        Ok(_) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to inspect {}", path.display()));
        }
    }
    let path_text = path.to_string_lossy();
    let output = run_bounded(
        "/usr/bin/plutil",
        &["-convert", "json", "-o", "-", &path_text],
    )?;
    if !output.success {
        bail!("{} is not a readable plist; leaving it unchanged", path.display());
    }
    let plist: Value = serde_json::from_str(&output.text).with_context(|| {
        format!(
            "{} is not a readable plist; leaving it unchanged",
            path.display()
        )
    })?;
    if plist.get("Label").and_then(Value::as_str) != Some(LAUNCH_AGENT_LABEL) {
        bail!("{} belongs to another job; leaving it unchanged", path.display());
    }
    if plist.get(MARKER_KEY).and_then(Value::as_str) != Some(MARKER) {
        bail!(
            "{} lacks this tool's ownership marker; leaving it unchanged",
            path.display()
        );
    }
    Ok(Some(OwnedPlist {
        interval: plist.get("StartInterval").and_then(Value::as_u64),
    }))
}

/// `gui/<uid>`, the user's login session domain in launchd.
fn user_domain() -> Result<String> {
    let output = run_bounded("/usr/bin/id", &["-u"])?;
    let uid = output.text.trim();
    if !output.success || uid.is_empty() || !uid.bytes().all(|byte| byte.is_ascii_digit()) {
        bail!("failed to read the current user id");
    }
    Ok(format!("gui/{uid}"))
}

/// Run `launchctl`; with `missing_ok`, a job that does not exist is `None`.
fn launchctl(args: &[&str], missing_ok: bool) -> Result<Option<String>> {
    let output = run_bounded("/bin/launchctl", args)?;
    if output.success {
        return Ok(Some(output.text));
    }
    let lower = output.text.to_lowercase();
    let missing = matches!(output.code, Some(3 | 113))
        && (lower.contains("could not find service") || lower.contains("no such process"));
    if missing_ok && missing {
        return Ok(None);
    }
    let reason = [
        "operation not permitted",
        "permission denied",
        "input/output error",
    ]
    .into_iter()
    .find(|phrase| lower.contains(phrase))
    .unwrap_or("service operation failed");
    bail!(
        "launchctl {} failed: {reason} (exit {})",
        args[0],
        output.code.map_or_else(|| "signal".to_string(), |code| code.to_string())
    )
}

/// A `key = value` line from `launchctl print`, as a number.
fn field(output: &str, key: &str) -> Option<i64> {
    output.lines().find_map(|line| {
        line.trim()
            .strip_prefix(key)?
            .trim_start()
            .strip_prefix('=')?
            .trim()
            .parse()
            .ok()
    })
}

struct CommandOutput {
    success: bool,
    code: Option<i32>,
    /// Standard output followed by standard error.
    text: String,
}

/// Run a system tool with a fixed environment, a time limit, and an output limit.
fn run_bounded(program: &str, args: &[&str]) -> Result<CommandOutput> {
    let mut child = Command::new(program)
        .args(args)
        .env_clear()
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to run {program}"))?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let stdout_reader = thread::spawn(move || read_capped(stdout));
    let stderr_reader = thread::spawn(move || read_capped(stderr));
    let deadline = Instant::now() + COMMAND_TIMEOUT;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!("{program} {} did not finish within 15 seconds", args.first().unwrap_or(&""));
        }
        thread::sleep(Duration::from_millis(20));
    };
    let mut bytes = stdout_reader.join().unwrap_or_default();
    bytes.extend(stderr_reader.join().unwrap_or_default());
    if bytes.len() > MAX_OUTPUT {
        bail!("{program} printed more output than expected");
    }
    Ok(CommandOutput {
        success: status.success(),
        code: status.code(),
        text: String::from_utf8_lossy(&bytes).into_owned(),
    })
}

/// Keep up to one byte past the limit, and drain the rest so the child never blocks.
fn read_capped(stream: Option<impl Read>) -> Vec<u8> {
    let Some(mut stream) = stream else {
        return Vec::new();
    };
    let mut kept = Vec::new();
    let _ = (&mut stream).take(MAX_OUTPUT as u64 + 1).read_to_end(&mut kept);
    let _ = io::copy(&mut stream, &mut io::sink());
    kept
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
