//! Project root resolution with redirect support.
//!
//! Walks up from the current directory looking for `.tempyr/` or `.tempyr-redirect`.
//! A `.tempyr-redirect` file contains a path (relative or absolute) pointing to
//! the real tempyr project root. This lets you run tempyr commands from a working
//! project that stores its knowledge graph in a separate repository.

use std::path::{Path, PathBuf};

/// Walk up the directory tree to find a tempyr project root.
///
/// Checks each directory for:
/// 1. `.tempyr-redirect` — a file whose first non-empty line is a path to the real project root
/// 2. `.tempyr/` — a directory indicating this is the project root
///
/// Redirect paths are resolved relative to the directory containing the redirect file.
/// Only one level of redirect is followed (no chaining).
pub fn find_project_root() -> Option<PathBuf> {
    find_project_root_from(std::env::current_dir().ok()?)
}

/// Same as [`find_project_root`] but starting from a given directory.
pub fn find_project_root_from(start: PathBuf) -> Option<PathBuf> {
    let mut dir = start;
    loop {
        // Check for redirect file first
        let redirect_path = dir.join(".tempyr-redirect");
        if redirect_path.is_file()
            && let Some(target) = read_redirect(&redirect_path, &dir)
            && target.join(".tempyr").is_dir()
        {
            return Some(target);
        }

        // Check for direct .tempyr/ directory
        if dir.join(".tempyr").is_dir() {
            return Some(dir);
        }

        if !dir.pop() {
            return None;
        }
    }
}

/// Read a `.tempyr-redirect` file and resolve the path it contains.
fn read_redirect(file: &Path, base_dir: &Path) -> Option<PathBuf> {
    let contents = std::fs::read_to_string(file).ok()?;
    let target = contents.lines().find(|l| !l.trim().is_empty())?.trim();

    if target.is_empty() {
        return None;
    }

    let path = PathBuf::from(target);
    let resolved = if path.is_absolute() {
        path
    } else {
        base_dir.join(path)
    };

    // Canonicalize to clean up ../ segments
    std::fs::canonicalize(resolved).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn finds_direct_project_root() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        fs::create_dir(root.join(".tempyr")).unwrap();

        let found = find_project_root_from(root.clone());
        assert_eq!(found, Some(root));
    }

    #[test]
    fn finds_root_from_subdirectory() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        fs::create_dir(root.join(".tempyr")).unwrap();
        let sub = root.join("src").join("deep");
        fs::create_dir_all(&sub).unwrap();

        let found = find_project_root_from(sub);
        assert_eq!(found, Some(root));
    }

    #[test]
    fn follows_redirect_file() {
        let tmp = tempfile::tempdir().unwrap();

        // Create the real project
        let real_root = tmp.path().join("knowledge-base");
        fs::create_dir_all(real_root.join(".tempyr")).unwrap();
        fs::create_dir(real_root.join("graph")).unwrap();

        // Create the working project with a redirect
        let work_root = tmp.path().join("main-project");
        fs::create_dir(&work_root).unwrap();
        fs::write(
            work_root.join(".tempyr-redirect"),
            "../knowledge-base\n",
        )
        .unwrap();

        let found = find_project_root_from(work_root);
        let expected = fs::canonicalize(&real_root).unwrap();
        assert_eq!(found, Some(expected));
    }

    #[test]
    fn redirect_to_missing_project_is_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();

        // Redirect points to a directory without .tempyr/
        let target = root.join("not-a-project");
        fs::create_dir(&target).unwrap();
        fs::write(root.join(".tempyr-redirect"), "not-a-project\n").unwrap();

        let found = find_project_root_from(root);
        assert_eq!(found, None);
    }

    #[test]
    fn redirect_with_absolute_path() {
        let tmp = tempfile::tempdir().unwrap();

        let real_root = tmp.path().join("kb");
        fs::create_dir_all(real_root.join(".tempyr")).unwrap();

        let work_root = tmp.path().join("app");
        fs::create_dir(&work_root).unwrap();
        fs::write(
            work_root.join(".tempyr-redirect"),
            real_root.to_str().unwrap(),
        )
        .unwrap();

        let found = find_project_root_from(work_root);
        let expected = fs::canonicalize(&real_root).unwrap();
        assert_eq!(found, Some(expected));
    }
}
