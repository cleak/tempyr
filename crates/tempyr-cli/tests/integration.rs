use assert_cmd::Command;
use predicates::prelude::*;
use rmcp::model::ProtocolVersion;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command as ProcessCommand, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};
use tempfile::TempDir;

fn tempyr() -> Command {
    Command::cargo_bin("tempyr").unwrap()
}

fn tempyr_bin() -> PathBuf {
    assert_cmd::cargo::cargo_bin("tempyr")
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

fn spawn_mcp_child(cwd: &Path) -> Child {
    ProcessCommand::new(tempyr_bin())
        .arg("--mcp")
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

fn spawn_mcp_child_with_project_root(cwd: &Path, project_root: &Path) -> Child {
    ProcessCommand::new(tempyr_bin())
        .arg("--mcp")
        .current_dir(cwd)
        .env(tempyr_core::project::PROJECT_ROOT_ENV_VAR, project_root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

fn spawn_mcp_child_with_project_root_arg(cwd: &Path, project_root: &Path) -> Child {
    ProcessCommand::new(tempyr_bin())
        .args(["--mcp", "--project-root"])
        .arg(project_root)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

fn spawn_mcp_child_with_project_root_arg_and_bad_env(
    cwd: &Path,
    project_root: &Path,
    bad_root: &Path,
) -> Child {
    ProcessCommand::new(tempyr_bin())
        .args(["--mcp", "--project-root"])
        .arg(project_root)
        .current_dir(cwd)
        .env(tempyr_core::project::PROJECT_ROOT_ENV_VAR, bad_root)
        .env(tempyr_core::project::GRAPH_DIR_ENV_VAR, bad_root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

fn wait_for_child_exit(child: &mut Child, timeout: Duration) -> ExitStatus {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        if Instant::now() >= deadline {
            panic!("child process did not exit within {:?}", timeout);
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn read_child_stderr(child: &mut Child) -> String {
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    stderr
}

fn write_json_line(stdin: &mut ChildStdin, value: serde_json::Value) {
    serde_json::to_writer(&mut *stdin, &value).unwrap();
    stdin.write_all(b"\n").unwrap();
    stdin.flush().unwrap();
}

fn initialize_mcp_session_core(
    child: &mut Child,
    capabilities: serde_json::Value,
) -> (ChildStdin, BufReader<ChildStdout>) {
    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut stdout = BufReader::new(stdout);

    write_json_line(
        &mut stdin,
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": ProtocolVersion::V_2025_06_18,
                "capabilities": capabilities,
                "clientInfo": {
                    "name": "integration-test",
                    "version": "0.1.0"
                }
            }
        }),
    );

    let mut response = String::new();
    stdout.read_line(&mut response).unwrap();
    assert!(
        !response.trim().is_empty(),
        "expected initialize response but stdout was empty"
    );

    let response: serde_json::Value = serde_json::from_str(response.trim()).unwrap();
    assert_eq!(response["id"], 1);

    write_json_line(
        &mut stdin,
        serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
            "params": {}
        }),
    );

    (stdin, stdout)
}

fn initialize_mcp_session(child: &mut Child) {
    let (stdin, stdout) = initialize_mcp_session_core(child, serde_json::json!({}));

    child.stdin = Some(stdin);
    child.stdout = Some(stdout.into_inner());
}

fn initialize_mcp_session_with_roots(child: &mut Child, roots: &[&Path]) {
    let (mut stdin, mut stdout) =
        initialize_mcp_session_core(child, serde_json::json!({ "roots": {} }));

    let mut roots_request = String::new();
    stdout.read_line(&mut roots_request).unwrap();
    assert!(
        !roots_request.trim().is_empty(),
        "expected roots/list request but stdout was empty"
    );
    let roots_request: serde_json::Value = serde_json::from_str(roots_request.trim()).unwrap();
    assert_eq!(roots_request["method"], "roots/list");

    let root_entries: Vec<_> = roots
        .iter()
        .map(|root| serde_json::json!({ "uri": file_uri(root) }))
        .collect();
    write_json_line(
        &mut stdin,
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": roots_request["id"].clone(),
            "result": {
                "roots": root_entries
            }
        }),
    );

    child.stdin = Some(stdin);
    child.stdout = Some(stdout.into_inner());
}

fn file_uri(path: &Path) -> String {
    let path = path.to_string_lossy().replace('\\', "/");
    if path.starts_with('/') {
        format!("file://{path}")
    } else {
        format!("file:///{path}")
    }
}

fn read_json_response(child: &mut Child) -> serde_json::Value {
    let stdout = child.stdout.take().unwrap();
    let mut stdout = BufReader::new(stdout);
    let mut response = String::new();
    stdout.read_line(&mut response).unwrap();
    child.stdout = Some(stdout.into_inner());

    assert!(
        !response.trim().is_empty(),
        "expected JSON-RPC response but stdout was empty"
    );

    serde_json::from_str(response.trim()).unwrap()
}

fn call_mcp_tool(
    child: &mut Child,
    id: u64,
    name: &str,
    arguments: serde_json::Value,
) -> serde_json::Value {
    write_json_line(
        child.stdin.as_mut().unwrap(),
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {
                "name": name,
                "arguments": arguments
            }
        }),
    );

    read_json_response(child)
}

