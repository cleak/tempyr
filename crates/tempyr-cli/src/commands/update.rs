use std::path::Path;

use super::git_hooks::{self, HookStatus};
use super::managed::{self, FileStatus, WriteOutcome};

pub fn run(check: bool, force: bool) -> anyhow::Result<()> {
    let cwd = std::env::current_dir()?;
    let tempyr_dir = cwd.join(".tempyr");

    if !tempyr_dir.exists() {
        anyhow::bail!("Not a tempyr project. Run `tempyr init` first.");
    }

    if check {
        return run_check(&cwd);
    }

    run_update(&cwd, force)
}

fn run_check(root: &Path) -> anyhow::Result<()> {
    let reports = managed::check_all(root)?;
    let hook_reports = git_hooks::check_all(root)?;
    let mut any_stale = false;

    for item in &reports {
        let (symbol, label) = match item.status {
            FileStatus::UpToDate => ("  ", "up to date"),
            FileStatus::Stale => {
                any_stale = true;
                ("S ", "stale (safe to update)")
            }
            FileStatus::UserModified => {
                any_stale = true;
                ("SM", "stale (user modified, use --force)")
            }
            FileStatus::Missing => {
                any_stale = true;
                ("M ", "missing (will create)")
            }
        };
        println!("{symbol} {:<55} {label} ({})", item.path, item.description);
    }

    for item in &hook_reports {
        let (symbol, label) = match item.status {
            HookStatus::UpToDate => ("  ", "up to date"),
            HookStatus::Stale => {
                any_stale = true;
                ("S ", "stale (safe to update)")
            }
            HookStatus::Missing => {
                any_stale = true;
                ("M ", "missing (will create)")
            }
        };
        println!(
            "{symbol} git hook {:<46} {label} ({})",
            item.name, item.description
        );
    }

    if any_stale {
        println!("\nRun `tempyr update` to apply updates.");
        std::process::exit(1);
    } else {
        println!("\nAll managed files and git hooks are up to date.");
    }

    Ok(())
}

fn run_update(root: &Path, force: bool) -> anyhow::Result<()> {
    // Preview what will happen.
    let reports = managed::check_all(root)?;
    let hook_reports = git_hooks::check_all(root)?;
    let has_user_modified = reports.iter().any(|r| r.status == FileStatus::UserModified);
    let has_work = reports
        .iter()
        .any(|r| !matches!(r.status, FileStatus::UpToDate))
        || hook_reports
            .iter()
            .any(|r| !matches!(r.status, HookStatus::UpToDate));

    if !has_work {
        println!("All managed files and git hooks are up to date.");
        return Ok(());
    }

    if has_user_modified && !force {
        eprintln!("Warning: some files were modified since tempyr last wrote them:");
        for item in &reports {
            if item.status == FileStatus::UserModified {
                eprintln!("  {}", item.path);
            }
        }
        eprintln!("These will be skipped. Use --force to overwrite.");
    }

    let results = managed::install_all(root, force)?;
    let hook_results = git_hooks::install_all(root)?;

    for r in &results {
        match r.outcome {
            WriteOutcome::Created => println!("  Created: {}  ({})", r.path, r.description),
            WriteOutcome::Updated => println!("  Updated: {}  ({})", r.path, r.description),
            WriteOutcome::Merged => println!("  Merged:  {}  ({})", r.path, r.description),
            WriteOutcome::Skipped => println!("  Skipped: {}  (user modified)", r.path),
            WriteOutcome::Unchanged => {}
        }
    }

    for r in &hook_results {
        match r.outcome {
            WriteOutcome::Created => {
                println!("  Created: git hook {}  ({})", r.name, r.description)
            }
            WriteOutcome::Updated => {
                println!("  Updated: git hook {}  ({})", r.name, r.description)
            }
            WriteOutcome::Merged => println!("  Merged:  git hook {}  ({})", r.name, r.description),
            WriteOutcome::Skipped => {}
            WriteOutcome::Unchanged => {}
        }
    }

    Ok(())
}
