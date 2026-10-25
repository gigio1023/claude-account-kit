//! Contract tests for `claude-account-kit sessions sync`.
//!
//! The command writes into Claude Desktop's own data directory. A bug that
//! overwrites or deletes a file Desktop owns, edits a transcript, or ignores a
//! deletion marker loses user data, and a normal run would not show it. Each
//! test builds a Desktop root and a Claude Code projects root in its own
//! temporary directory, runs the built binary on them, and checks the files
//! that result against the sync contract. The tests were written from that
//! contract without reading the implementation, and they assert nothing about
//! output text: stdout is only checked to be JSON.
//!
//! Every run also checks two guarantees that hold for any input: no file under
//! the projects root changes (contract 4), and under the Desktop root only
//! `local_<uuid>.json` files inside partitions that already existed change
//! (contract 9). See [`Sandbox::sync`].

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

// ---------------------------------------------------------------------------
// Fixture helpers
// ---------------------------------------------------------------------------

const BIN: &str = env!("CARGO_BIN_EXE_claude-account-kit");

/// `createdAt` of every fixture conversation, in Unix milliseconds.
const CREATED_MS: u64 = 1_790_000_000_000;
const MINUTE_MS: u64 = 60_000;
const HOUR_MS: u64 = 3_600_000;

/// Canonical lowercase UUID text, distinct for each `n`.
fn uuid(n: u32) -> String {
    format!("{n:08x}-0000-4000-8000-{n:012x}")
}

/// One test's private filesystem: the Desktop root, the projects root, the
/// tool's state directory, HOME, and the working directory the fixture
/// conversations point at. The whole tree is removed on drop unless the test
/// failed, in which case its path is printed for inspection.
struct Sandbox {
    root: PathBuf,
    desktop: PathBuf,
    projects: PathBuf,
    state: PathBuf,
    home: PathBuf,
    cwd: PathBuf,
}

impl Sandbox {
    fn new() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        let name = format!(
            "cak-sessions-sync-{}-{}-{nanos}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let created = std::env::temp_dir().join(name);
        fs::create_dir_all(&created).expect("create sandbox root");
        // The temp dir can sit behind a symbolic link (/var on macOS); resolve
        // it so every cwd a fixture records is a real absolute path.
        let root = fs::canonicalize(&created).expect("resolve sandbox root");
        let sandbox = Self {
            desktop: root.join("desktop"),
            projects: root.join("projects"),
            state: root.join("state"),
            home: root.join("home"),
            cwd: root.join("work").join("project"),
            root,
        };
        for dir in [
            sandbox.desktop.join("claude-code-sessions"),
            sandbox.projects.clone(),
            sandbox.state.clone(),
            sandbox.home.clone(),
            sandbox.cwd.clone(),
        ] {
            fs::create_dir_all(dir).expect("create sandbox directory");
        }
        sandbox
    }

    /// Creates the partition `<D>/claude-code-sessions/<account>/<org>/`.
    fn partition(&self, account: u32, org: u32) -> PathBuf {
        let dir = self
            .desktop
            .join("claude-code-sessions")
            .join(uuid(account))
            .join(uuid(org));
        fs::create_dir_all(&dir).expect("create partition");
        dir
    }

    /// The wrapper Desktop keeps for conversation `wrapper` whose current
    /// transcript is `cli`, with earlier transcript ids in `prior`.
    fn wrapper_json(&self, wrapper: &str, cli: &str, prior: &[&str], last_activity_ms: u64) -> Value {
        let cwd = self.cwd.to_str().expect("utf-8 cwd");
        let mut value = json!({
            "sessionId": format!("local_{wrapper}"),
            "cliSessionId": cli,
            "cwd": cwd,
            "originCwd": cwd,
            "createdAt": CREATED_MS,
            "lastActivityAt": last_activity_ms,
            "title": "Fixture conversation",
            "isArchived": false,
        });
        if !prior.is_empty() {
            value["priorCliSessionIds"] = json!(prior);
        }
        value
    }

    /// Writes transcript `<P>/<project dir>/<cli>.jsonl` whose lines record
    /// the sandbox working directory.
    fn write_transcript(&self, cli: &str) -> PathBuf {
        self.write_transcript_in(cli, &self.cwd)
    }

    /// Writes transcript `<P>/<project dir of cwd>/<cli>.jsonl` with a short
    /// exchange whose lines record `cwd`, which need not exist.
    fn write_transcript_in(&self, cli: &str, cwd: &Path) -> PathBuf {
        self.write_transcript_from(cli, cwd, "cli")
    }