fn tool_result_text(response: &serde_json::Value) -> &str {
    assert!(
        response.get("error").is_none(),
        "unexpected JSON-RPC error: {response}"
    );
    response["result"]["content"][0]["text"]
        .as_str()
        .expect("expected text content in MCP tool response")
}

#[derive(serde::Deserialize, serde::Serialize)]
struct ParentDeathHelperInfo {
    pid: u32,
    stderr_path: PathBuf,
    #[serde(default)]
    created_at: Option<u64>,
}

#[cfg(unix)]
fn process_is_running(pid: u32, _created_at: Option<u64>) -> bool {
    let status = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if status == 0 {
        true
    } else {
        std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
    }
}

#[cfg(windows)]
fn process_is_running(pid: u32, created_at: Option<u64>) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, WAIT_TIMEOUT};
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
        WaitForSingleObject,
    };

    unsafe {
        let handle = OpenProcess(
            PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
            0,
            pid,
        );
        if handle.is_null() {
            return false;
        }

        let mut process_created_at = FILETIME::default();
        let mut exit_time = FILETIME::default();
        let mut kernel_time = FILETIME::default();
        let mut user_time = FILETIME::default();
        if GetProcessTimes(
            handle,
            &mut process_created_at,
            &mut exit_time,
            &mut kernel_time,
            &mut user_time,
        ) == 0
        {
            let _ = CloseHandle(handle);
            return false;
        }

        let actual_created_at = ((process_created_at.dwHighDateTime as u64) << 32)
            | process_created_at.dwLowDateTime as u64;
        if let Some(expected_created_at) = created_at
            && actual_created_at != expected_created_at
        {
            let _ = CloseHandle(handle);
            return false;
        }

        let wait = WaitForSingleObject(handle, 0);
        let _ = CloseHandle(handle);
        wait == WAIT_TIMEOUT
    }
}

#[cfg(windows)]
fn process_creation_time(pid: u32) -> Option<u64> {
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME};
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return None;
        }

        let mut process_created_at = FILETIME::default();
        let mut exit_time = FILETIME::default();
        let mut kernel_time = FILETIME::default();
        let mut user_time = FILETIME::default();
        let result = GetProcessTimes(
            handle,
            &mut process_created_at,
            &mut exit_time,
            &mut kernel_time,
            &mut user_time,
        );
        let _ = CloseHandle(handle);
        if result == 0 {
            return None;
        }

        Some(
            ((process_created_at.dwHighDateTime as u64) << 32)
                | process_created_at.dwLowDateTime as u64,
        )
    }
}

