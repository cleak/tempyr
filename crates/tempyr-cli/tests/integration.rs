use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use std::process::{Command as ProcessCommand, Stdio};
use std::thread;
use std::time::Duration;
use tempfile::TempDir;

fn tempyr() -> Command {
    Command::cargo_bin("tempyr").unwrap()
}

fn init_project(dir: &TempDir) {
    tempyr()
        .current_dir(dir.path())
        .arg("init")
        .assert()
        .success();
}

fn write_node(dir: &TempDir, subdir: &str, id: &str, content: &str) {
    let path = dir
        .path()
        .join("graph")
        .join(subdir)
        .join(format!("{id}.md"));
    fs::write(path, content).unwrap();
}

#[test]
fn test_init_creates_structure() {
    let tmp = TempDir::new().unwrap();
    tempyr()
        .current_dir(tmp.path())
        .arg("init")
        .assert()
        .success()
        .stdout(predicate::str::contains("Initialized tempyr project"));

    assert!(tmp.path().join(".tempyr/schema.toml").exists());
    assert!(tmp.path().join(".tempyr/config.toml").exists());
    assert!(tmp.path().join("graph/features").is_dir());
    assert!(tmp.path().join("graph/epics").is_dir());
    assert!(tmp.path().join("graph/tasks").is_dir());
}

#[test]
fn test_init_fails_if_already_initialized() {
    let tmp = TempDir::new().unwrap();
    init_project(&tmp);

    tempyr()
        .current_dir(tmp.path())
        .arg("init")
        .assert()
        .failure()
        .stderr(predicate::str::contains("Already initialized"));
}

#[test]
fn test_init_json_is_rejected_until_supported() {
    let tmp = TempDir::new().unwrap();

    tempyr()
        .current_dir(tmp.path())
        .args(["--json", "init"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "`tempyr init --json` is not supported yet",
        ));
}

#[test]
fn test_init_rejects_json_and_wizard_together() {
    let tmp = TempDir::new().unwrap();

    tempyr()
        .current_dir(tmp.path())
        .args(["--json", "init", "--wizard"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "--json cannot be combined with --wizard",
        ));
}

#[test]
fn test_mcp_flag_rejects_extra_args() {
    let tmp = TempDir::new().unwrap();

    tempyr()
        .current_dir(tmp.path())
        .args(["--mcp", "validate"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "`--mcp` must be the first and only argument",
        ));
}

#[test]
fn test_mcp_mode_starts_on_stdio() {
    let tempyr_bin = assert_cmd::cargo::cargo_bin("tempyr");
    let mut child = ProcessCommand::new(tempyr_bin)
        .arg("--mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    thread::sleep(Duration::from_millis(300));

    if let Some(status) = child.try_wait().unwrap() {
        panic!("tempyr --mcp exited early with status {status}");
    }

    child.kill().unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.stdout.is_empty());
}

#[test]
fn test_validate_empty_graph() {
    let tmp = TempDir::new().unwrap();
    init_project(&tmp);

    tempyr()
        .current_dir(tmp.path())
        .arg("validate")
        .assert()
        .success()
        .stdout(predicate::str::contains("Graph is valid"));
}