    /// Writes a transcript the way Claude Desktop starts one.
    fn write_desktop_transcript(&self, cli: &str) -> PathBuf {
        self.write_transcript_from(cli, &self.cwd, "claude-desktop")
    }

    /// Writes a transcript whose records name `entrypoint` as the client.
    fn write_transcript_from(&self, cli: &str, cwd: &Path, entrypoint: &str) -> PathBuf {
        let cwd = cwd.to_str().expect("utf-8 cwd");
        // Claude Code names a project directory after its cwd with every
        // character other than an ASCII letter or digit replaced by '-'.
        let project: String = cwd
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        let lines = [
            json!({"type": "user", "sessionId": cli, "cwd": cwd,
                   "entrypoint": entrypoint,
                   "timestamp": "2026-10-01T00:00:00.000Z",
                   "message": {"role": "user", "content": "hi"}}),
            json!({"type": "assistant", "sessionId": cli, "cwd": cwd,
                   "entrypoint": entrypoint,
                   "timestamp": "2026-10-01T00:00:05.000Z",
                   "message": {"role": "assistant", "content": [{"type": "text", "text": "hello"}]}}),
        ];
        let mut body = String::new();
        for line in &lines {
            body.push_str(&line.to_string());
            body.push('\n');
        }
        let path = self.projects.join(project).join(format!("{cli}.jsonl"));
        write_file(&path, body.as_bytes());
        path
    }

    /// Path of `file` relative to the Desktop root, as snapshots key it.
    fn desktop_relative(&self, file: &Path) -> String {
        let relative = file.strip_prefix(&self.desktop).expect("path under the Desktop root");
        relative.to_str().expect("utf-8 path").to_owned()
    }

    /// Runs `sessions sync` on this sandbox with `extra_args` and checks what
    /// every run must hold: exit status 0, JSON on stdout, no change under the
    /// projects root (contract 4), and under the Desktop root no change except
    /// `local_<uuid>.json` files inside partitions that existed before the run
    /// (contract 9).
    fn sync(&self, extra_args: &[&str]) {
        let desktop_before = tree(&self.desktop);
        let projects_before = tree(&self.projects);

        let mut command = Command::new(BIN);
        command
            .args(["sessions", "sync", "--desktop-root"])
            .arg(&self.desktop)
            .arg("--projects-root")
            .arg(&self.projects)
            .arg("--json")
            .args(extra_args);
        // Keep the run away from the real Claude configuration and the real
        // home, where a legacy ledger could live.
        for (key, _) in std::env::vars_os() {
            let key_text = key.to_string_lossy();
            if key_text.starts_with("CLAUDE") || key_text.starts_with("ANTHROPIC") {
                command.env_remove(&key);
            }
        }
        command
            .env("CLAUDE_ACCOUNT_KIT_HOME", &self.state)
            .env("HOME", &self.home)
            .env("CLAUDE_CONFIG_DIR", self.home.join(".claude"));
        let output = command.output().expect("run claude-account-kit");

        assert!(
            output.status.success(),
            "sessions sync {extra_args:?} failed with {} in {}\nstderr:\n{}",
            output.status,
            self.root.display(),
            String::from_utf8_lossy(&output.stderr)
        );
        if let Err(error) = serde_json::from_slice::<Value>(&output.stdout) {
            panic!(
                "stdout is not JSON ({error}):\n{}",
                String::from_utf8_lossy(&output.stdout)
            );
        }
        let touched = changed_paths(&projects_before, &tree(&self.projects));
        assert!(
            touched.is_empty(),
            "contract 4: sync changed files under the projects root: {touched:?}"
        );
        let stray: Vec<String> = changed_paths(&desktop_before, &tree(&self.desktop))
            .into_iter()
            .filter(|path| !is_wrapper_in_partition(path, &desktop_before))
            .collect();
        assert!(
            stray.is_empty(),
            "contract 9: sync changed Desktop paths that are not wrappers in existing partitions: {stray:?}"
        );
    }