#[cfg(not(windows))]
fn process_creation_time(_pid: u32) -> Option<u64> {
    None
}

fn wait_for_pid_exit(info: &ParentDeathHelperInfo, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if !process_is_running(info.pid, info.created_at) {
            return true;
        }
        thread::sleep(Duration::from_millis(100));
    }
    false
}

#[allow(clippy::zombie_processes)]
fn run_parent_death_helper_if_requested() {
    if std::env::var_os("TEMPYR_TEST_PARENT_DEATH_HELPER").is_none() {
        return;
    }

    let info_path = PathBuf::from(std::env::var_os("TEMPYR_TEST_PARENT_DEATH_INFO").unwrap());
    let stderr_path = PathBuf::from(std::env::var_os("TEMPYR_TEST_PARENT_DEATH_STDERR").unwrap());
    let tempyr_bin = PathBuf::from(std::env::var_os("TEMPYR_TEST_TEMPYR_BIN").unwrap());
    let cwd = PathBuf::from(std::env::var_os("TEMPYR_TEST_PARENT_DEATH_CWD").unwrap());
    let stderr_file = fs::File::create(&stderr_path).unwrap();

    // This helper must exit while tempyr is still alive so the MCP process observes parent death.
    let child = ProcessCommand::new(tempyr_bin)
        .arg("--mcp")
        .current_dir(&cwd)
        .stdin(Stdio::inherit())
        .stdout(Stdio::null())
        .stderr(Stdio::from(stderr_file))
        .spawn()
        .unwrap();

    let info = ParentDeathHelperInfo {
        pid: child.id(),
        stderr_path,
        created_at: process_creation_time(child.id()),
    };
    fs::write(info_path, serde_json::to_vec(&info).unwrap()).unwrap();
    thread::sleep(Duration::from_millis(500));
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
            "`--mcp` must be the first argument, and if using `--project-root` it must be provided with a valid value. Launch the MCP server with `tempyr --mcp` or `tempyr --mcp --project-root <path>`.",
        ));
}

