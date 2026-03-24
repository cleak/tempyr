use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use tempfile::TempDir;

fn graphforge() -> Command {
    Command::cargo_bin("graphforge").unwrap()
}

fn init_project(dir: &TempDir) {
    graphforge()
        .current_dir(dir.path())
        .arg("init")
        .assert()
        .success();
}

fn write_node(dir: &TempDir, subdir: &str, id: &str, content: &str) {
    let path = dir.path().join("graph").join(subdir).join(format!("{id}.md"));
    fs::write(path, content).unwrap();
}

#[test]
fn test_init_creates_structure() {
    let tmp = TempDir::new().unwrap();
    graphforge()
        .current_dir(tmp.path())
        .arg("init")
        .assert()
        .success()
        .stdout(predicate::str::contains("Initialized graphforge project"));

    assert!(tmp.path().join(".graphforge/schema.toml").exists());
    assert!(tmp.path().join(".graphforge/config.toml").exists());
    assert!(tmp.path().join("graph/features").is_dir());
    assert!(tmp.path().join("graph/epics").is_dir());
    assert!(tmp.path().join("graph/tasks").is_dir());
}

#[test]
fn test_init_fails_if_already_initialized() {
    let tmp = TempDir::new().unwrap();
    init_project(&tmp);

    graphforge()
        .current_dir(tmp.path())
        .arg("init")
        .assert()
        .failure()
        .stderr(predicate::str::contains("Already initialized"));
}