    fn dry_run(&self) {
        self.sync(&["--dry-run"]);
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!("sandbox kept for inspection: {}", self.root.display());
        } else {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
}

/// Writes `value` as `<partition>/<sessionId>.json` the way Desktop would and
/// returns the bytes written.
fn write_wrapper(partition: &Path, value: &Value) -> Vec<u8> {
    let session_id = value["sessionId"].as_str().expect("wrapper has a sessionId");
    let bytes = serde_json::to_vec_pretty(value).expect("serialize wrapper");
    write_file(&partition.join(format!("{session_id}.json")), &bytes);
    bytes
}

/// Writes an empty deletion marker named `name` into `partition`.
fn write_marker(partition: &Path, name: &str) {
    write_file(&partition.join(name), b"");
}

fn write_file(path: &Path, bytes: &[u8]) {
    fs::create_dir_all(path.parent().expect("file has a parent")).expect("create parent directory");
    fs::write(path, bytes).expect("write fixture file");
}

/// Where the wrapper of conversation `wrapper` lives in `partition`.
fn wrapper_path(partition: &Path, wrapper: &str) -> PathBuf {
    partition.join(format!("local_{wrapper}.json"))
}

fn read_bytes(path: &Path) -> Vec<u8> {
    fs::read(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&read_bytes(path))
        .unwrap_or_else(|error| panic!("{} is not JSON: {error}", path.display()))
}

/// The `cliSessionId` a wrapper file points at.
fn cli_session_id(path: &Path) -> String {
    read_json(path)["cliSessionId"]
        .as_str()
        .unwrap_or_else(|| panic!("{} has no string cliSessionId", path.display()))
        .to_owned()
}

/// Conversation ids of the `local_<id>.json` wrappers in `partition`.
fn wrapper_ids(partition: &Path) -> BTreeSet<String> {
    fs::read_dir(partition)
        .expect("read partition")
        .map(|entry| entry.expect("partition entry").file_name().into_string().expect("utf-8 name"))
        .filter_map(|name| Some(name.strip_prefix("local_")?.strip_suffix(".json")?.to_owned()))
        .collect()
}

fn set<S: AsRef<str>>(ids: &[S]) -> BTreeSet<String> {
    ids.iter().map(|id| id.as_ref().to_owned()).collect()
}

/// Changes one byte of a wrapper file the way a person renaming the
/// conversation would: the first letter of its title flips case. The file
/// stays valid JSON. Returns the new bytes.
fn edit_title(path: &Path) -> Vec<u8> {
    let mut bytes = read_bytes(path);
    let key = b"\"title\"";
    let key_at = bytes
        .windows(key.len())
        .position(|window| window == key)
        .expect("wrapper has a title");
    let after_key = key_at + key.len();
    let open_quote = after_key
        + bytes[after_key..]
            .iter()
            .position(|&byte| byte == b'"')
            .expect("title is a string");
    let first = open_quote + 1;
    assert!(bytes[first].is_ascii_alphabetic(), "title starts with a letter");
    bytes[first] ^= 0x20;
    fs::write(path, &bytes).expect("write edited wrapper");
    serde_json::from_slice::<Value>(&bytes).expect("edited wrapper is still JSON");
    bytes
}

/// One filesystem entry as a snapshot records it. Symbolic links are recorded
/// as links and never followed.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Entry {
    Dir,
    File(Vec<u8>),
    Link(PathBuf),
}

/// Every entry under a root, keyed by its `/`-joined path relative to the root.
type Tree = BTreeMap<String, Entry>;

fn tree(root: &Path) -> Tree {
    let mut entries = Tree::new();
    walk(root, "", &mut entries);
    entries
}

fn walk(dir: &Path, prefix: &str, entries: &mut Tree) {
    for item in fs::read_dir(dir).expect("read directory") {
        let item = item.expect("directory entry");
        let name = item.file_name().into_string().expect("utf-8 file name");
        let relative = if prefix.is_empty() { name } else { format!("{prefix}/{name}") };
        let kind = item.file_type().expect("file type");
        if kind.is_symlink() {
            entries.insert(relative, Entry::Link(fs::read_link(item.path()).expect("read link")));
        } else if kind.is_dir() {
            entries.insert(relative.clone(), Entry::Dir);
            walk(&item.path(), &relative, entries);
        } else {
            entries.insert(relative, Entry::File(read_bytes(&item.path())));
        }
    }
}

/// Paths whose entry differs between two snapshots, added and removed ones included.
fn changed_paths(before: &Tree, after: &Tree) -> Vec<String> {
    let paths: BTreeSet<&String> = before.keys().chain(after.keys()).collect();
    paths
        .into_iter()
        .filter(|path| before.get(*path) != after.get(*path))
        .cloned()
        .collect()
}

