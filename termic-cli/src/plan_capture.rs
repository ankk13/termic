//! `termic capture-plan`: the Claude Code hook body behind plan capture.
//!
//! Claude's plan mode writes the plan to the GLOBAL `~/.claude/plans/<slug>.md`,
//! which is outside the worktree and so invisible to termic's file tree. The
//! app injects a hook that pipes the hook payload here (see
//! `src-tauri/src/plan_hook.rs`), and we drop the plan into the task's
//! `.context/plans/`.
//!
//! Payload shapes, captured from a real claude v2.1.226 session:
//!
//! ```text
//! PreToolUse   tool_input:    { plan, planFilePath }   <- before the prompt
//! PostToolUse  tool_input:    {}                       <- EMPTY, note
//!              tool_response: { plan, filePath, isAgent }
//! ```
//!
//! `PreToolUse` is the one that matters: it fires BEFORE the approval prompt
//! renders, which is the whole point (you want to read the plan while
//! deciding). `PostToolUse` only fires on approval and is kept as an
//! idempotent confirm, which the byte-equality check turns into a no-op.
//!
//! Everything here is best-effort and SILENT. A hook that writes to stdout or
//! exits non-zero can disturb the agent's turn (`PreToolUse` stdout can even
//! carry permission decisions), so every failure path just returns.

use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

/// How many leading lines of an existing capture we search for our markers.
/// The header we write is 3 lines; a few extra absorbs manual edits.
const HEADER_SCAN_LINES: usize = 6;

/// Directory-scan ceiling. `.context/plans/` holding more than this is not a
/// plan folder any more, and we would rather mint a new file than stat a
/// pathological directory on the agent's hot path.
const MAX_SCAN_ENTRIES: usize = 200;

/// Refuse absurd payloads rather than buffering them.
const MAX_STDIN_BYTES: u64 = 4 * 1024 * 1024;

/// The plan markdown: `tool_input.plan` (PreToolUse) then
/// `tool_response.plan` (PostToolUse, whose `tool_input` is empty).
pub fn extract_plan(payload: &Value) -> Option<String> {
    for path in [["tool_input", "plan"], ["tool_response", "plan"]] {
        if let Some(s) = payload[path[0]][path[1]].as_str() {
            if !s.trim().is_empty() {
                return Some(s.to_string());
            }
        }
    }
    None
}

/// Claude's own plan file for this session. Doubles as an identity key that
/// survives a session-id change (see `markers`).
pub fn extract_source_path(payload: &Value) -> Option<String> {
    for path in [
        ["tool_input", "planFilePath"],
        ["tool_response", "filePath"],
    ] {
        if let Some(s) = payload[path[0]][path[1]].as_str() {
            if !s.trim().is_empty() {
                return Some(s.to_string());
            }
        }
    }
    None
}

/// Session id: the payload's, else the one termic injected, else a constant.
pub fn extract_session_id(payload: &Value) -> String {
    payload["session_id"]
        .as_str()
        .map(|s| s.to_string())
        .filter(|s| !s.trim().is_empty())
        .or_else(|| std::env::var("TERMIC_SESSION_ID").ok().filter(|s| !s.is_empty()))
        .unwrap_or_else(|| "unknown".to_string())
}

/// First `# ` heading, else the first non-empty line. Used only to name the
/// file on its FIRST capture; revisions never re-derive it.
pub fn plan_title(md: &str) -> Option<String> {
    let mut first_non_empty = None;
    for line in md.lines() {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        if let Some(h) = t.strip_prefix("# ") {
            let h = h.trim();
            if !h.is_empty() {
                return Some(h.to_string());
            }
        }
        if first_non_empty.is_none() {
            first_non_empty = Some(t.to_string());
        }
    }
    first_non_empty
}

/// Lowercase kebab slug, capped so the filename stays readable.
pub fn slugify(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
        if out.len() >= 60 {
            break;
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "plan".to_string()
    } else {
        trimmed
    }
}

/// Identity markers, written into the capture's header and matched on the
/// next run so a revision UPDATES the same file instead of forking a new one.
///
/// Two independent keys, either sufficient:
///  - session: the ordinary reject-revise-resubmit loop, same session throughout.
///  - source path: survives a session-id change, which is what happens when
///    the task is relaunched and claude resumes with the same plan file.
pub fn markers(session_id: &str, source_path: Option<&str>) -> Vec<String> {
    let mut m = vec![format!("<!-- termic-session: {session_id} -->")];
    if let Some(p) = source_path {
        m.push(format!("<!-- termic-plan-source: {p} -->"));
    }
    m
}

