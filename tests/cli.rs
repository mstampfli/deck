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

    assert_eq!(json[0]["commands"][0]["safety"]["direct_argv"], true);
    assert_eq!(json[0]["commands"][1]["safety"]["uses_shell"], true);
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
