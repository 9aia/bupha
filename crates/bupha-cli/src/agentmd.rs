//! The managed `AGENTS.md` agent-awareness block — `init` writes
//! it into the scaffolded repo, `doctor` validates it across the
//! watched fleet.
//!
//! The block is marker-delimited so human prose elsewhere in the
//! file survives syncs; a stale or hand-written `Bupha awareness`
//! section is upgraded in place, everything else is untouched.
//! Same contract shape as myco's and sturn's blocks — each tool
//! owns its own marker pair and canonical text.

use std::path::Path;

/// Marker pair delimiting the bupha-managed block.
pub const BEGIN: &str = "<!-- bupha:agent-awareness -->";
/// Closing marker.
pub const END: &str = "<!-- /bupha:agent-awareness -->";

/// Canonical managed section, between the markers — the repo
/// contract every bupha-gated repo carries.
const SECTION: &str = "\
## Bupha awareness

- This repo is gated by bupha — pushes to the watched branch run
  the configured pipeline in a detached worktree; keep it green.
- Gate pipelines should run `myco check` — ledger hygiene is part
  of CI, not a manual step.
- Commits that move a ledger item carry an `Item: T-NNN` trailer
  naming the myco item — runs and history stay attributable.
- `bupha init <dir>` scaffolds the gate and writes this block;
  `bupha doctor` validates it.
";

/// The canonical emitted block, markers included.
fn canonical() -> String {
    format!("{BEGIN}\n\n{SECTION}\n{END}\n")
}

/// Where the managed block stands in a repo root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// No `AGENTS.md` at all.
    Missing,
    /// File exists, nothing agent-awareness-shaped in it.
    Absent,
    /// A block or `Bupha awareness` section exists but isn't the
    /// canonical marked form — `sync` repairs it.
    Drift,
    /// Canonical.
    Ok,
}

impl State {
    /// Stable label for JSON output.
    pub fn as_str(self) -> &'static str {
        match self {
            State::Missing => "missing",
            State::Absent => "absent",
            State::Drift => "drift",
            State::Ok => "ok",
        }
    }
}

/// What a `sync` did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// `AGENTS.md` was created.
    Created,
    /// The block was inserted or rewritten in an existing file.
    Updated,
    /// Already canonical — nothing written.
    Unchanged,
}

/// ATX heading level for a line (`## foo` → 2), else 0.
fn heading_level(line: &str) -> usize {
    let t = line.trim_start();
    let n = t.bytes().take_while(|b| *b == b'#').count();
    if n > 0 && t[n..].starts_with(char::is_whitespace) {
        n
    } else {
        0
    }
}

/// Heading text for a line, if it's an ATX heading.
fn heading_text(line: &str) -> Option<&str> {
    let n = heading_level(line);
    (n > 0).then(|| line.trim_start()[n..].trim())
}

/// Index of the `Bupha awareness` heading line (any level).
fn section_start(lines: &[String]) -> Option<usize> {
    lines
        .iter()
        .position(|l| heading_text(l).is_some_and(|t| t.eq_ignore_ascii_case("bupha awareness")))
}

/// Merge the canonical block into `src`:
/// - a marked block is rewritten in place;
/// - an unmarked `Bupha awareness` section is replaced (upgrade);
/// - stray markers are dropped before either rule runs;
/// - otherwise the block is appended.
///
/// Emission is deterministic: exactly one blank line on either
/// side of the block, file ends with a single newline.
fn merged(src: &str) -> String {
    let canon: Vec<String> = canonical()
        .trim_end_matches('\n')
        .split('\n')
        .map(str::to_string)
        .collect();
    let mut lines: Vec<String> = src.lines().map(str::to_string).collect();

    let begin = lines.iter().position(|l| l.trim() == BEGIN);
    let end = begin.and_then(|b| {
        lines
            .iter()
            .enumerate()
            .skip(b + 1)
            .find(|(_, l)| l.trim() == END)
            .map(|(i, _)| i)
    });

    // [..h) head prose, [h..e) replaced span, [e..) tail prose
    let (h, e) = match (begin, end) {
        (Some(b), Some(e)) => (b, e + 1),
        _ => {
            // malformed or absent markers: drop strays, then try a
            // hand-written section
            lines.retain(|l| {
                let t = l.trim();
                t != BEGIN && t != END
            });
            match section_start(&lines) {
                Some(s) => {
                    let lvl = heading_level(&lines[s]);
                    let e = lines[s + 1..]
                        .iter()
                        .position(|l| {
                            let n = heading_level(l);
                            n > 0 && n <= lvl
                        })
                        .map_or(lines.len(), |p| s + 1 + p);
                    (s, e)
                }
                None => (lines.len(), lines.len()),
            }
        }
    };

    let mut out: Vec<String> = lines[..h].to_vec();
    while out.last().is_some_and(|l| l.trim().is_empty()) {
        out.pop();
    }
    if !out.is_empty() {
        out.push(String::new());
    }
    out.extend(canon);
    let tail = lines[e..]
        .iter()
        .skip_while(|l| l.trim().is_empty())
        .cloned()
        .collect::<Vec<_>>();
    if !tail.is_empty() {
        out.push(String::new());
        out.extend(tail);
    }
    // Canonical emission ends with exactly one newline — trailing
    // blank lines are dropped so `block_state` and `sync` agree on
    // what "canonical" means (a file ending in "\n\n" would
    // otherwise report drift that no sync can clear).
    while out.last().is_some_and(|l| l.trim().is_empty()) {
        out.pop();
    }
    format!("{}\n", out.join("\n"))
}

