use serde_json::Value;
use std::io::Write;
use std::process::{Command, Output, Stdio};

fn sio() -> Command {
    Command::new(env!("CARGO_BIN_EXE_sio"))
}

fn output(command: &mut Command) -> Output {
    command.output().expect("sio should run")
}

#[test]
fn help_exposes_only_implemented_local_surfaces() {
    let result = output(sio().arg("--help"));
    assert!(result.status.success());
    let stdout = String::from_utf8(result.stdout).unwrap();
    for command in ["list", "terminal", "transact", "stdio", "skill"] {
        assert!(stdout.contains(command), "missing {command} from help");
    }
    assert!(!stdout.contains("serve"));
    assert!(!stdout.contains("HTTP"));
}

#[test]
fn embedded_skill_round_trips_and_writes_safely() {
    let expected = include_bytes!("../skills/sio/SKILL.md");
    let printed = output(sio().arg("skill"));
    assert!(printed.status.success());
    assert_eq!(printed.stdout, expected);

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("SKILL.md");
    assert!(
        output(sio().args(["skill", "--output"]).arg(&path))
            .status
            .success()
    );
    assert_eq!(std::fs::read(&path).unwrap(), expected);
    assert!(
        output(sio().args(["skill", "--check"]).arg(&path))
            .status
            .success()
    );

    std::fs::write(&path, b"different\n").unwrap();
    assert!(
        !output(sio().args(["skill", "--output"]).arg(&path))
            .status
            .success()
    );
    assert_eq!(std::fs::read(&path).unwrap(), b"different\n");
    assert!(
        output(sio().args(["skill", "--output"]).arg(&path).arg("--force"))
            .status
            .success()
    );
    assert_eq!(std::fs::read(&path).unwrap(), expected);
}

#[test]
fn transact_round_trips_binary_without_utf8_loss() {
    let result = output(sio().args([
        "transact",
        "--loopback",
        "--port",
        "loop://",
        "--hex",
        "00ff",
        "--bytes",
        "2",
        "--json",
    ]));
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let response: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(response["ok"], true);
    assert_eq!(response["command"], "read");
    assert_eq!(response["result"]["data_b64"], "AP8=");
    assert!(response["result"].get("text").is_none());
}

#[test]
fn stdio_keeps_one_session_and_one_json_response_per_command() {
    let mut child = sio()
        .args(["stdio", "--loopback"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(
            br#"{"cmd":"open","port":"loop://"}
{"cmd":"write","data_b64":"AP8="}
{"cmd":"read","bytes":2,"timeout_ms":1000}
{"cmd":"close"}
"#,
        )
        .unwrap();
    drop(child.stdin.take());
    let result = child.wait_with_output().unwrap();
    assert!(result.status.success());
    assert!(result.stderr.is_empty());
    let responses: Vec<Value> = result
        .stdout
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).unwrap())
        .collect();
    assert_eq!(responses.len(), 4);
    let session = responses[0]["session_id"].clone();
    assert!(
        responses
            .iter()
            .all(|response| response["session_id"] == session)
    );
    assert_eq!(responses[2]["result"]["data_b64"], "AP8=");
}

#[test]
fn invalid_commands_are_structured_and_fail_fast_is_nonzero() {
    let mut child = sio()
        .args(["stdio", "--loopback", "--fail-fast"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"{\"cmd\":\"open\",\"port\":\"loop://\",\"bogus\":true}\n{\"cmd\":\"list\"}\n")
        .unwrap();
    drop(child.stdin.take());
    let result = child.wait_with_output().unwrap();
    assert!(!result.status.success());
    let lines: Vec<_> = result
        .stdout
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .collect();
    assert_eq!(lines.len(), 1);
    let response: Value = serde_json::from_slice(lines[0]).unwrap();
    assert_eq!(response["ok"], false);
    assert_eq!(response["error"]["code"], "invalid_request");
    assert_eq!(response["error"]["retryable"], false);
}

#[test]
fn validation_happens_before_opening_a_device() {
    let result = output(sio().args([
        "transact",
        "--port",
        "/dev/this-must-not-be-opened",
        "--bits",
        "9",
        "--text",
        "x",
        "--json",
    ]));
    assert!(!result.status.success());
    let response: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(response["command"], "open");
    assert_eq!(response["error"]["code"], "invalid_request");
    assert!(
        response["error"]["message"]
            .as_str()
            .unwrap()
            .contains("bits")
    );
}

#[test]
fn read_only_rejects_mutations_but_keeps_stream_alive() {
    let input = concat!(
        "{\"cmd\":\"open\",\"port\":\"loop://\"}\n",
        "{\"cmd\":\"write\",\"text\":\"x\"}\n",
        "{\"cmd\":\"modem\"}\n"
    );
    let mut child = sio()
        .args(["stdio", "--loopback", "--read-only"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    drop(child.stdin.take());
    let result = child.wait_with_output().unwrap();
    assert!(result.status.success());
    let responses: Vec<Value> = result
        .stdout
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).unwrap())
        .collect();
    assert_eq!(responses[1]["error"]["code"], "read_only");
    assert_eq!(responses[2]["ok"], true);
}

#[test]
fn terminal_refuses_non_terminal_stdio() {
    let result = output(sio().args(["terminal", "--loopback", "--port", "loop://"]));
    assert!(!result.status.success());
    assert!(
        String::from_utf8_lossy(&result.stderr)
            .contains("terminal mode requires terminal stdin and stdout")
    );
}