/// Whether `path` is a `local_<uuid>.json` regular file directly inside a
/// partition that was a real directory in `before`.
fn is_wrapper_in_partition(path: &str, before: &Tree) -> bool {
    let Some((partition, file)) = path.rsplit_once('/') else {
        return false;
    };
    let is_partition_path = matches!(
        partition.split('/').collect::<Vec<_>>().as_slice(),
        ["claude-code-sessions", account, org] if is_canonical_uuid(account) && is_canonical_uuid(org)
    );
    let partition_existed = before.get(partition) == Some(&Entry::Dir);
    let is_wrapper_name = file
        .strip_prefix("local_")
        .and_then(|rest| rest.strip_suffix(".json"))
        .is_some_and(is_canonical_uuid);
    let was_not_dir_or_link = matches!(before.get(path), None | Some(Entry::File(_)));
    is_partition_path && partition_existed && is_wrapper_name && was_not_dir_or_link
}

fn is_canonical_uuid(text: &str) -> bool {
    text.len() == 36
        && text.char_indices().all(|(index, c)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                c == '-'
            } else {
                c.is_ascii_digit() || ('a'..='f').contains(&c)
            }
        })
}

// ---------------------------------------------------------------------------
// Contract tests
// ---------------------------------------------------------------------------

/// Contract 1: a conversation Desktop lists in one partition, and a transcript
/// no wrapper refers to, both appear in every partition; the source wrapper
/// stays as Desktop wrote it (contract 2).
#[test]
fn adds_each_conversation_to_every_partition() {
    let sb = Sandbox::new();
    let a1 = sb.partition(0xA, 0xA1);
    let b1 = sb.partition(0xB, 0xB1);
    let c1 = sb.partition(0xC, 0xC1);
    let (w, x1, y) = (uuid(0x101), uuid(0x201), uuid(0x301));
    sb.write_transcript(&x1);
    let a_original = write_wrapper(&a1, &sb.wrapper_json(&w, &x1, &[], CREATED_MS + MINUTE_MS));
    sb.write_transcript(&y);

    sb.sync(&[]);

    for partition in [&b1, &c1] {
        let copy = read_json(&wrapper_path(partition, &w));
        assert_eq!(copy["sessionId"], json!(format!("local_{w}")), "copy keeps the wrapper id");
        assert_eq!(copy["cliSessionId"], json!(x1), "copy points at the source transcript");
    }
    for partition in [&a1, &b1, &c1] {
        let entry = read_json(&wrapper_path(partition, &y));
        assert_eq!(entry["sessionId"], json!(format!("local_{y}")), "CLI-only wrapper id is the transcript id");
        assert_eq!(entry["cliSessionId"], json!(y));
        assert_eq!(wrapper_ids(partition), set(&[&w, &y]), "one wrapper per conversation");
    }
    assert_eq!(read_bytes(&wrapper_path(&a1, &w)), a_original, "contract 2: source wrapper untouched");
}

/// Contract 2: wrappers Desktop wrote stay byte-identical across runs, even an
/// entry whose transcript is gone. The one exception is an entry the newest
/// copy has moved on from: it follows the move to the new transcript and keeps
/// its own title and other fields, because Desktop rewrites every entry when
/// an app opens or quits and leaves the entry behind otherwise.
#[test]
fn never_modifies_a_wrapper_it_did_not_write() {
    let sb = Sandbox::new();
    let a1 = sb.partition(0xA, 0xA1);
    let b1 = sb.partition(0xB, 0xB1);
    let c1 = sb.partition(0xC, 0xC1);
    let (w, x1, x2) = (uuid(0x101), uuid(0x201), uuid(0x202));
    let (v, xv) = (uuid(0x102), uuid(0x203));
    sb.write_transcript(&x1);
    write_wrapper(&a1, &sb.wrapper_json(&w, &x1, &[], CREATED_MS + MINUTE_MS));
    // Account B's Desktop already lists W: older activity, its own title, an
    // extra field, compact formatting.
    let mut b_entry = sb.wrapper_json(&w, &x1, &[], CREATED_MS);
    b_entry["title"] = json!("Renamed in account B");
    b_entry["model"] = json!("fixture-model");
    let b_original = serde_json::to_vec(&b_entry).expect("serialize");
    write_file(&wrapper_path(&b1, &w), &b_original);
    // Account C's Desktop lists V, whose transcript no longer exists.
    let c_original = write_wrapper(&c1, &sb.wrapper_json(&v, &xv, &[], CREATED_MS));

    sb.sync(&[]);

    assert_eq!(read_bytes(&wrapper_path(&b1, &w)), b_original, "B's own entry for W");
    assert_eq!(read_bytes(&wrapper_path(&c1, &v)), c_original, "C's own entry for V");

    // Desktop moves W in A to a new transcript, so B's own entry is now stale.
    sb.write_transcript(&x2);
    let a_moved = write_wrapper(&a1, &sb.wrapper_json(&w, &x2, &[&x1], CREATED_MS + HOUR_MS));

    sb.sync(&[]);

    let b_moved = read_json(&wrapper_path(&b1, &w));
    assert_eq!(b_moved["cliSessionId"], json!(x2), "B's entry follows the move");
    assert_eq!(b_moved["priorCliSessionIds"], json!([x1]), "B's entry history");
    assert_eq!(b_moved["lastActivityAt"], json!(CREATED_MS + HOUR_MS), "B's entry activity");
    assert_eq!(b_moved["title"], json!("Renamed in account B"), "B's own title");
    assert_eq!(b_moved["model"], json!("fixture-model"), "B's own field");
    assert_eq!(read_bytes(&wrapper_path(&c1, &v)), c_original, "C's orphan entry is not removed");
    assert_eq!(read_bytes(&wrapper_path(&a1, &w)), a_moved, "A's moved entry");
}