#[test]
fn test_validate_empty_graph() {
    let tmp = TempDir::new().unwrap();
    init_project(&tmp);

    graphforge()
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

    graphforge()
        .current_dir(tmp.path())
        .args(["add", "feature", "--id", "feat-test", "--status", "draft", "--owner", "caleb"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Created feat-test"));

    assert!(tmp.path().join("graph/features/feat-test.md").exists());
}

#[test]
fn test_validate_with_valid_nodes() {
    let tmp = TempDir::new().unwrap();
    init_project(&tmp);

    write_node(&tmp, "features", "feat-a",
        "---\nid: feat-a\ntype: feature\nstatus: draft\nowner: caleb\nedges:\n  - target: epic-a\n    type: child_of\n---\n# Feat A\n");
    write_node(&tmp, "epics", "epic-a",
        "---\nid: epic-a\ntype: epic\nstatus: draft\nowner: caleb\nedges:\n  - target: feat-a\n    type: parent_of\n---\n# Epic A\n");

    graphforge()
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

    write_node(&tmp, "features", "feat-a",
        "---\nid: feat-a\ntype: feature\nstatus: draft\nowner: caleb\nedges:\n  - target: nonexistent\n    type: depends_on\n---\n# A\n");

    graphforge()
        .current_dir(tmp.path())
        .arg("validate")
        .assert()
        .failure();
}

#[test]
fn test_add_edge_and_validate() {
    let tmp = TempDir::new().unwrap();
    init_project(&tmp);

    write_node(&tmp, "features", "feat-a",
        "---\nid: feat-a\ntype: feature\nstatus: draft\nowner: caleb\n---\n# A\n");
    write_node(&tmp, "epics", "epic-a",
        "---\nid: epic-a\ntype: epic\nstatus: draft\nowner: caleb\n---\n# Epic\n");

    graphforge()
        .current_dir(tmp.path())
        .args(["add-edge", "feat-a", "epic-a", "child_of"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Added edge"));

    graphforge()
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

    write_node(&tmp, "features", "feat-a",
        "---\nid: feat-a\ntype: feature\nstatus: draft\nowner: caleb\n---\n# A\n");
    write_node(&tmp, "epics", "epic-a",
        "---\nid: epic-a\ntype: epic\nstatus: draft\nowner: caleb\n---\n# Epic\n");

    graphforge()
        .current_dir(tmp.path())
        .args(["add-edge", "feat-a", "epic-a", "child_of"])
        .assert()
        .success();

    graphforge()
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

    write_node(&tmp, "features", "feat-old",
        "---\nid: feat-old\ntype: feature\nstatus: draft\nowner: caleb\n---\n# Old\n");

    graphforge()
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

    write_node(&tmp, "features", "feat-a",
        "---\nid: feat-a\ntype: feature\nstatus: draft\nowner: caleb\n---\n# A\n");

    graphforge()
        .current_dir(tmp.path())
        .args(["status", "feat-a", "active"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Updated feat-a status to 'active'"));
}

#[test]
fn test_traverse() {
    let tmp = TempDir::new().unwrap();
    init_project(&tmp);

    write_node(&tmp, "features", "feat-a",
        "---\nid: feat-a\ntype: feature\nstatus: draft\nowner: caleb\nedges:\n  - target: epic-a\n    type: child_of\n---\n# Feat A\n");
    write_node(&tmp, "epics", "epic-a",
        "---\nid: epic-a\ntype: epic\nstatus: draft\nowner: caleb\nedges:\n  - target: feat-a\n    type: parent_of\n---\n# Epic A\n");

    graphforge()
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

    write_node(&tmp, "features", "feat-replay",
        "---\nid: feat-replay\ntype: feature\nstatus: draft\nowner: caleb\n---\n# Session Replay\n\nCapture and replay user sessions.\n");

    graphforge()
        .current_dir(tmp.path())
        .args(["index", "rebuild"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Index rebuilt: 1 nodes"));

    graphforge()
        .current_dir(tmp.path())
        .args(["search", "replay"])
        .assert()
        .success()
        .stdout(predicate::str::contains("feat-replay"));
}

#[test]
fn test_render_prd() {
    let tmp = TempDir::new().unwrap();
    init_project(&tmp);

    write_node(&tmp, "features", "feat-replay",
        "---\nid: feat-replay\ntype: feature\nstatus: draft\nowner: caleb\n---\n# Session Replay\n\nCapture user sessions.\n");

    graphforge()
        .current_dir(tmp.path())
        .args(["render", "prd", "feat-replay"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Product Requirements Document: Session Replay"));
}

#[test]
fn test_render_to_file() {
    let tmp = TempDir::new().unwrap();
    init_project(&tmp);

    write_node(&tmp, "features", "feat-a",
        "---\nid: feat-a\ntype: feature\nstatus: draft\nowner: caleb\n---\n# Feature A\n\nBody text.\n");

    let output_path = tmp.path().join("output.md");
    graphforge()
        .current_dir(tmp.path())
        .args(["render", "prd", "feat-a", "--output", output_path.to_str().unwrap()])
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

    write_node(&tmp, "features", "feat-a",
        "---\nid: feat-a\ntype: feature\nstatus: draft\nowner: caleb\n---\n# A\n");

    graphforge()
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

    write_node(&tmp, "features", "feat-a",
        "---\nid: feat-a\ntype: feature\nstatus: draft\nowner: caleb\n---\n# A\n");

    graphforge()
        .current_dir(tmp.path())
        .args(["index", "rebuild"])
        .assert()
        .success();

    graphforge()
        .current_dir(tmp.path())
        .args(["index", "stats"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Nodes: 1"));
}

#[test]
fn test_interview_start_and_list() {
    let tmp = TempDir::new().unwrap();
    init_project(&tmp);

    graphforge()
        .current_dir(tmp.path())
        .args(["interview", "start", "We need session replay for debugging funnel drop-offs", "--root-type", "feature"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Interview started"))
        .stdout(predicate::str::contains("Next questions"));

    graphforge()
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
    let output = graphforge()
        .current_dir(tmp.path())
        .args(["--json", "interview", "start", "Session replay feature"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let json: serde_json::Value = serde_json::from_slice(&output).unwrap();
    let session_id = json["session_id"].as_str().unwrap();

    graphforge()
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

    let output = graphforge()
        .current_dir(tmp.path())
        .args(["--json", "interview", "start", "Build a replay system for sessions"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let json: serde_json::Value = serde_json::from_slice(&output).unwrap();
    let session_id = json["session_id"].as_str().unwrap();

    graphforge()
        .current_dir(tmp.path())
        .args(["interview", "commit", session_id])
        .assert()
        .success()
        .stdout(predicate::str::contains("Committed session"));

    // Session should be gone
    graphforge()
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
    let output = graphforge()
        .current_dir(tmp.path())
        .args(["--json", "interview", "start", "We need a session replay feature for debugging"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let json: serde_json::Value = serde_json::from_slice(&output).unwrap();
    let session_id = json["session_id"].as_str().unwrap().to_string();

    // Answer a question
    graphforge()
        .current_dir(tmp.path())
        .args(["interview", "answer", &session_id, "Platform engineers are the target users"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Answer recorded"));

    // Show state
    graphforge()
        .current_dir(tmp.path())
        .args(["interview", "show", &session_id])
        .assert()
        .success()
        .stdout(predicate::str::contains("Remaining gaps"));

    // Commit
    graphforge()
        .current_dir(tmp.path())
        .args(["interview", "commit", &session_id])
        .assert()
        .success()
        .stdout(predicate::str::contains("Committed"));

    // Validate the committed graph
    graphforge()
        .current_dir(tmp.path())
        .arg("validate")
        .assert()
        .success();
}
