//! Per-conversation persistent history for channel adapters.
//!
//! The WeChat plugin (and any future passive-reply channel) needs each fan
//! to get coherent multi-turn dialog: when "我叫小明" is followed by "我叫
//! 什么", the LLM call must include the prior turn. EvoClaw's
//! `ConversationRuntime` already supports in-memory multi-turn history
//! (see `runtime/mod.rs::history`) but `evo channel run` spins up a fresh
//! runtime per inbound message, so without persistence the in-memory
//! history is born empty and dies empty each turn.
//!
//! `SessionStore` is the gap-filler. Each `conversation_id` maps to a
//! jsonl file under `root/{shard}/{cid}.jsonl` where `shard` is the first
//! two hex chars of `sha1(cid)`. Sharding keeps each directory bounded
//! (256 buckets) so ext4/apfs perform well at 100k+ user counts.
//!
//! ## Concurrency model
//!
//! `SessionStore` is **not** internally locked across cids — it relies on
//! its caller (the WeChat plugin's `ConvSerializer`) to ensure only one
//! in-flight message per cid. Within a single cid the write is atomic via
//! tmp+rename, so a crashing worker can't leave a half-written file.
//! Cross-cid operations are fully parallel.
//!
//! ## Failure handling
//!
//! - Missing file → `Ok(vec![])` (cold start = new user)
//! - Corrupt individual lines → skip with `warn!`; do not lose the whole
//!   file. The next save will rewrite cleanly.
//! - Path traversal in `cid` → reject with `InvalidInput`. Callers are
//!   expected to sanitize too; this is defence in depth.

use evo_providers::Message;
use sha1::{Digest, Sha1};
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Maximum cid length we accept on disk. Keeps filenames well under any
/// reasonable filesystem `NAME_MAX` (typically 255) while leaving room for
/// the `.jsonl` extension and tmp suffixes during atomic write. Plugin
/// builders must enforce the same upper bound so they never produce a
/// cid that this store rejects — see
/// `evoclaw-plugin-wechat/src/wechat/handler.rs::build_conversation_id`.
const MAX_CID_LEN: usize = 256;

/// Cids beginning with this prefix bypass persistence entirely:
/// `load` returns empty, `append_and_truncate` is a no-op, `purge` is a
/// no-op. Used by callers (notably the WeChat plugin's intent
/// classifier) for one-shot LLM calls whose "history" has no
/// conversation continuity to preserve — without this hook those calls
/// would write a fresh per-classification jsonl every time and clutter
/// the session dir indefinitely.
pub const EPHEMERAL_CID_PREFIX: &str = "_ephemeral_";

/// Reusable persistent history store. Cheap to clone — only holds a
/// `PathBuf` and a `usize`. Wrap in `Arc` when sharing across the channel
/// dispatch loop and worker tasks.
#[derive(Debug, Clone)]
pub struct SessionStore {
    root: PathBuf,
    /// Cap on **user/assistant turn pairs** (system prompt + tool messages
    /// do not count). When exceeded, the oldest pairs are dropped before
    /// the file is rewritten.
    max_turns: usize,
}