#[test]
fn test_mcp_mode_starts_on_stdio() {
    let tmp = TempDir::new().unwrap();
    let tempyr_bin = assert_cmd::cargo::cargo_bin("tempyr");
    let mut child = ProcessCommand::new(tempyr_bin)
        .arg("--mcp")
        .current_dir(tmp.path())
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
fn test_mcp_exits_cleanly_on_stdin_eof_before_initialize() {
    let tmp = TempDir::new().unwrap();
    let mut child = spawn_mcp_child(tmp.path());

    drop(child.stdin.take());

    let status = wait_for_child_exit(&mut child, Duration::from_secs(5));
    assert_eq!(status.code(), Some(0));

    let stderr = read_child_stderr(&mut child);
    assert!(
        stderr.contains("tempyr shutting down: stdin EOF"),
        "{stderr}"
    );
}

#[test]
fn test_mcp_exits_cleanly_on_stdin_eof_after_initialize() {
    let tmp = TempDir::new().unwrap();
    let mut child = spawn_mcp_child(tmp.path());
    initialize_mcp_session(&mut child);

    drop(child.stdin.take());

    let status = wait_for_child_exit(&mut child, Duration::from_secs(5));
    assert_eq!(status.code(), Some(0));

    let stderr = read_child_stderr(&mut child);
    assert!(
        stderr.contains("tempyr shutting down: stdin EOF"),
        "{stderr}"
    );
}

#[test]
fn test_mcp_uses_tempyr_project_root_env_when_server_cwd_is_wrong() {
    let tmp = TempDir::new().unwrap();
    let project_root = tmp.path().join("project");
    let launch_root = tmp.path().join("launch");
    fs::create_dir(&project_root).unwrap();
    fs::create_dir(&launch_root).unwrap();

    tempyr()
        .current_dir(&project_root)
        .arg("init")
        .assert()
        .success();

    let mut child = spawn_mcp_child_with_project_root(&launch_root, &project_root);
    initialize_mcp_session(&mut child);

    let response = call_mcp_tool(&mut child, 2, "graph_validate", serde_json::json!({}));
    assert_eq!(response["id"], 2);
    assert!(tool_result_text(&response).starts_with("Graph is valid."));

    drop(child.stdin.take());
    let status = wait_for_child_exit(&mut child, Duration::from_secs(5));
    assert_eq!(status.code(), Some(0));
}

#[test]
fn test_mcp_uses_project_root_arg_when_server_cwd_is_wrong() {
    let tmp = TempDir::new().unwrap();
    let project_root = tmp.path().join("project");
    let launch_root = tmp.path().join("launch");
    fs::create_dir(&project_root).unwrap();
    fs::create_dir(&launch_root).unwrap();

    tempyr()
        .current_dir(&project_root)
        .arg("init")
        .assert()
        .success();

    let mut child = spawn_mcp_child_with_project_root_arg(&launch_root, &project_root);
    initialize_mcp_session(&mut child);

    let response = call_mcp_tool(&mut child, 2, "graph_validate", serde_json::json!({}));
    assert_eq!(response["id"], 2);
    assert!(tool_result_text(&response).starts_with("Graph is valid."));

    drop(child.stdin.take());
    let status = wait_for_child_exit(&mut child, Duration::from_secs(5));
    assert_eq!(status.code(), Some(0));
}

#[test]
fn test_mcp_uses_client_roots_when_server_cwd_is_wrong() {
    let tmp = TempDir::new().unwrap();
    let project_root = tmp.path().join("project");
    let launch_root = tmp.path().join("launch");
    fs::create_dir(&project_root).unwrap();
    fs::create_dir(&launch_root).unwrap();

    tempyr()
        .current_dir(&project_root)
        .arg("init")
        .assert()
        .success();

    let mut child = spawn_mcp_child(&launch_root);
    initialize_mcp_session_with_roots(&mut child, &[&project_root]);

    let response = call_mcp_tool(&mut child, 2, "graph_validate", serde_json::json!({}));
    assert_eq!(response["id"], 2);
    assert!(tool_result_text(&response).starts_with("Graph is valid."));

    drop(child.stdin.take());
    let status = wait_for_child_exit(&mut child, Duration::from_secs(5));
    assert_eq!(status.code(), Some(0));
}

#[test]
fn test_mcp_relative_project_root_arg_uses_client_roots_when_server_cwd_is_wrong() {
    let tmp = TempDir::new().unwrap();
    let workspace_root = tmp.path().join("workspace");
    let project_root = workspace_root.join("project");
    let launch_root = tmp.path().join("launch");
    fs::create_dir(&workspace_root).unwrap();
    fs::create_dir(&project_root).unwrap();
    fs::create_dir(&launch_root).unwrap();

    tempyr()
        .current_dir(&project_root)
        .arg("init")
        .assert()
        .success();

    let mut child = spawn_mcp_child_with_project_root_arg(&launch_root, Path::new("project"));
    initialize_mcp_session_with_roots(&mut child, &[&workspace_root]);

    let response = call_mcp_tool(&mut child, 2, "graph_validate", serde_json::json!({}));
    assert_eq!(response["id"], 2);
    assert!(tool_result_text(&response).starts_with("Graph is valid."));

    drop(child.stdin.take());
    let status = wait_for_child_exit(&mut child, Duration::from_secs(5));
    assert_eq!(status.code(), Some(0));
}

#[test]
fn test_mcp_relative_project_root_arg_prefers_client_roots_over_launch_project() {
    let tmp = TempDir::new().unwrap();
    let launch_root = tmp.path().join("launch-project");
    let client_root = tmp.path().join("client-project");
    fs::create_dir(&launch_root).unwrap();
    fs::create_dir(&client_root).unwrap();

    tempyr()
        .current_dir(&launch_root)
        .arg("init")
        .assert()
        .success();
    tempyr()
        .current_dir(&client_root)
        .arg("init")
        .assert()
        .success();

    fs::write(
        client_root.join("graph/features/client-only.md"),
        "---\nid: client-only\ntype: feature\nstatus: draft\nowner: caleb\nedges: []\n---\n# Client Only\n",
    )
    .unwrap();

    let mut child = spawn_mcp_child_with_project_root_arg(&launch_root, Path::new("."));
    initialize_mcp_session_with_roots(&mut child, &[&client_root]);

    let response = call_mcp_tool(&mut child, 2, "graph_stats", serde_json::json!({}));
    assert_eq!(response["id"], 2);
    let stats: serde_json::Value = serde_json::from_str(tool_result_text(&response)).unwrap();
    assert_eq!(stats["node_count"], 1);
    assert_eq!(stats["nodes_by_type"]["feature"], 1);

    drop(child.stdin.take());
    let status = wait_for_child_exit(&mut child, Duration::from_secs(5));
    assert_eq!(status.code(), Some(0));
}

#[test]
fn test_mcp_project_root_arg_wins_over_stale_env() {
    let tmp = TempDir::new().unwrap();
    let project_root = tmp.path().join("project");
    let launch_root = tmp.path().join("launch");
    let stale_root = tmp.path().join("stale");
    fs::create_dir(&project_root).unwrap();
    fs::create_dir(&launch_root).unwrap();
    fs::create_dir(&stale_root).unwrap();

    tempyr()
        .current_dir(&project_root)
        .arg("init")
        .assert()
        .success();

    let mut child =
        spawn_mcp_child_with_project_root_arg_and_bad_env(&launch_root, &project_root, &stale_root);
    initialize_mcp_session(&mut child);

    let response = call_mcp_tool(&mut child, 2, "graph_validate", serde_json::json!({}));
    assert_eq!(response["id"], 2);
    assert!(tool_result_text(&response).starts_with("Graph is valid."));

    drop(child.stdin.take());
    let status = wait_for_child_exit(&mut child, Duration::from_secs(5));
    assert_eq!(status.code(), Some(0));
}

#[test]
#[ignore]
fn test_mcp_parent_death_helper() {
    run_parent_death_helper_if_requested();
}

#[test]
fn test_mcp_exits_on_parent_death() {
    let tmp = TempDir::new().unwrap();
    let helper_info_path = tmp.path().join("parent-death.json");
    let helper_stderr_path = tmp.path().join("tempyr-parent-death.stderr");
    let tempyr_bin = tempyr_bin();

    let mut helper = ProcessCommand::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "test_mcp_parent_death_helper",
            "--nocapture",
        ])
        .env("TEMPYR_TEST_PARENT_DEATH_HELPER", "1")
        .env("TEMPYR_TEST_PARENT_DEATH_INFO", &helper_info_path)
        .env("TEMPYR_TEST_PARENT_DEATH_STDERR", &helper_stderr_path)
        .env("TEMPYR_TEST_TEMPYR_BIN", &tempyr_bin)
        .env("TEMPYR_TEST_PARENT_DEATH_CWD", tmp.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let keep_stdin_open = helper.stdin.take().unwrap();
    let helper_status = wait_for_child_exit(&mut helper, Duration::from_secs(20));
    assert!(
        helper_status.success(),
        "helper failed with status {helper_status}"
    );

    let info: ParentDeathHelperInfo =
        serde_json::from_slice(&fs::read(&helper_info_path).unwrap()).unwrap();

    let exited = wait_for_pid_exit(&info, Duration::from_secs(20));
    drop(keep_stdin_open);

    let stderr = fs::read_to_string(&info.stderr_path).unwrap();
    assert!(
        exited,
        "process {} did not exit within 20s\nstderr:\n{}",
        info.pid, stderr
    );
    assert!(
        stderr.contains("tempyr shutting down: parent pid"),
        "{stderr}"
    );
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
fn test_search_builds_structural_index_when_missing() {
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

#[test]
fn test_doctor_text_output_lists_paths_and_provider() {
    let tmp = TempDir::new().unwrap();
    init_project(&tmp);

    tempyr()
        .current_dir(tmp.path())
        .arg("doctor")
        .assert()
        .success()
        .stdout(predicate::str::contains("Project"))
        .stdout(predicate::str::contains("Embedding"))
        .stdout(predicate::str::contains("Config files"))
        .stdout(predicate::str::contains("schema.toml"))
        .stdout(predicate::str::contains("provider:"))
        .stdout(predicate::str::contains("value never displayed"));
}

#[test]
fn test_doctor_json_output_is_structured() {
    let tmp = TempDir::new().unwrap();
    init_project(&tmp);

    let output = tempyr()
        .current_dir(tmp.path())
        .args(["--json", "doctor"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let report: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert!(report["tempyr_version"].is_string());
    assert!(report["embedding"]["provider"].is_string());
    assert!(report["config_files"].is_array());
    assert!(report["env_files"].is_array());
    assert!(report["project"]["root"].is_string());

    // The report MUST surface only the env var name, never the value.
    assert!(report["embedding"]["api_key_env_var"].is_string());
    assert!(report["embedding"]["api_key_set"].is_boolean());
    assert!(report.get("api_key").is_none());
    assert!(
        report["embedding"].get("api_key").is_none(),
        "report.embedding.api_key must not exist (would risk leaking the secret value)"
    );
}

#[test]
fn test_doctor_does_not_leak_api_key_value() {
    let tmp = TempDir::new().unwrap();
    init_project(&tmp);

    let secret = "sk-doctor-test-secret-must-not-leak";
    let output = tempyr()
        .current_dir(tmp.path())
        .env("VOYAGE_API_KEY", secret)
        .args(["--json", "doctor"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let raw = String::from_utf8(output).unwrap();
    assert!(
        !raw.contains(secret),
        "doctor JSON output leaked the API key value"
    );
    let report: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(
        report["embedding"]["api_key_set"],
        serde_json::Value::Bool(true)
    );
    assert!(
        report["embedding"].get("api_key").is_none(),
        "report.embedding.api_key must not exist (would risk leaking the secret value)"
    );
}

#[test]
fn test_mcp_system_doctor_returns_report_without_api_key() {
    let tmp = TempDir::new().unwrap();
    init_project(&tmp);

    let secret = "sk-mcp-doctor-secret-must-not-leak";
    let mut child = ProcessCommand::new(tempyr_bin())
        .arg("--mcp")
        .current_dir(tmp.path())
        .env("VOYAGE_API_KEY", secret)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    initialize_mcp_session(&mut child);

    let response = call_mcp_tool(&mut child, 2, "system_doctor", serde_json::json!({}));
    assert_eq!(response["id"], 2);
    let text = tool_result_text(&response);
    assert!(
        !text.contains(secret),
        "system_doctor leaked the API key value: {text}"
    );

    let report: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(report["embedding"]["api_key_env_var"], "VOYAGE_API_KEY");
    assert_eq!(
        report["embedding"]["api_key_set"],
        serde_json::Value::Bool(true)
    );
    assert!(
        report["embedding"].get("api_key").is_none(),
        "report.embedding.api_key must not exist (would risk leaking the secret value)"
    );
    assert!(report["config_files"].is_array());
    assert!(report["project"]["root"].is_string());

    drop(child.stdin.take());
    let status = wait_for_child_exit(&mut child, Duration::from_secs(5));
    assert_eq!(status.code(), Some(0));
}