/// Contract 3: a deletion marker for a conversation, named by wrapper id or by
/// transcript id, keeps that conversation out of every partition of the
/// marker's account, while other accounts still receive it.
#[test]
fn deletion_marker_keeps_conversation_out_of_its_whole_account() {
    let sb = Sandbox::new();
    let a1 = sb.partition(0xA, 0xA1);
    let b1 = sb.partition(0xB, 0xB1);
    let b2 = sb.partition(0xB, 0xB2);
    let c1 = sb.partition(0xC, 0xC1);
    let (w, xw) = (uuid(0x101), uuid(0x201));
    let (v, xv) = (uuid(0x102), uuid(0x202));
    let y = uuid(0x301);
    for cli in [&xw, &xv, &y] {
        sb.write_transcript(cli);
    }
    write_wrapper(&a1, &sb.wrapper_json(&w, &xw, &[], CREATED_MS + MINUTE_MS));
    write_wrapper(&a1, &sb.wrapper_json(&v, &xv, &[], CREATED_MS + MINUTE_MS));
    write_marker(&b1, &format!("deleted_local_{w}"));
    write_marker(&c1, &format!("deleted_{xv}"));
    write_marker(&b2, &format!("deleted_{y}"));

    sb.sync(&[]);
    sb.sync(&[]);

    for (partition, id) in [(&b1, &w), (&b2, &w), (&c1, &v), (&b1, &y), (&b2, &y)] {
        assert!(
            !wrapper_path(partition, id).exists(),
            "contract 3: {id} was added to {} despite a deletion marker in its account",
            sb.desktop_relative(partition)
        );
    }
    // The same conversations still reach accounts without a marker (contract 1).
    for (partition, id) in [(&c1, &w), (&b1, &v), (&b2, &v), (&a1, &y), (&c1, &y)] {
        assert!(wrapper_path(partition, id).exists(), "{id} missing from {}", sb.desktop_relative(partition));
    }
}

/// Contract 3: when a person deletes a copy in Desktop, which leaves a marker,
/// the next sync does not bring the conversation back.
#[test]
fn deletion_in_desktop_is_not_undone_by_the_next_sync() {
    let sb = Sandbox::new();
    let a1 = sb.partition(0xA, 0xA1);
    let c1 = sb.partition(0xC, 0xC1);
    let (w, x1) = (uuid(0x101), uuid(0x201));
    sb.write_transcript(&x1);
    write_wrapper(&a1, &sb.wrapper_json(&w, &x1, &[], CREATED_MS + MINUTE_MS));
    sb.sync(&[]);
    assert!(wrapper_path(&c1, &w).exists(), "precondition: first sync added W to C");

    fs::remove_file(wrapper_path(&c1, &w)).expect("Desktop deletes the entry");
    write_marker(&c1, &format!("deleted_local_{w}"));
    sb.sync(&[]);

    assert!(!wrapper_path(&c1, &w).exists(), "contract 3: deleted conversation came back");
}