impl SessionStore {
    /// Create the root directory (mode 0700 on Unix) and return a handle.
    /// Subsequent calls are idempotent: existing dirs and permissions are
    /// left untouched if already correct.
    pub fn new(root: PathBuf, max_turns: usize) -> io::Result<Self> {
        fs::create_dir_all(&root)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // 0700: session files contain user chat content — keep them
            // unreadable to other users on shared hosts. We tighten only;
            // never widen pre-existing permissions.
            let perm = fs::metadata(&root)?.permissions();
            if perm.mode() & 0o077 != 0 {
                fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
            }
        }
        Ok(Self { root, max_turns })
    }

    /// Resolve the absolute jsonl path for `cid`, creating the shard dir
    /// on demand. Rejects unsafe cids before touching the filesystem.
    fn path_for(&self, cid: &str) -> io::Result<PathBuf> {
        validate_cid(cid)?;
        let shard = shard_of(cid);
        let dir = self.root.join(&shard);
        fs::create_dir_all(&dir)?;
        Ok(dir.join(format!("{cid}.jsonl")))
    }

    /// Load existing history. Missing file (cold start) returns `Ok([])`.
    /// Individual malformed lines are skipped (with a `tracing::warn!`)
    /// rather than failing the whole load — one bad line shouldn't lose
    /// the user's entire memory.
    ///
    /// Cids starting with [`EPHEMERAL_CID_PREFIX`] return `Ok(vec![])`
    /// without touching the filesystem.
    pub fn load(&self, cid: &str) -> io::Result<Vec<Message>> {
        if cid.starts_with(EPHEMERAL_CID_PREFIX) {
            return Ok(Vec::new());
        }
        let path = self.path_for(cid)?;
        let file = match File::open(&path) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let reader = BufReader::new(file);
        let mut history = Vec::new();
        for (lineno, line) in reader.lines().enumerate() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<Message>(&line) {
                Ok(msg) => history.push(msg),
                Err(e) => {
                    tracing::warn!(
                        cid = %cid,
                        path = %path.display(),
                        lineno = lineno + 1,
                        error = %e,
                        "session_store: skipping malformed history line"
                    );
                }
            }
        }
        Ok(history)
    }

    /// Replace `cid`'s jsonl with `history`, truncating older user/
    /// assistant turn pairs so at most `max_turns` pairs remain.
    ///
    /// The write is atomic: data is written to a per-write temp file
    /// (suffixed with pid+nanos so concurrent writers in different
    /// processes can't clash), fsynced, then renamed over the target.
    /// POSIX guarantees the rename is atomic, so a crash mid-write leaves
    /// either the old contents or the new — never a half-written file.
    ///
    /// Ephemeral cids (see [`EPHEMERAL_CID_PREFIX`]) are silently
    /// dropped — no file is written.
    pub fn append_and_truncate(&self, cid: &str, history: &[Message]) -> io::Result<()> {
        if cid.starts_with(EPHEMERAL_CID_PREFIX) {
            return Ok(());
        }
        let path = self.path_for(cid)?;
        let trimmed = truncate_to_pairs(history, self.max_turns);
        let mut buf = Vec::with_capacity(trimmed.len() * 128);
        for msg in &trimmed {
            let line = serde_json::to_string(msg).map_err(io::Error::other)?;
            buf.extend_from_slice(line.as_bytes());
            buf.push(b'\n');
        }
        atomic_write(&path, &buf)
    }

    /// Delete a single user's history (e.g. GDPR right-to-forget). No-op
    /// when the file does not exist.
    pub fn purge(&self, cid: &str) -> io::Result<()> {
        if cid.starts_with(EPHEMERAL_CID_PREFIX) {
            return Ok(());
        }
        let path = self.path_for(cid)?;
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }
}

/// First two hex chars of `sha1(cid)` — yields 256 directory buckets so
/// 100k users average ~400 files per dir, well within fs-friendly limits.
fn shard_of(cid: &str) -> String {
    let mut h = Sha1::new();
    h.update(cid.as_bytes());
    let digest = h.finalize();
    format!("{:02x}", digest[0])
}

/// Whitelist cids to `[A-Za-z0-9_-]` and bound length. Rejects path
/// traversal, embedded `..`, slashes, control chars, anything that could
/// escape the shard dir. Plugin layer is expected to sanitize too — this
/// is the second line of defence inside evo-core.
fn validate_cid(cid: &str) -> io::Result<()> {
    if cid.is_empty() || cid.len() > MAX_CID_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("cid length {} out of bounds (1..={MAX_CID_LEN})", cid.len()),
        ));
    }
    for c in cid.chars() {
        if !c.is_ascii_alphanumeric() && c != '_' && c != '-' {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("cid contains forbidden character {c:?}; allowed: [A-Za-z0-9_-]"),
            ));
        }
    }
    Ok(())
}