/// The file this capture belongs in: an existing capture whose header carries
/// any of our markers, else a fresh `<timestamp>_<slug>.md`.
///
/// Returning the EXISTING path is what pins the filename: a revision that
/// changes the plan's title must not rename or duplicate the file.
pub fn target_file(dir: &Path, markers: &[String], title: Option<&str>, stamp: &str) -> PathBuf {
    if let Ok(entries) = fs::read_dir(dir) {
        // Sorted, because `read_dir` order is unspecified: without this both
        // which file a marker matches and what the cap cuts off would vary
        // between runs and between filesystems.
        let mut paths: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().map(|e| e == "md").unwrap_or(false))
            .collect();
        paths.sort();
        for path in paths.into_iter().take(MAX_SCAN_ENTRIES) {
            let Ok(content) = fs::read_to_string(&path) else { continue };
            let header: String = content.lines().take(HEADER_SCAN_LINES).collect::<Vec<_>>().join("\n");
            if markers.iter().any(|m| header.contains(m.as_str())) {
                return path;
            }
        }
    }
    let slug = slugify(title.unwrap_or("plan"));
    dir.join(format!("{stamp}_{slug}.md"))
}

/// Compact UTC stamp for a fresh filename, e.g. `20260808T203612Z`.
///
/// Hand-rolled from the epoch because the CLI links no time crate on purpose
/// (it must stay milliseconds-fast and dependency-light).
pub fn utc_stamp(epoch_secs: u64) -> String {
    let (y, mo, d, h, mi, s) = civil_from_epoch(epoch_secs);
    format!("{y:04}{mo:02}{d:02}T{h:02}{mi:02}{s:02}Z")
}

/// Days-from-epoch to civil date, Howard Hinnant's algorithm.
fn civil_from_epoch(epoch_secs: u64) -> (i64, u32, u32, u32, u32, u32) {
    let days = (epoch_secs / 86_400) as i64;
    let rem = epoch_secs % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (
        y,
        m,
        d,
        (rem / 3600) as u32,
        ((rem % 3600) / 60) as u32,
        (rem % 60) as u32,
    )
}

/// The file we write: marker header, then the plan verbatim.
///
/// Deliberately carries NO capture timestamp. The filename already stamps the
/// first capture and the mtime records the latest, whereas a time in the body
/// would make every re-render differ, so the PostToolUse confirm could never
/// be recognised as a no-op and would churn the file on every approval.
pub fn render(plan: &str, session_id: &str, source_path: Option<&str>, task: Option<&str>) -> String {
    let mut out = String::new();
    for m in markers(session_id, source_path) {
        out.push_str(&m);
        out.push('\n');
    }
    if let Some(t) = task {
        out.push_str(&format!("<!-- termic-task: {t} -->\n"));
    }
    out.push('\n');
    out.push_str(plan.trim_end());
    out.push('\n');
    out
}

/// Where captures go: `$TERMIC_CONTEXT_DIR/plans`. The app creates it on
/// every agent spawn (`ensure_context_dirs`), so a missing dir means this is
/// not a termic agent PTY and we have no business writing anything.
fn plans_dir() -> Option<PathBuf> {
    let base = std::env::var("TERMIC_CONTEXT_DIR").ok().filter(|s| !s.is_empty())?;
    let dir = PathBuf::from(base).join("plans");
    dir.is_dir().then_some(dir)
}

fn now_epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Read the hook payload from stdin and capture the plan. Never fails, never
/// prints: see the module note on why silence is mandatory.
pub fn run() {
    let _ = std::panic::catch_unwind(capture);
}

fn capture() {
    use std::io::Read as _;
    let mut raw = String::new();
    if std::io::stdin()
        .take(MAX_STDIN_BYTES)
        .read_to_string(&mut raw)
        .is_err()
    {
        return;
    }
    let Ok(payload) = serde_json::from_str::<Value>(&raw) else { return };
    let Some(dir) = plans_dir() else { return };
    let task = std::env::var("TERMIC_TASK_ID").ok().filter(|s| !s.is_empty());
    capture_into(&payload, &dir, task.as_deref(), now_epoch_secs());
}