#[test]
fn test_add_node() {
    let tmp = TempDir::new().unwrap();
    init_project(&tmp);

    tempyr()
        .current_dir(tmp.path())
        .args([
            "add",
            "feature",
            "--id",
            "feat-test",
            "--status",
            "draft",
            "--owner",
            "caleb",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Created feat-test"));

    assert!(tmp.path().join("graph/features/feat-test.md").exists());
}

#[test]
fn test_validate_with_valid_nodes() {
    let tmp = TempDir::new().unwrap();
    init_project(&tmp);

    write_node(
        &tmp,
        "features",
        "feat-a",
        "---\nid: feat-a\ntype: feature\nstatus: draft\nowner: caleb\nedges:\n  - target: epic-a\n    type: child_of\n---\n# Feat A\n",
    );
    write_node(
        &tmp,
        "epics",
        "epic-a",
        "---\nid: epic-a\ntype: epic\nstatus: draft\nowner: caleb\nedges:\n  - target: feat-a\n    type: parent_of\n---\n# Epic A\n",
    );

    tempyr()
        .current_dir(tmp.path())
        .arg("validate")
        .assert()
        .success()
        .stdout(predicate::str::contains("Graph is valid"));
}

#[test]
fn test_validate_catches_dangling_edge() {
    let tmp = TempDir::new().unwrap();
    init_project(&tmp);

    write_node(
        &tmp,
        "features",
        "feat-a",
        "---\nid: feat-a\ntype: feature\nstatus: draft\nowner: caleb\nedges:\n  - target: nonexistent\n    type: depends_on\n---\n# A\n",
    );

    tempyr()
        .current_dir(tmp.path())
        .arg("validate")
        .assert()
        .failure();
}

#[test]
fn test_add_edge_and_validate() {
    let tmp = TempDir::new().unwrap();
    init_project(&tmp);

    write_node(
        &tmp,
        "features",
        "feat-a",
        "---\nid: feat-a\ntype: feature\nstatus: draft\nowner: caleb\n---\n# A\n",
    );
    write_node(
        &tmp,
        "epics",
        "epic-a",
        "---\nid: epic-a\ntype: epic\nstatus: draft\nowner: caleb\n---\n# Epic\n",
    );

    tempyr()
        .current_dir(tmp.path())
        .args(["add-edge", "feat-a", "epic-a", "child_of"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Added edge"));

    tempyr()
        .current_dir(tmp.path())
        .arg("validate")
        .assert()
        .success()
        .stdout(predicate::str::contains("Graph is valid"));
}

#[test]
fn test_remove_edge() {
    let tmp = TempDir::new().unwrap();
    init_project(&tmp);

    write_node(
        &tmp,
        "features",
        "feat-a",
        "---\nid: feat-a\ntype: feature\nstatus: draft\nowner: caleb\n---\n# A\n",
    );
    write_node(
        &tmp,
        "epics",
        "epic-a",
        "---\nid: epic-a\ntype: epic\nstatus: draft\nowner: caleb\n---\n# Epic\n",
    );

    tempyr()
        .current_dir(tmp.path())
        .args(["add-edge", "feat-a", "epic-a", "child_of"])
        .assert()
        .success();

    tempyr()
        .current_dir(tmp.path())
        .args(["remove-edge", "feat-a", "epic-a", "child_of"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Removed edge"));
}

#[test]
fn test_rename_node() {
    let tmp = TempDir::new().unwrap();
    init_project(&tmp);

    write_node(
        &tmp,
        "features",
        "feat-old",
        "---\nid: feat-old\ntype: feature\nstatus: draft\nowner: caleb\n---\n# Old\n",
    );

    tempyr()
        .current_dir(tmp.path())
        .args(["rename", "feat-old", "feat-new"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Renamed feat-old -> feat-new"));

    assert!(!tmp.path().join("graph/features/feat-old.md").exists());
    assert!(tmp.path().join("graph/features/feat-new.md").exists());
}

#[test]
fn test_status_update() {
    let tmp = TempDir::new().unwrap();
    init_project(&tmp);

    write_node(
        &tmp,
        "features",
        "feat-a",
        "---\nid: feat-a\ntype: feature\nstatus: draft\nowner: caleb\n---\n# A\n",
    );

    tempyr()
        .current_dir(tmp.path())
        .args(["status", "feat-a", "active"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Updated feat-a status to 'active'",
        ));
}

#[test]
fn test_traverse() {
    let tmp = TempDir::new().unwrap();
    init_project(&tmp);

    write_node(
        &tmp,
        "features",
        "feat-a",
        "---\nid: feat-a\ntype: feature\nstatus: draft\nowner: caleb\nedges:\n  - target: epic-a\n    type: child_of\n---\n# Feat A\n",
    );
    write_node(
        &tmp,
        "epics",
        "epic-a",
        "---\nid: epic-a\ntype: epic\nstatus: draft\nowner: caleb\nedges:\n  - target: feat-a\n    type: parent_of\n---\n# Epic A\n",
    );

    tempyr()
        .current_dir(tmp.path())
        .args(["traverse", "feat-a"])
        .assert()
        .success()
        .stdout(predicate::str::contains("feat-a"))
        .stdout(predicate::str::contains("epic-a"));
}

#[test]
fn test_index_rebuild_and_search() {
    let tmp = TempDir::new().unwrap();
    init_project(&tmp);

    write_node(
        &tmp,
        "features",
        "feat-replay",
        "---\nid: feat-replay\ntype: feature\nstatus: draft\nowner: caleb\n---\n# Session Replay\n\nCapture and replay user sessions.\n",
    );

    tempyr()
        .current_dir(tmp.path())
        .args(["index", "rebuild"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Index rebuilt: 1 nodes"));

    tempyr()
        .current_dir(tmp.path())
        .args(["search", "replay"])
        .assert()
        .success()
        .stdout(predicate::str::contains("feat-replay"));
}

#[test]
fn test_add_refreshes_index_for_follow_up_search() {
    let tmp = TempDir::new().unwrap();
    init_project(&tmp);

    write_node(
        &tmp,
        "features",
        "feat-existing",
        "---\nid: feat-existing\ntype: feature\nstatus: draft\nowner: caleb\n---\n# Existing\n\nAlready indexed.\n",
    );

    tempyr()
        .current_dir(tmp.path())
        .args(["index", "rebuild"])
        .assert()
        .success();

    tempyr()
        .current_dir(tmp.path())
        .args([
            "add",
            "feature",
            "--id",
            "feat-terrain",
            "--status",
            "draft",
            "--owner",
            "caleb",
            "--body",
            "# Terrain Streaming\n\nLOD terrain streaming for large worlds.\n",
        ])
        .assert()
        .success();

    tempyr()
        .current_dir(tmp.path())
        .args(["search", "terrain"])
        .assert()
        .success()
        .stdout(predicate::str::contains("feat-terrain"));
}

#[test]
fn test_add_builds_index_when_missing() {
    let tmp = TempDir::new().unwrap();
    init_project(&tmp);

    tempyr()
        .current_dir(tmp.path())
        .args([
            "add",
            "feature",
            "--id",
            "feat-terrain",
            "--status",
            "draft",
            "--owner",
            "caleb",
            "--body",
            "# Terrain Streaming\n\nLOD terrain streaming for large worlds.\n",
        ])
        .assert()
        .success();

    tempyr()
        .current_dir(tmp.path())
        .args(["search", "terrain"])
        .assert()
        .success()
        .stdout(predicate::str::contains("feat-terrain"));
}

#[test]
fn test_status_refreshes_index_for_filtered_search() {
    let tmp = TempDir::new().unwrap();
    init_project(&tmp);

    write_node(
        &tmp,
        "features",
        "feat-terrain",
        "---\nid: feat-terrain\ntype: feature\nstatus: draft\nowner: caleb\n---\n# Terrain Streaming\n\nLOD terrain streaming for large worlds.\n",
    );

    tempyr()
        .current_dir(tmp.path())
        .args(["index", "rebuild"])
        .assert()
        .success();

    tempyr()
        .current_dir(tmp.path())
        .args(["status", "feat-terrain", "active"])
        .assert()
        .success();

    tempyr()
        .current_dir(tmp.path())
        .args(["search", "terrain", "--status", "active"])
        .assert()
        .success()
        .stdout(predicate::str::contains("feat-terrain"));
}

#[test]
fn test_add_edge_refreshes_index_for_follow_up_search() {
    let tmp = TempDir::new().unwrap();
    init_project(&tmp);

    write_node(
        &tmp,
        "features",
        "feat-terrain",
        "---\nid: feat-terrain\ntype: feature\nstatus: draft\nowner: caleb\n---\n# Terrain Streaming\n\nLOD terrain streaming for large worlds.\n",
    );
    write_node(
        &tmp,
        "epics",
        "epic-world",
        "---\nid: epic-world\ntype: epic\nstatus: draft\nowner: caleb\n---\n# World Streaming\n\nParent epic.\n",
    );

    tempyr()
        .current_dir(tmp.path())
        .args(["index", "rebuild"])
        .assert()
        .success();

    tempyr()
        .current_dir(tmp.path())
        .args(["add-edge", "feat-terrain", "epic-world", "child_of"])
        .assert()
        .success();

    tempyr()
        .current_dir(tmp.path())
        .args(["search", "terrain"])
        .assert()
        .success()
        .stdout(predicate::str::contains("feat-terrain"));
}

#[test]
fn test_render_prd() {
    let tmp = TempDir::new().unwrap();
    init_project(&tmp);

    write_node(
        &tmp,
        "features",
        "feat-replay",
        "---\nid: feat-replay\ntype: feature\nstatus: draft\nowner: caleb\n---\n# Session Replay\n\nCapture user sessions.\n",
    );

    tempyr()
        .current_dir(tmp.path())
        .args(["render", "prd", "feat-replay"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Product Requirements Document: Session Replay",
        ));
}

#[test]
fn test_render_to_file() {
    let tmp = TempDir::new().unwrap();
    init_project(&tmp);

    write_node(
        &tmp,
        "features",
        "feat-a",
        "---\nid: feat-a\ntype: feature\nstatus: draft\nowner: caleb\n---\n# Feature A\n\nBody text.\n",
    );

    let output_path = tmp.path().join("output.md");
    tempyr()
        .current_dir(tmp.path())
        .args([
            "render",
            "prd",
            "feat-a",
            "--output",
            output_path.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Rendered to"));

    assert!(output_path.exists());
    let content = fs::read_to_string(&output_path).unwrap();
    assert!(content.contains("Product Requirements Document"));
}

#[test]
fn test_json_output() {
    let tmp = TempDir::new().unwrap();
    init_project(&tmp);

    write_node(
        &tmp,
        "features",
        "feat-a",
        "---\nid: feat-a\ntype: feature\nstatus: draft\nowner: caleb\n---\n# A\n",
    );

    tempyr()
        .current_dir(tmp.path())
        .args(["--json", "validate"])
        .assert()
        .success()
        .stdout(predicate::str::contains("[]")); // empty array = no issues
}

#[test]
fn test_index_stats() {
    let tmp = TempDir::new().unwrap();
    init_project(&tmp);

    write_node(
        &tmp,
        "features",
        "feat-a",
        "---\nid: feat-a\ntype: feature\nstatus: draft\nowner: caleb\n---\n# A\n",
    );

    tempyr()
        .current_dir(tmp.path())
        .args(["index", "rebuild"])
        .assert()
        .success();

    tempyr()
        .current_dir(tmp.path())
        .args(["index", "stats"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Nodes: 1"));
}

#[test]
fn test_list_by_status() {
    let tmp = TempDir::new().unwrap();
    init_project(&tmp);

    write_node(
        &tmp,
        "tasks",
        "task-a",
        "---\nid: task-a\ntype: task\nstatus: backlog\nowner: caleb\n---\n# Task A\n\nDo stuff.\n",
    );
    write_node(
        &tmp,
        "tasks",
        "task-b",
        "---\nid: task-b\ntype: task\nstatus: in_progress\nowner: alice\n---\n# Task B\n\nDo other stuff.\n",
    );
    write_node(
        &tmp,
        "features",
        "feat-x",
        "---\nid: feat-x\ntype: feature\nstatus: draft\nowner: caleb\n---\n# Feature X\n\nA feature.\n",
    );

    tempyr()
        .current_dir(tmp.path())
        .args(["index", "rebuild"])
        .assert()
        .success();

    // List all tasks
    tempyr()
        .current_dir(tmp.path())
        .args(["list", "--type", "task"])
        .assert()
        .success()
        .stdout(predicate::str::contains("task-a"))
        .stdout(predicate::str::contains("task-b"));

    // List by status
    tempyr()
        .current_dir(tmp.path())
        .args(["list", "--status", "backlog"])
        .assert()
        .success()
        .stdout(predicate::str::contains("task-a"))
        .stdout(predicate::str::contains("task-b").not());

    // List by owner
    tempyr()
        .current_dir(tmp.path())
        .args(["list", "--owner", "caleb"])
        .assert()
        .success()
        .stdout(predicate::str::contains("task-a"))
        .stdout(predicate::str::contains("feat-x"))
        .stdout(predicate::str::contains("task-b").not());

    // Combined: type + status
    tempyr()
        .current_dir(tmp.path())
        .args(["list", "--type", "task", "--status", "in_progress"])
        .assert()
        .success()
        .stdout(predicate::str::contains("task-b"))
        .stdout(predicate::str::contains("task-a").not());

    // No filters lists everything
    tempyr()
        .current_dir(tmp.path())
        .args(["list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("task-a"))
        .stdout(predicate::str::contains("task-b"))
        .stdout(predicate::str::contains("feat-x"));
}

#[test]
fn test_search_with_status_filter() {
    let tmp = TempDir::new().unwrap();
    init_project(&tmp);

    write_node(
        &tmp,
        "tasks",
        "task-a",
        "---\nid: task-a\ntype: task\nstatus: backlog\n---\n# Build Pipeline\n\nBuild the data pipeline.\n",
    );
    write_node(
        &tmp,
        "tasks",
        "task-b",
        "---\nid: task-b\ntype: task\nstatus: done\n---\n# Test Pipeline\n\nTest the data pipeline.\n",
    );

    tempyr()
        .current_dir(tmp.path())
        .args(["index", "rebuild"])
        .assert()
        .success();

    // Search "pipeline" with no filter finds both
    tempyr()
        .current_dir(tmp.path())
        .args(["search", "pipeline"])
        .assert()
        .success()
        .stdout(predicate::str::contains("task-a"))
        .stdout(predicate::str::contains("task-b"));

    // Search "pipeline" with --status backlog finds only task-a
    tempyr()
        .current_dir(tmp.path())
        .args(["search", "pipeline", "--status", "backlog"])
        .assert()
        .success()
        .stdout(predicate::str::contains("task-a"))
        .stdout(predicate::str::contains("task-b").not());
}

#[test]
fn test_interview_start_and_list() {
    let tmp = TempDir::new().unwrap();
    init_project(&tmp);

    tempyr()
        .current_dir(tmp.path())
        .args([
            "interview",
            "start",
            "We need session replay for debugging funnel drop-offs",
            "--root-type",
            "feature",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Interview started"))
        .stdout(predicate::str::contains("Next questions"));

    tempyr()
        .current_dir(tmp.path())
        .args(["interview", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Active sessions"));
}

#[test]
fn test_interview_show() {
    let tmp = TempDir::new().unwrap();
    init_project(&tmp);

    // Start a session and capture the ID
    let output = tempyr()
        .current_dir(tmp.path())
        .args(["--json", "interview", "start", "Session replay feature"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let json: serde_json::Value = serde_json::from_slice(&output).unwrap();
    let session_id = json["session_id"].as_str().unwrap();

    tempyr()
        .current_dir(tmp.path())
        .args(["interview", "show", session_id])
        .assert()
        .success()
        .stdout(predicate::str::contains("Session:"))
        .stdout(predicate::str::contains("Phase:"));
}

#[test]
fn test_interview_commit() {
    let tmp = TempDir::new().unwrap();
    init_project(&tmp);

    let output = tempyr()
        .current_dir(tmp.path())
        .args([
            "--json",
            "interview",
            "start",
            "Build a replay system for sessions",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let json: serde_json::Value = serde_json::from_slice(&output).unwrap();
    let session_id = json["session_id"].as_str().unwrap();

    tempyr()
        .current_dir(tmp.path())
        .args(["interview", "commit", session_id])
        .assert()
        .success()
        .stdout(predicate::str::contains("Committed session"));

    // Session should be gone
    tempyr()
        .current_dir(tmp.path())
        .args(["interview", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("No active"));
}

#[test]
fn test_interview_full_flow() {
    let tmp = TempDir::new().unwrap();
    init_project(&tmp);

    // Start
    let output = tempyr()
        .current_dir(tmp.path())
        .args([
            "--json",
            "interview",
            "start",
            "We need a session replay feature for debugging",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let json: serde_json::Value = serde_json::from_slice(&output).unwrap();
    let session_id = json["session_id"].as_str().unwrap().to_string();

    // Answer a question
    tempyr()
        .current_dir(tmp.path())
        .args([
            "interview",
            "answer",
            &session_id,
            "Platform engineers are the target users",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Answer recorded"));

    // Show state
    tempyr()
        .current_dir(tmp.path())
        .args(["interview", "show", &session_id])
        .assert()
        .success()
        .stdout(predicate::str::contains("Remaining gaps"));

    // Commit
    tempyr()
        .current_dir(tmp.path())
        .args(["interview", "commit", &session_id])
        .assert()
        .success()
        .stdout(predicate::str::contains("Committed"));

    // Validate the committed graph
    tempyr()
        .current_dir(tmp.path())
        .arg("validate")
        .assert()
        .success();
}