/// Keep at most `max_pairs` user/assistant pairs from the END of history,
/// always preserving the leading system prompt(s). Tool messages travel
/// with their owning assistant turn.
///
/// Algorithm: walk from the end, count user→assistant transitions, cut
/// once we've kept `max_pairs` pairs. System messages and any pre-pair
/// tail are left in place (they're cheap and lossy truncation in the
/// middle is harder to reason about).
/// Keep at most `max_pairs` user/assistant pairs (counted by Role::User
/// occurrences). When truncating, the leading System message is
/// **preserved** at index 0 so the loaded history is still legal as a
/// runtime starting state — without that the runtime's
/// `if self.history.is_empty()` branch in `runtime/exec.rs` would NOT
/// fire (non-empty after truncation) and the LLM call would proceed
/// without any system prompt at all.
fn truncate_to_pairs(history: &[Message], max_pairs: usize) -> Vec<Message> {
    use evo_providers::Role;
    if max_pairs == 0 || history.is_empty() {
        return history.to_vec();
    }
    let user_positions: Vec<usize> = history
        .iter()
        .enumerate()
        .filter_map(|(i, m)| matches!(m.role, Role::User).then_some(i))
        .collect();
    if user_positions.len() <= max_pairs {
        return history.to_vec();
    }
    let cut_idx = user_positions[user_positions.len() - max_pairs];
    let preserve_sys = matches!(history.first().map(|m| &m.role), Some(Role::System));
    let mut out = Vec::with_capacity(history.len() - cut_idx + usize::from(preserve_sys));
    if preserve_sys && cut_idx > 0 {
        out.push(history[0].clone());
    }
    out.extend_from_slice(&history[cut_idx..]);
    out
}

