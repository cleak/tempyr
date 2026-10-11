//! Helpers for keeping the project `.gitignore` covering Tempyr's local-only paths.

use std::fs;
use std::path::Path;

/// Interview session archives may contain raw user answers, so they must never
/// be committed. `tempyr init`, `tempyr schema upgrade --apply`, and
/// `tempyr doctor` all keep these covered.
pub const INTERVIEW_ARCHIVE_ENTRIES: &[&str] = &[".tempyr/sessions/", ".tempyr/spar/"];
pub const INTERVIEW_ARCHIVE_HEADER: &str = "# Tempyr interview archives (may contain raw answers)";

/// Return the entries from `entries` that the project `.gitignore` does not cover.
pub fn missing_entries<'a>(root: &Path, entries: &[&'a str]) -> anyhow::Result<Vec<&'a str>> {
    let path = root.join(".gitignore");
    let existing = if path.exists() {
        fs::read_to_string(&path)?
    } else {
        String::new()
    };
    Ok(entries
        .iter()
        .copied()
        .filter(|entry| !is_covered(&existing, entry))
        .collect())
}

/// Append any of `entries` the project `.gitignore` does not already cover,
/// under `header`. Returns the entries that were added.
pub fn ensure_entries<'a>(
    root: &Path,
    header: &str,
    entries: &[&'a str],
) -> anyhow::Result<Vec<&'a str>> {
    let missing = missing_entries(root, entries)?;
    if missing.is_empty() {
        return Ok(missing);
    }

    let path = root.join(".gitignore");
    let mut output = if path.exists() {
        fs::read_to_string(&path)?
    } else {
        String::new()
    };
    if !output.is_empty() && !output.ends_with('\n') {
        output.push('\n');
    }
    if !output.is_empty() {
        output.push('\n');
    }
    output.push_str(header);
    output.push('\n');
    for entry in &missing {
        output.push_str(entry);
        output.push('\n');
    }

    fs::write(path, output)?;
    Ok(missing)
}

/// Whether a `.gitignore` body already ignores `entry`, either by an equivalent
/// line (leading `/` and trailing `/` are not significant here) or by ignoring
/// one of its parent directories.
fn is_covered(gitignore: &str, entry: &str) -> bool {
    let wanted = normalize(entry);
    gitignore
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#') && !line.starts_with('!'))
        .map(normalize)
        .any(|line| {
            let line = line.strip_suffix("/*").unwrap_or(line);
            wanted == line
                || wanted
                    .strip_prefix(line)
                    .is_some_and(|rest| rest.starts_with('/'))
        })
}

fn normalize(pattern: &str) -> &str {
    let pattern = pattern.strip_prefix('/').unwrap_or(pattern);
    pattern.strip_suffix('/').unwrap_or(pattern)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_entries_appends_missing_entries_once() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join(".gitignore"), "/target").unwrap();

        let added = ensure_entries(
            tmp.path(),
            INTERVIEW_ARCHIVE_HEADER,
            INTERVIEW_ARCHIVE_ENTRIES,
        )
        .unwrap();
        assert_eq!(added, INTERVIEW_ARCHIVE_ENTRIES);

        let again = ensure_entries(
            tmp.path(),
            INTERVIEW_ARCHIVE_HEADER,
            INTERVIEW_ARCHIVE_ENTRIES,
        )
        .unwrap();
        assert!(again.is_empty());

        let content = fs::read_to_string(tmp.path().join(".gitignore")).unwrap();
        assert_eq!(
            content,
            format!("/target\n\n{INTERVIEW_ARCHIVE_HEADER}\n.tempyr/sessions/\n.tempyr/spar/\n")
        );
    }

    #[test]
    fn ensure_entries_only_adds_uncovered_entries() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join(".gitignore"), ".tempyr/sessions/\n").unwrap();

        let added = ensure_entries(
            tmp.path(),
            INTERVIEW_ARCHIVE_HEADER,
            INTERVIEW_ARCHIVE_ENTRIES,
        )
        .unwrap();
        assert_eq!(added, vec![".tempyr/spar/"]);
        let content = fs::read_to_string(tmp.path().join(".gitignore")).unwrap();
        assert_eq!(content.matches(".tempyr/sessions/").count(), 1);
    }

    #[test]
    fn creates_gitignore_when_missing() {
        let tmp = tempfile::tempdir().unwrap();
        ensure_entries(tmp.path(), "# header", &[".env.local"]).unwrap();
        assert_eq!(
            fs::read_to_string(tmp.path().join(".gitignore")).unwrap(),
            "# header\n.env.local\n"
        );
    }

    #[test]
    fn coverage_accepts_equivalent_and_parent_patterns() {
        assert!(is_covered("/.tempyr/sessions\n", ".tempyr/sessions/"));
        assert!(is_covered(".tempyr/\n", ".tempyr/spar/"));
        assert!(is_covered(".tempyr/*\n", ".tempyr/spar/"));
        assert!(!is_covered("# .tempyr/spar/\n", ".tempyr/spar/"));
        assert!(!is_covered(".tempyr/sessions-old/\n", ".tempyr/sessions/"));
        assert!(!is_covered(".tempyr/index.db\n", ".tempyr/spar/"));
    }
}