/// Contract 4: transcripts and the other files Claude Code keeps under the
/// projects root survive adds, dry runs, refreshes, and removals unchanged.
#[test]
fn never_touches_transcripts() {
    let sb = Sandbox::new();
    let a1 = sb.partition(0xA, 0xA1);
    sb.partition(0xB, 0xB1);
    let (w, x1, x2) = (uuid(0x101), uuid(0x201), uuid(0x202));
    let (y, z) = (uuid(0x301), uuid(0x302));
    let x1_path = sb.write_transcript(&x1);
    write_wrapper(&a1, &sb.wrapper_json(&w, &x1, &[], CREATED_MS + MINUTE_MS));
    sb.write_transcript(&y);
    sb.write_transcript_in(&z, &sb.root.join("deleted-checkout"));
    let project = x1_path.parent().expect("project directory").to_owned();
    write_file(&project.join(&x1).join("subagents").join("agent-1.jsonl"), b"{\"type\":\"user\"}\n");
    write_file(&project.join("memory").join("notes.md"), b"# notes\n");
    let initial = tree(&sb.projects);

    sb.sync(&[]);
    sb.dry_run();
    sb.sync(&[]);
    assert!(changed_paths(&initial, &tree(&sb.projects)).is_empty(), "projects root changed");

    // Desktop moves W to a new transcript; the sync then refreshes W's copies.
    let x2_path = sb.write_transcript(&x2);
    write_wrapper(&a1, &sb.wrapper_json(&w, &x2, &[&x1], CREATED_MS + HOUR_MS));
    sb.sync(&[]);
    // Every transcript of W goes away; the sync then removes W's copies.
    fs::remove_file(&x1_path).expect("remove transcript");
    fs::remove_file(&x2_path).expect("remove transcript");
    let expected = tree(&sb.projects);
    sb.sync(&[]);

    assert!(changed_paths(&expected, &tree(&sb.projects)).is_empty(), "projects root changed");
}

/// Contract 5: when Desktop moves a conversation to a new transcript, every
/// copy follows it, and a copy someone changed since keeps that change; and
/// the run after that changes nothing (contract 7).
#[test]
fn refresh_moves_every_copy_and_keeps_its_changes() {
    let sb = Sandbox::new();
    let a1 = sb.partition(0xA, 0xA1);
    let b1 = sb.partition(0xB, 0xB1);
    let c1 = sb.partition(0xC, 0xC1);
    let (w, x1, x2) = (uuid(0x101), uuid(0x201), uuid(0x202));
    sb.write_transcript(&x1);
    write_wrapper(&a1, &sb.wrapper_json(&w, &x1, &[], CREATED_MS + MINUTE_MS));
    sb.sync(&[]);
    for partition in [&b1, &c1] {
        assert_eq!(cli_session_id(&wrapper_path(partition, &w)), x1, "precondition: copy of W");
    }

    let b_edited: Value =
        serde_json::from_slice(&edit_title(&wrapper_path(&b1, &w))).expect("edited wrapper");
    sb.write_transcript(&x2);
    let a_moved = write_wrapper(&a1, &sb.wrapper_json(&w, &x2, &[&x1], CREATED_MS + HOUR_MS));
    sb.sync(&[]);

    assert_eq!(cli_session_id(&wrapper_path(&c1, &w)), x2, "contract 5: unchanged copy follows the move");
    let b_moved = read_json(&wrapper_path(&b1, &w));
    assert_eq!(b_moved["cliSessionId"], json!(x2), "contract 5: edited copy follows the move");
    assert_eq!(b_moved["title"], b_edited["title"], "contract 5: edited title kept");
    assert_eq!(read_bytes(&wrapper_path(&a1, &w)), a_moved, "contract 2: Desktop's wrapper");

    let settled = tree(&sb.desktop);
    sb.sync(&[]);
    let changed = changed_paths(&settled, &tree(&sb.desktop));
    assert!(changed.is_empty(), "contract 7: run after a refresh changed {changed:?}");
}

/// Contract 10: a transcript Desktop started is listed only under Desktop's
/// own entry. Desktop writes the transcript before its entry points at it, so
/// a run in between lists nothing, and a second entry named after the
/// transcript is removed once Desktop's entry names that transcript.
#[test]
fn desktop_transcript_is_listed_only_under_desktops_entry() {
    let sb = Sandbox::new();
    let a1 = sb.partition(0xA, 0xA1);
    let b1 = sb.partition(0xB, 0xB1);
    let (w, x1, x2) = (uuid(0x101), uuid(0x201), uuid(0x202));
    sb.write_desktop_transcript(&x1);

    sb.sync(&[]);

    for partition in [&a1, &b1] {
        assert!(
            wrapper_ids(partition).is_empty(),
            "nothing listed before Desktop's entry"
        );
    }

    // W moves to x2 while both partitions list x2 under its own name, as an
    // earlier version of the sync did.
    sb.write_desktop_transcript(&x2);
    for partition in [&a1, &b1] {
        write_wrapper(partition, &sb.wrapper_json(&x2, &x2, &[], CREATED_MS));
    }
    write_wrapper(&a1, &sb.wrapper_json(&w, &x2, &[&x1], CREATED_MS + HOUR_MS));

    sb.sync(&[]);

    for partition in [&a1, &b1] {
        assert_eq!(wrapper_ids(partition), set(&[&w]), "only Desktop's entry");
        assert_eq!(cli_session_id(&wrapper_path(partition, &w)), x2);
    }
}