/// Atomic write: tmp file with unique suffix → fsync → rename.
fn atomic_write(path: &Path, data: &[u8]) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "session_store path has no parent directory",
        )
    })?;
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp = parent.join(format!(
        ".{}.tmp.{}.{}",
        path.file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("session"),
        std::process::id(),
        nanos
    ));
    {
        let mut f = File::create(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
    }
    match fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            // Clean up the temp on rename failure so we don't litter the
            // shard dir. Best-effort; ignore the cleanup error.
            let _ = fs::remove_file(&tmp);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use evo_providers::{Message, Role};
    use std::env;

    fn unique_root(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        env::temp_dir().join(format!("evoclaw-session-{label}-{nanos}"))
    }

    fn store(label: &str) -> (SessionStore, PathBuf) {
        let root = unique_root(label);
        (SessionStore::new(root.clone(), 5).unwrap(), root)
    }

    #[test]
    fn cold_start_returns_empty_history() {
        let (s, _root) = store("cold");
        assert!(s.load("user_a").unwrap().is_empty());
    }

    #[test]
    fn round_trip_user_assistant_pair() {
        let (s, _root) = store("rt");
        let history = vec![
            Message::system("you are helpful"),
            Message::user("hello"),
            Message::assistant("hi there"),
        ];
        s.append_and_truncate("user_b", &history).unwrap();
        let loaded = s.load("user_b").unwrap();
        assert_eq!(loaded.len(), 3);
        assert_eq!(loaded[0].role, Role::System);
        assert_eq!(loaded[1].content, "hello");
        assert_eq!(loaded[2].content, "hi there");
    }

    #[test]
    fn truncation_drops_oldest_pairs() {
        let (s, _root) = store("trunc");
        let mut h: Vec<Message> = vec![Message::system("sys")];
        for i in 0..10 {
            h.push(Message::user(format!("q{i}")));
            h.push(Message::assistant(format!("a{i}")));
        }
        // max_pairs = 5 (from store() helper)
        s.append_and_truncate("user_c", &h).unwrap();
        let loaded = s.load("user_c").unwrap();
        let user_count = loaded.iter().filter(|m| m.role == Role::User).count();
        assert_eq!(user_count, 5, "should keep exactly max_turns user msgs");
        // The newest pair must be present.
        assert!(loaded.iter().any(|m| m.content == "q9"));
        assert!(loaded.iter().any(|m| m.content == "a9"));
        // The oldest must be gone.
        assert!(!loaded.iter().any(|m| m.content == "q0"));
    }

    #[test]
    fn truncation_preserves_leading_system_message() {
        let (s, _root) = store("trunc-sys");
        let mut h: Vec<Message> = vec![Message::system("sys-prompt-original")];
        for i in 0..10 {
            h.push(Message::user(format!("q{i}")));
            h.push(Message::assistant(format!("a{i}")));
        }
        s.append_and_truncate("user_sys", &h).unwrap();
        let loaded = s.load("user_sys").unwrap();
        // The whole point of this fix: without it, the System message
        // would be dropped and the next runtime turn would skip the
        // `if history.is_empty()` system-prompt branch — leaving the
        // LLM call with NO system context at all.
        assert_eq!(
            loaded.first().map(|m| &m.role),
            Some(&Role::System),
            "system message must survive truncation"
        );
        assert_eq!(loaded[0].content, "sys-prompt-original");
    }

    #[test]
    fn ephemeral_cid_skips_load_and_save() {
        let (s, root) = store("eph");
        let cid = format!("{EPHEMERAL_CID_PREFIX}foo-123");
        // Save: must be a no-op (no file created).
        s.append_and_truncate(&cid, &[Message::user("ignore me")])
            .unwrap();
        // Load: must return empty.
        let loaded = s.load(&cid).unwrap();
        assert!(loaded.is_empty());
        // Purge: must be a no-op (Ok even though file doesn't exist).
        s.purge(&cid).unwrap();
        // Confirm no shard dir was created for this cid.
        // (root may have other shards from setup, so we just verify no
        // file matches the ephemeral filename anywhere under root.)
        let mut found = false;
        for entry in walkdir(&root) {
            let p = entry.display().to_string();
            if p.contains(EPHEMERAL_CID_PREFIX) {
                found = true;
                break;
            }
        }
        assert!(!found, "ephemeral cid leaked a file: {root:?}");
    }

    /// Minimal recursive lister for the ephemeral-cid test. Avoids adding
    /// a walkdir dependency for one test.
    fn walkdir(root: &std::path::Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(p) = stack.pop() {
            if let Ok(rd) = std::fs::read_dir(&p) {
                for entry in rd.flatten() {
                    let pp = entry.path();
                    if pp.is_dir() {
                        stack.push(pp);
                    } else {
                        out.push(pp);
                    }
                }
            }
        }
        out
    }

    #[test]
    fn malformed_lines_are_skipped() {
        let (s, root) = store("malformed");
        // Write a file with a mix of good and bad lines manually.
        let path = s.path_for("user_d").unwrap();
        let body = format!(
            "{}\nNOT JSON\n{}\n",
            serde_json::to_string(&Message::user("alpha")).unwrap(),
            serde_json::to_string(&Message::assistant("beta")).unwrap(),
        );
        std::fs::write(&path, body).unwrap();
        let loaded = s.load("user_d").unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].content, "alpha");
        assert_eq!(loaded[1].content, "beta");
        // Make sure the file actually existed where we expected.
        assert!(path.starts_with(&root));
    }

    #[test]
    fn cid_path_traversal_rejected() {
        let (s, _root) = store("trav");
        let err = s.load("../etc/passwd").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn cid_empty_rejected() {
        let (s, _root) = store("empty");
        assert!(s.load("").is_err());
    }

    #[test]
    fn cid_too_long_rejected() {
        let (s, _root) = store("long");
        let cid = "a".repeat(MAX_CID_LEN + 1);
        assert!(s.load(&cid).is_err());
    }

    #[test]
    fn shard_directory_is_consistent_for_same_cid() {
        assert_eq!(shard_of("hello"), shard_of("hello"));
        // Different cids land in different shards more often than not —
        // not strict but sanity check that the hash is being used.
        let a = shard_of("user_a");
        let b = shard_of("user_b");
        // Both should be 2-char lowercase hex.
        assert_eq!(a.len(), 2);
        assert_eq!(b.len(), 2);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn purge_removes_existing_file() {
        let (s, _root) = store("purge");
        s.append_and_truncate("user_e", &[Message::user("x")])
            .unwrap();
        assert_eq!(s.load("user_e").unwrap().len(), 1);
        s.purge("user_e").unwrap();
        assert!(s.load("user_e").unwrap().is_empty());
    }

    #[test]
    fn purge_missing_file_is_ok() {
        let (s, _root) = store("purge-miss");
        s.purge("never_existed").unwrap();
    }

    #[test]
    fn atomic_write_leaves_no_temp_files() {
        let (s, _root) = store("atomic");
        s.append_and_truncate("user_f", &[Message::user("first")])
            .unwrap();
        s.append_and_truncate("user_f", &[Message::user("second")])
            .unwrap();
        // Walk the shard dir and ensure no orphan .tmp.* leftovers.
        let path = s.path_for("user_f").unwrap();
        let dir = path.parent().unwrap();
        for entry in std::fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            assert!(
                !name.contains(".tmp."),
                "unexpected temp file remaining: {name}"
            );
        }
    }
}
