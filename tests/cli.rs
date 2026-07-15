use std::process::{Command, Output};

fn deck(state_home: &std::path::Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_deck"))
        .args(args)
        .env("XDG_STATE_HOME", state_home)
        .env("XDG_DATA_HOME", state_home.join("data"))
        .output()
        .expect("run deck")
}

fn fixture_project(root: &std::path::Path) {
    std::fs::create_dir_all(root).unwrap();
    std::fs::write(
        root.join("deck.toml"),
        r#"name = "fixture"

[commands.hello]
argv = ["printf", "hello"]

[commands.shell]
cmd = "printf hello"

[commands.fail]
argv = ["false"]

[sandbox.locked]
backend = "bwrap"
network = false
readonly_project = true
writable = ["./tmp"]
env = ["PATH"]
timeout_seconds = 5
allow_shell = false
"#,
    )
    .unwrap();
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "status={:?}\nstdout={}\nstderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn commands_json_includes_safety_metadata() {
    let state = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    fixture_project(project.path());
    assert_success(&deck(
        state.path(),
        &["scan", project.path().to_str().unwrap()],
    ));

    let output = deck(state.path(), &["commands", "fixture", "--json"]);
    assert_success(&output);
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();

    let commands = json[0]["commands"].as_array().unwrap();
    let safety = |name: &str| {
        &commands
            .iter()
            .find(|command| command["name"] == name)
            .unwrap_or_else(|| panic!("missing command {name}"))["safety"]
    };
    assert_eq!(safety("hello")["direct_argv"], true);
    assert_eq!(safety("shell")["uses_shell"], true);
}

#[test]
fn tasks_can_be_added_and_listed() {
    let state = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    fixture_project(project.path());
    assert_success(&deck(
        state.path(),
        &["scan", project.path().to_str().unwrap()],
    ));
    assert_success(&deck(
        state.path(),
        &[
            "tasks",
            "add",
            "fixture",
            "ship",
            "--title",
            "Ship the first useful version",
            "--status",
            "doing",
        ],
    ));

    let output = deck(state.path(), &["tasks", "list", "fixture", "--json"]);
    assert_success(&output);
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();

    assert_eq!(json["tasks"][0]["name"], "ship");
    assert_eq!(json["tasks"][0]["status"], "doing");
}

#[test]
fn task_edits_print_human_confirmations_by_default() {
    let state = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    fixture_project(project.path());
    assert_success(&deck(
        state.path(),
        &["scan", project.path().to_str().unwrap()],
    ));

    let output = deck(
        state.path(),
        &["tasks", "add", "fixture", "ship", "--title", "Ship it"],
    );
    assert_success(&output);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        stdout.starts_with("add-task fixture: wrote "),
        "unexpected stdout: {stdout}"
    );
    assert!(!stdout.contains('{'), "expected no JSON: {stdout}");
}

#[test]
fn failed_run_exits_nonzero_with_a_single_json_document() {
    let state = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    fixture_project(project.path());
    assert_success(&deck(
        state.path(),
        &["scan", project.path().to_str().unwrap()],
    ));

    let output = deck(state.path(), &["run", "fixture", "fail", "--json"]);
    assert!(!output.status.success());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();

    assert_eq!(json["ok"], false);
    assert_eq!(json["exit_code"], 1);
}

#[test]
fn global_json_flag_wraps_errors_in_the_envelope() {
    let state = tempfile::tempdir().unwrap();

    let output = deck(state.path(), &["--json", "context", "missing"]);
    assert!(!output.status.success());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();

    assert_eq!(json["ok"], false);
    assert_eq!(json["error"]["kind"], "unknown_project");
}

#[test]
fn every_list_surface_renders_both_forms() {
    let state = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    fixture_project(project.path());
    assert_success(&deck(
        state.path(),
        &["scan", project.path().to_str().unwrap()],
    ));

    let json_out = deck(state.path(), &["list", "--json"]);
    assert_success(&json_out);
    let json: serde_json::Value = serde_json::from_slice(&json_out.stdout).unwrap();
    assert_eq!(json[0]["name"], "fixture");

    let human_out = deck(state.path(), &["list"]);
    assert_success(&human_out);
    let stdout = String::from_utf8_lossy(&human_out.stdout);
    assert!(stdout.contains("fixture"), "unexpected stdout: {stdout}");
    assert!(!stdout.contains('{'), "expected no JSON: {stdout}");
}

#[test]
fn sandbox_plan_blocks_shell_under_locked_profile() {
    let state = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    fixture_project(project.path());
    assert_success(&deck(
        state.path(),
        &["scan", project.path().to_str().unwrap()],
    ));

    let output = deck(
        state.path(),
        &[
            "sandbox",
            "plan",
            "fixture",
            "shell",
            "--profile",
            "locked",
            "--json",
        ],
    );
    assert!(!output.status.success());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();

    assert_eq!(json["error"]["kind"], "sandbox_policy_denied");
}

#[test]
fn summary_renders_for_humans_and_agents() {
    let state = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    fixture_project(project.path());
    assert_success(&deck(
        state.path(),
        &["scan", project.path().to_str().unwrap()],
    ));

    let json_out = deck(state.path(), &["summary", "fixture", "--json"]);
    assert_success(&json_out);
    let json: serde_json::Value = serde_json::from_slice(&json_out.stdout).unwrap();
    assert_eq!(json["ok"], true);
    assert_eq!(json["context"]["project"]["name"], "fixture");
    assert!(
        json["commands"]
            .as_array()
            .unwrap()
            .iter()
            .any(|command| command["name"] == "hello" && command["safety"]["direct_argv"] == true)
    );

    let human_out = deck(state.path(), &["summary", "fixture"]);
    assert_success(&human_out);
    let stdout = String::from_utf8_lossy(&human_out.stdout);
    assert!(stdout.contains("commands:"), "unexpected stdout: {stdout}");
    assert!(
        stdout.contains("sandbox profiles:"),
        "unexpected stdout: {stdout}"
    );
    assert!(!stdout.contains('{'), "expected no JSON: {stdout}");
}

#[test]
fn agent_session_contains_context_and_safety() {
    let state = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    fixture_project(project.path());
    assert_success(&deck(
        state.path(),
        &["scan", project.path().to_str().unwrap()],
    ));

    let output = deck(state.path(), &["agent", "session", "start", "fixture"]);
    assert_success(&output);
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();

    assert_eq!(json["ok"], true);
    assert_eq!(json["context"]["project"]["name"], "fixture");
    assert_eq!(json["commands"][0]["safety"]["direct_argv"], true);
}