/// Contract 6: once every transcript of a conversation is gone, the copies
/// the tool wrote are removed, but Desktop's own wrappers and a copy someone
/// edited stay. Losing only an earlier transcript removes nothing (contract 1).
#[test]
fn removal_deletes_only_unchanged_tool_copies() {
    let sb = Sandbox::new();
    let a1 = sb.partition(0xA, 0xA1);
    let b1 = sb.partition(0xB, 0xB1);
    let c1 = sb.partition(0xC, 0xC1);
    let e1 = sb.partition(0xE, 0xE1);
    let (w, x1, x2) = (uuid(0x101), uuid(0x201), uuid(0x202));
    let x1_path = sb.write_transcript(&x1);
    let x2_path = sb.write_transcript(&x2);
    // The contract is silent on a projects root with no transcripts at all,
    // which the tool treats as unverifiable and skips removals for. Another
    // conversation's transcript keeps this fixture to the case the contract
    // describes: W's transcripts are gone while the projects root is readable.
    sb.write_transcript(&uuid(0x301));
    let a_original = write_wrapper(&a1, &sb.wrapper_json(&w, &x2, &[&x1], CREATED_MS + HOUR_MS));
    // Account E's Desktop lists W on its own; that file is Desktop's, not a copy.
    let e_original = write_wrapper(&e1, &sb.wrapper_json(&w, &x2, &[&x1], CREATED_MS + MINUTE_MS));
    sb.sync(&[]);
    for partition in [&b1, &c1] {
        assert!(wrapper_path(partition, &w).exists(), "precondition: copy of W");
    }
    let b_edited = edit_title(&wrapper_path(&b1, &w));

    fs::remove_file(&x1_path).expect("remove earlier transcript");
    sb.sync(&[]);
    assert!(wrapper_path(&c1, &w).exists(), "contract 1: W still has its current transcript");

    fs::remove_file(&x2_path).expect("remove current transcript");
    sb.sync(&[]);

    assert!(!wrapper_path(&c1, &w).exists(), "contract 6: unchanged copy is removed");
    assert_eq!(read_bytes(&wrapper_path(&b1, &w)), b_edited, "contract 6: edited copy stays");
    assert_eq!(read_bytes(&wrapper_path(&a1, &w)), a_original, "contract 6: Desktop's original stays");
    assert_eq!(read_bytes(&wrapper_path(&e1, &w)), e_original, "contract 2: Desktop's own entry stays");
}

/// Contract 7: a run right after another, with nothing changed in between,
/// leaves the Desktop tree identical in names and bytes.
#[test]
fn second_run_without_changes_changes_nothing() {
    let sb = Sandbox::new();
    let a1 = sb.partition(0xA, 0xA1);
    let a2 = sb.partition(0xA, 0xA2);
    let b1 = sb.partition(0xB, 0xB1);
    let c1 = sb.partition(0xC, 0xC1);
    let (w, xw) = (uuid(0x101), uuid(0x201));
    let (v, xv1, xv2) = (uuid(0x102), uuid(0x202), uuid(0x203));
    let (orphan, x_orphan) = (uuid(0x103), uuid(0x204));
    let y = uuid(0x301);
    for cli in [&xw, &xv1, &xv2, &y] {
        sb.write_transcript(cli);
    }
    // W is listed by two accounts' Desktops with different activity times.
    write_wrapper(&a1, &sb.wrapper_json(&w, &xw, &[], CREATED_MS + HOUR_MS));
    write_wrapper(&b1, &sb.wrapper_json(&w, &xw, &[], CREATED_MS + MINUTE_MS));
    write_wrapper(&c1, &sb.wrapper_json(&v, &xv2, &[&xv1], CREATED_MS + MINUTE_MS));
    write_wrapper(&b1, &sb.wrapper_json(&orphan, &x_orphan, &[], CREATED_MS));
    write_marker(&a2, &format!("deleted_local_{y}"));

    sb.sync(&[]);
    let settled = tree(&sb.desktop);
    sb.sync(&[]);

    let changed = changed_paths(&settled, &tree(&sb.desktop));
    assert!(changed.is_empty(), "contract 7: second run changed {changed:?}");
}