/// Inspect `<root>/AGENTS.md` — cheap, no writes.
pub fn block_state(root: &Path) -> State {
    let path = root.join("AGENTS.md");
    let Ok(src) = std::fs::read_to_string(&path) else {
        return State::Missing;
    };
    // canonical iff emitting it is a no-op — the same predicate
    // `sync` uses, so "canonical" and "would change" can never
    // disagree
    if src.contains(BEGIN) && merged(&src) == src {
        return State::Ok;
    }
    let lines: Vec<String> = src.lines().map(str::to_string).collect();
    let has_marker = lines.iter().any(|l| {
        let t = l.trim();
        t == BEGIN || t == END
    });
    let has_section = section_start(&lines).is_some();
    if has_marker || has_section {
        State::Drift
    } else {
        State::Absent
    }
}

/// Write the canonical block into `<root>/AGENTS.md`.
pub fn sync(root: &Path) -> Result<Change, String> {
    let path = root.join("AGENTS.md");
    let existed = path.exists();
    let src = std::fs::read_to_string(&path).unwrap_or_default();
    let next = merged(&src);
    if existed && next == src {
        return Ok(Change::Unchanged);
    }
    std::fs::write(&path, &next).map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(if existed {
        Change::Updated
    } else {
        Change::Created
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("bupha-agentmd-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn missing_file_created_then_unchanged() {
        let d = scratch("missing");
        assert_eq!(block_state(&d), State::Missing);
        assert_eq!(sync(&d).unwrap(), Change::Created);
        assert_eq!(
            std::fs::read_to_string(d.join("AGENTS.md")).unwrap(),
            canonical()
        );
        assert_eq!(block_state(&d), State::Ok);
        assert_eq!(sync(&d).unwrap(), Change::Unchanged);
    }

    #[test]
    fn appends_to_existing_prose() {
        let d = scratch("append");
        std::fs::write(d.join("AGENTS.md"), "# Repo\n\nRules here.\n").unwrap();
        assert_eq!(block_state(&d), State::Absent);
        assert_eq!(sync(&d).unwrap(), Change::Updated);
        let out = std::fs::read_to_string(d.join("AGENTS.md")).unwrap();
        assert!(out.starts_with("# Repo\n\nRules here.\n\n"));
        assert!(out.contains(BEGIN));
        assert!(out.ends_with(&format!("{END}\n")));
        assert_eq!(block_state(&d), State::Ok);
        assert_eq!(sync(&d).unwrap(), Change::Unchanged);
    }

    #[test]
    fn rewrites_drifted_marked_block() {
        let d = scratch("drift");
        let bad = canonical().replace("myco check", "cargo check");
        std::fs::write(d.join("AGENTS.md"), &bad).unwrap();
        assert_eq!(block_state(&d), State::Drift);
        assert_eq!(sync(&d).unwrap(), Change::Updated);
        assert_eq!(
            std::fs::read_to_string(d.join("AGENTS.md")).unwrap(),
            canonical()
        );
        assert_eq!(block_state(&d), State::Ok);
    }

    #[test]
    fn keeps_tail_prose_after_marked_block() {
        let d = scratch("tail");
        std::fs::write(
            d.join("AGENTS.md"),
            format!("# R\n\n{BEGIN}\n\nold\n\n{END}\n\n\n## Layout\n\nstuff\n\n\n"),
        )
        .unwrap();
        assert_eq!(block_state(&d), State::Drift);
        sync(&d).unwrap();
        let out = std::fs::read_to_string(d.join("AGENTS.md")).unwrap();
        assert!(out.starts_with("# R\n"));
        assert!(out.ends_with("## Layout\n\nstuff\n"));
        assert!(!out.contains("\nold\n"));
        assert_eq!(block_state(&d), State::Ok);
        assert_eq!(sync(&d).unwrap(), Change::Unchanged);
    }

    #[test]
    fn upgrades_handwritten_section() {
        let d = scratch("handwritten");
        std::fs::write(
            d.join("AGENTS.md"),
            "# Repo\n\n## Bupha awareness\n\n- custom bullet\n\n## Layout\n\nstuff\n",
        )
        .unwrap();
        assert_eq!(block_state(&d), State::Drift);
        sync(&d).unwrap();
        let out = std::fs::read_to_string(d.join("AGENTS.md")).unwrap();
        assert!(!out.contains("custom bullet"));
        assert!(out.contains("## Layout"));
        assert_eq!(block_state(&d), State::Ok);
    }

    #[test]
    fn drops_stray_markers() {
        let d = scratch("stray");
        std::fs::write(d.join("AGENTS.md"), format!("# R\n\n{BEGIN}\n\nstuff\n")).unwrap();
        sync(&d).unwrap();
        let out = std::fs::read_to_string(d.join("AGENTS.md")).unwrap();
        assert_eq!(out.matches(BEGIN).count(), 1);
        assert_eq!(block_state(&d), State::Ok);
    }

    #[test]
    fn trailing_blank_line_is_drift_and_sync_fixes_it() {
        let d = scratch("trailblanks");
        sync(&d).unwrap();
        let p = d.join("AGENTS.md");
        let src = std::fs::read_to_string(&p).unwrap();
        std::fs::write(&p, format!("{src}\n")).unwrap();
        assert_eq!(block_state(&d), State::Drift);
        assert_eq!(sync(&d).unwrap(), Change::Updated);
        assert_eq!(block_state(&d), State::Ok);
        assert_eq!(std::fs::read_to_string(&p).unwrap(), src);
    }

    #[test]
    fn write_error_surfaces() {
        let d = scratch("isdir");
        std::fs::create_dir_all(d.join("AGENTS.md")).unwrap();
        assert!(sync(&d).is_err());
    }
}