/// The whole capture with its inputs handed in, so it can be tested without
/// stdin or environment. Returns the path written, or `None` when there was
/// nothing to do (no plan in the payload, or the file already matches).
pub fn capture_into(
    payload: &Value,
    dir: &Path,
    task: Option<&str>,
    now: u64,
) -> Option<PathBuf> {
    let plan = extract_plan(payload)?;
    let source = extract_source_path(payload);
    let session = extract_session_id(payload);

    let path = target_file(
        dir,
        &markers(&session, source.as_deref()),
        plan_title(&plan).as_deref(),
        &utc_stamp(now),
    );
    let body = render(&plan, &session, source.as_deref(), task);

    // Skip an identical rewrite so the PostToolUse confirm does not churn the
    // mtime and set off the file tree's refresh for nothing.
    if fs::read_to_string(&path).map(|prior| prior == body).unwrap_or(false) {
        return None;
    }
    fs::write(&path, body).ok()?;
    Some(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Unique scratch dir. `tempfile` is deliberately not a dependency: the
    /// CLI's dep list is kept short on purpose.
    fn scratch(tag: &str) -> PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "termic-plan-{}-{}-{}",
            tag,
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn pre_payload() -> Value {
        serde_json::json!({
            "session_id": "fba986d7",
            "hook_event_name": "PreToolUse",
            "tool_name": "ExitPlanMode",
            "tool_input": {
                "plan": "# Change a.txt to say goodbye\n\nEdit a.txt.",
                "planFilePath": "/Users/x/.claude/plans/cached-peach.md",
            },
        })
    }

    fn post_payload() -> Value {
        serde_json::json!({
            "session_id": "fba986d7",
            "hook_event_name": "PostToolUse",
            "tool_name": "ExitPlanMode",
            "tool_input": {},
            "tool_response": {
                "plan": "# Change a.txt to say goodbye\n\nEdit a.txt.",
                "filePath": "/Users/x/.claude/plans/cached-peach.md",
                "isAgent": false,
            },
        })
    }

    #[test]
    fn reads_plan_from_both_events() {
        assert!(extract_plan(&pre_payload()).unwrap().starts_with("# Change"));
        assert!(extract_plan(&post_payload()).unwrap().starts_with("# Change"));
    }

    #[test]
    fn rejects_missing_and_blank_plans() {
        assert!(extract_plan(&serde_json::json!({})).is_none());
        assert!(extract_plan(&serde_json::json!({"tool_input": {"plan": "   \n"}})).is_none());
    }

    #[test]
    fn reads_source_path_from_both_events() {
        let want = "/Users/x/.claude/plans/cached-peach.md";
        assert_eq!(extract_source_path(&pre_payload()).unwrap(), want);
        assert_eq!(extract_source_path(&post_payload()).unwrap(), want);
        assert!(extract_source_path(&serde_json::json!({})).is_none());
    }

    #[test]
    fn titles_prefer_the_h1() {
        assert_eq!(plan_title("# Hello\n\nbody").unwrap(), "Hello");
        assert_eq!(plan_title("\n\nplain line\n# Later").unwrap(), "Later");
        assert_eq!(plan_title("just text").unwrap(), "just text");
        assert!(plan_title("   \n\n  ").is_none());
    }

    #[test]
    fn slugs_are_kebab_and_bounded() {
        assert_eq!(slugify("Change a.txt to say goodbye"), "change-a-txt-to-say-goodbye");
        assert_eq!(slugify("!!!"), "plan");
        assert_eq!(slugify(""), "plan");
        assert!(slugify(&"x".repeat(200)).len() <= 60);
    }

    #[test]
    fn stamps_are_utc() {
        // 2026-08-08T20:36:12Z
        assert_eq!(utc_stamp(1_786_221_372), "20260808T203612Z");
    }

    #[test]
    fn mints_a_fresh_name_when_nothing_matches() {
        let dir = scratch("fresh");
        let p = target_file(&dir, &markers("s1", None), Some("My Plan"), "20260808T000000Z");
        assert_eq!(p.file_name().unwrap(), "20260808T000000Z_my-plan.md");
    }

    #[test]
    fn reuses_the_file_matching_the_session_marker() {
        let dir = scratch("session");
        let existing = dir.join("20260101T000000Z_old-title.md");
        fs::write(&existing, render("# Old", "s1", None, None)).unwrap();
        let p = target_file(&dir, &markers("s1", None), Some("Brand New Title"), "20260808T000000Z");
        assert_eq!(p, existing, "a retitled revision must not fork a new file");
    }

    #[test]
    fn reuses_the_file_matching_the_source_marker_across_sessions() {
        let dir = scratch("source");
        let src = "/Users/x/.claude/plans/cached-peach.md";
        let existing = dir.join("20260101T000000Z_old.md");
        fs::write(&existing, render("# Old", "s1", Some(src), None)).unwrap();
        // Relaunched task: new session id, same plan file.
        let p = target_file(&dir, &markers("s2", Some(src)), Some("Old"), "20260808T000000Z");
        assert_eq!(p, existing);
    }

    #[test]
    fn an_unrelated_plan_gets_its_own_file() {
        let dir = scratch("unrelated");
        fs::write(
            dir.join("20260101T000000Z_old.md"),
            render("# Old", "s1", Some("/a.md"), None),
        )
        .unwrap();
        let p = target_file(&dir, &markers("s2", Some("/b.md")), Some("New"), "20260808T000000Z");
        assert_eq!(p.file_name().unwrap(), "20260808T000000Z_new.md");
    }

    #[test]
    fn bails_to_a_fresh_name_past_the_scan_cap() {
        let dir = scratch("cap");
        // Names sort before the marker-bearing one, so it falls past the cap.
        for i in 0..MAX_SCAN_ENTRIES + 5 {
            fs::write(dir.join(format!("filler-{i:04}.md")), "filler\n").unwrap();
        }
        fs::write(dir.join("zzz-real.md"), render("# Old", "s1", None, None)).unwrap();
        let p = target_file(&dir, &markers("s1", None), Some("T"), "20260808T000000Z");
        assert_eq!(p.file_name().unwrap(), "20260808T000000Z_t.md");
    }

    #[test]
    fn rendered_body_carries_markers_and_plan() {
        let out = render("# Hi\n\nbody\n\n\n", "s1", Some("/p.md"), Some("w7"));
        assert!(out.contains("<!-- termic-session: s1 -->"));
        assert!(out.contains("<!-- termic-plan-source: /p.md -->"));
        assert!(out.contains("<!-- termic-task: w7 -->"));
        assert!(out.ends_with("# Hi\n\nbody\n"), "{out:?}");
    }

    #[test]
    fn pre_writes_and_the_matching_post_is_a_no_op() {
        let dir = scratch("prepost");
        let written = capture_into(&pre_payload(), &dir, Some("w7"), 0).unwrap();
        assert!(written.file_name().unwrap().to_string_lossy().ends_with("_change-a-txt-to-say-goodbye.md"));
        let after_pre = fs::read_to_string(&written).unwrap();
        assert!(after_pre.contains("Edit a.txt."));

        // Approval fires PostToolUse with the same plan: same file, no rewrite.
        assert!(capture_into(&post_payload(), &dir, Some("w7"), 999).is_none());
        assert_eq!(fs::read_to_string(&written).unwrap(), after_pre);
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
    }

    #[test]
    fn a_revised_plan_updates_the_original_capture() {
        let dir = scratch("revised");
        let first = capture_into(&pre_payload(), &dir, None, 0).unwrap();

        // User comments, claude revises: same session, same plan file, new title.
        let mut revised = pre_payload();
        revised["tool_input"]["plan"] =
            serde_json::json!("# Change a.txt, take two\n\nEdit a.txt properly.");
        let second = capture_into(&revised, &dir, None, 5_000).unwrap();

        assert_eq!(first, second, "a revision must update the same file");
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        assert!(fs::read_to_string(&second).unwrap().contains("take two"));
    }

    #[test]
    fn payloads_with_nothing_to_capture_are_ignored() {
        let dir = scratch("empty");
        for payload in [
            serde_json::json!({}),
            serde_json::json!({"tool_input": {"plan": "  "}}),
            serde_json::json!({"tool_name": "Write", "tool_input": {"file_path": "/x"}}),
        ] {
            assert!(capture_into(&payload, &dir, None, 0).is_none());
        }
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
    }

    #[test]
    fn a_revision_overwrites_the_same_file() {
        let dir = scratch("revise");
        let markers_v = markers("s1", Some("/p.md"));

        let first = render("# A\n\nfirst", "s1", Some("/p.md"), None);
        let p1 = target_file(&dir, &markers_v, Some("A"), "20260808T000000Z");
        fs::write(&p1, &first).unwrap();

        // The revised plan even changes its title; the path must not move.
        let second = render("# A revised\n\nsecond", "s1", Some("/p.md"), None);
        let p2 = target_file(&dir, &markers_v, Some("A revised"), "20260808T111111Z");
        fs::write(&p2, &second).unwrap();

        assert_eq!(p1, p2);
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        assert!(fs::read_to_string(&p2).unwrap().contains("second"));
    }
}