/// Contract 8 and the command's "`--dry-run` writes nothing": a dry run with
/// adds, a refresh, and a removal pending leaves the Desktop tree and the
/// tool's state directory as they were. The real runs after each dry run show
/// that there was work to skip.
#[test]
fn dry_run_writes_nothing() {
    let sb = Sandbox::new();
    let a1 = sb.partition(0xA, 0xA1);
    let b1 = sb.partition(0xB, 0xB1);
    let (w, x1, x2) = (uuid(0x101), uuid(0x201), uuid(0x202));
    let y = uuid(0x301);
    sb.write_transcript(&x1);
    write_wrapper(&a1, &sb.wrapper_json(&w, &x1, &[], CREATED_MS + MINUTE_MS));
    let y_path = sb.write_transcript(&y);

    let (desktop, state) = (tree(&sb.desktop), tree(&sb.state));
    sb.dry_run();
    assert!(changed_paths(&desktop, &tree(&sb.desktop)).is_empty(), "contract 8: dry run with adds pending");
    assert!(changed_paths(&state, &tree(&sb.state)).is_empty(), "dry run wrote tool state");
    sb.sync(&[]);
    assert!(wrapper_path(&b1, &w).exists() && wrapper_path(&b1, &y).exists(), "adds were pending");

    // A refresh (W moved) and a removal (Y's only transcript gone) are pending.
    sb.write_transcript(&x2);
    write_wrapper(&a1, &sb.wrapper_json(&w, &x2, &[&x1], CREATED_MS + HOUR_MS));
    fs::remove_file(&y_path).expect("remove transcript");
    let (desktop, state) = (tree(&sb.desktop), tree(&sb.state));
    sb.dry_run();
    assert!(changed_paths(&desktop, &tree(&sb.desktop)).is_empty(), "contract 8: dry run with refresh and removal pending");
    assert!(changed_paths(&state, &tree(&sb.state)).is_empty(), "dry run wrote tool state");
    sb.sync(&[]);
    assert_eq!(cli_session_id(&wrapper_path(&b1, &w)), x2, "refresh was pending");
    assert!(!wrapper_path(&b1, &y).exists(), "removal was pending");
}

/// Contract 9: the only files a run adds under the Desktop root are wrappers in
/// real partition directories with lowercase canonical UUID names. Look-alike
/// directories, a symbolic link standing in for a partition, and Desktop's
/// other data are left alone, and no temporary file remains.
#[cfg(unix)]
#[test]
fn writes_only_wrappers_inside_real_partitions() {
    let sb = Sandbox::new();
    let a1 = sb.partition(0xA, 0xA1);
    let b1 = sb.partition(0xB, 0xB1);
    let sessions = sb.desktop.join("claude-code-sessions");
    let outside = sb.root.join("outside");
    for dir in [
        sessions.join(uuid(0xC).to_uppercase()).join(uuid(0xC1)),
        sessions.join(uuid(0xD)).join("not-a-uuid"),
        sessions.join(uuid(0xE)),
        outside.clone(),
    ] {
        fs::create_dir_all(dir).expect("create look-alike directory");
    }
    std::os::unix::fs::symlink(&outside, sessions.join(uuid(0xE)).join(uuid(0xE1))).expect("symlink");
    write_file(&sessions.join(uuid(0xF)).join(uuid(0xF1)), b"a file, not a partition");
    write_file(&sb.desktop.join("config.json"), b"{}");
    write_file(&sb.desktop.join("Local Storage").join("leveldb").join("000003.log"), b"\x00\x01");
    let (w, x1, y) = (uuid(0x101), uuid(0x201), uuid(0x301));
    sb.write_transcript(&x1);
    write_wrapper(&a1, &sb.wrapper_json(&w, &x1, &[], CREATED_MS + MINUTE_MS));
    sb.write_transcript(&y);
    let before = tree(&sb.desktop);

    sb.sync(&[]);

    let added = changed_paths(&before, &tree(&sb.desktop));
    let mut expected: Vec<String> = [wrapper_path(&b1, &w), wrapper_path(&a1, &y), wrapper_path(&b1, &y)]
        .iter()
        .map(|path| sb.desktop_relative(path))
        .collect();
    expected.sort();
    assert_eq!(added, expected, "contract 9: paths added under the Desktop root");
    let through_link = fs::read_dir(&outside).expect("read outside").count();
    assert_eq!(through_link, 0, "contract 9: wrote through a symbolic link posing as a partition");
}
