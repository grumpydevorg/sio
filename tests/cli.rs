use serde_json::Value;
use std::io::Write;
use std::process::{Command, ExitStatus, Output, Stdio};

fn sio() -> Command {
    Command::new(env!("CARGO_BIN_EXE_sio"))
}

fn output(command: &mut Command) -> Output {
    command.output().expect("sio should run")
}

/// Run `sio stdio` with `args`, feed `input`, and parse every stdout line.
fn stdio(args: &[&str], input: &str) -> (ExitStatus, Vec<Value>, Vec<u8>) {
    let mut child = sio()
        .arg("stdio")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let result = child.wait_with_output().unwrap();
    let responses = result
        .stdout
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).unwrap())
        .collect();
    (result.status, responses, result.stderr)
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

    let raw = output(sio().args([
        "transact",
        "--loopback",
        "--port",
        "loop://",
        "--hex",
        "00ff",
        "--bytes",
        "2",
    ]));
    assert!(raw.status.success());
    assert_eq!(raw.stdout, [0x00, 0xff]);
}

#[test]
fn transact_fails_when_the_pattern_never_arrives() {
    let result = output(sio().args([
        "transact",
        "--loopback",
        "--port",
        "loop://",
        "--text",
        "x",
        "--expect",
        "nomatch",
        "--timeout-ms",
        "100",
    ]));
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("timed out"));
    assert_eq!(result.stdout, b"x");
}

#[test]
fn stdio_keeps_one_session_and_one_json_response_per_command() {
    let (status, responses, stderr) = stdio(
        &["--loopback"],
        concat!(
            "{\"cmd\":\"open\",\"port\":\"loop://\"}\n",
            "{\"cmd\":\"write\",\"data_b64\":\"AP8=\"}\n",
            "{\"cmd\":\"read\",\"bytes\":2,\"timeout_ms\":1000}\n",
            "{\"cmd\":\"close\"}\n",
        ),
    );
    assert!(status.success());
    assert!(stderr.is_empty());
    assert_eq!(responses.len(), 4);
    let session = &responses[0]["session_id"];
    assert!(
        responses
            .iter()
            .all(|response| &response["session_id"] == session)
    );
    assert!(
        responses
            .iter()
            .all(|response| response["protocol_version"] == 2)
    );
    assert_eq!(responses[2]["result"]["data_b64"], "AP8=");
}

#[test]
fn stdio_echoes_request_ids_on_success_and_failure() {
    let (status, responses, _) = stdio(
        &["--loopback"],
        concat!(
            "{\"id\":\"a1\",\"cmd\":\"open\",\"port\":\"loop://\"}\n",
            "{\"id\":7,\"cmd\":\"read\",\"bytes\":0}\n",
            "{\"cmd\":\"drain\",\"id\":\"late\"}\n",
            "not json\n",
            "{\"cmd\":\"close\"}\n",
        ),
    );
    assert!(status.success());
    assert_eq!(responses.len(), 5);
    assert_eq!(responses[0]["id"], "a1");
    assert_eq!(responses[0]["ok"], true);
    assert_eq!(responses[1]["id"], 7);
    assert_eq!(responses[1]["command"], "read");
    assert_eq!(responses[1]["error"]["code"], "invalid_request");
    assert_eq!(responses[2]["id"], "late");
    assert!(responses[3].get("id").is_none());
    assert_eq!(responses[3]["command"], "parse");
    assert!(responses[4].get("id").is_none());
}

#[test]
fn invalid_commands_are_structured_and_fail_fast_is_nonzero() {
    let (status, responses, _) = stdio(
        &["--loopback", "--fail-fast"],
        "{\"cmd\":\"open\",\"port\":\"loop://\",\"bogus\":true}\n{\"cmd\":\"list\"}\n",
    );
    assert!(!status.success());
    assert_eq!(responses.len(), 1);
    assert_eq!(responses[0]["ok"], false);
    assert_eq!(responses[0]["error"]["code"], "invalid_request");
    assert_eq!(responses[0]["error"]["retryable"], false);
}

#[test]
fn stdio_cancel_jumps_queued_work_and_replies_in_order() {
    use std::io::{BufRead, BufReader};
    use std::time::{Duration, Instant};

    let mut child = sio()
        .args(["stdio", "--loopback"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut next = || {
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        serde_json::from_str::<Value>(&line).unwrap()
    };
    stdin
        .write_all(
            concat!(
                "{\"cmd\":\"open\",\"port\":\"loop://\"}\n",
                "{\"id\":\"a\",\"cmd\":\"expect\",\"pattern\":\"never\",\"timeout_ms\":20000}\n",
                "{\"id\":\"q\",\"cmd\":\"write\",\"text\":\"x\"}\n",
            )
            .as_bytes(),
        )
        .unwrap();
    assert_eq!(next()["command"], "open");
    // The expect starts right after the open reply; a cancel sent before it
    // starts would (correctly) find nothing running.
    std::thread::sleep(Duration::from_millis(300));
    let start = Instant::now();
    stdin
        .write_all(b"{\"id\":\"c\",\"cmd\":\"cancel\",\"target\":\"a\"}\n")
        .unwrap();
    drop(stdin);
    let responses = [next(), next(), next()];
    assert!(start.elapsed() < Duration::from_secs(5));
    assert!(child.wait().unwrap().success());
    let ids: Vec<_> = responses.iter().map(|response| &response["id"]).collect();
    assert_eq!(ids, ["a", "q", "c"]);
    assert_eq!(responses[0]["result"]["end"], "cancelled");
    assert_eq!(responses[1]["ok"], true, "queued write still runs");
    assert_eq!(responses[2]["result"]["cancelled"], true);
}

#[test]
fn a_flood_behind_a_running_wait_is_bounded_and_cancel_still_lands() {
    use base64::Engine;
    use std::io::{BufRead, BufReader};

    // 40 writes of 1 MiB each: ~56 MiB of request lines, far over the
    // 16 MiB queue budget, sent while an expect is waiting.
    let payload = base64::engine::general_purpose::STANDARD.encode(vec![b'x'; 1024 * 1024]);
    let mut child = sio()
        .args(["stdio", "--loopback"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut next = || {
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        serde_json::from_str::<Value>(&line).unwrap()
    };
    stdin
        .write_all(
            concat!(
                "{\"cmd\":\"open\",\"port\":\"loop://\"}\n",
                "{\"id\":\"wait\",\"cmd\":\"expect\",\"pattern\":\"never\",\"timeout_ms\":30000}\n",
            )
            .as_bytes(),
        )
        .unwrap();
    assert_eq!(next()["command"], "open");
    std::thread::sleep(std::time::Duration::from_millis(200));
    for index in 0..40 {
        writeln!(
            stdin,
            r#"{{"id":"w{index}","cmd":"write","data_b64":"{payload}"}}"#
        )
        .unwrap();
    }
    stdin
        .write_all(b"{\"id\":\"c\",\"cmd\":\"cancel\",\"target\":\"wait\"}\n")
        .unwrap();
    drop(stdin);

    let waited = next();
    assert_eq!(waited["id"], "wait");
    assert_eq!(waited["result"]["end"], "cancelled");
    let writes: Vec<Value> = (0..40).map(|_| next()).collect();
    let cancel = next();
    assert!(child.wait().unwrap().success());
    assert_eq!(cancel["result"]["cancelled"], true);

    let full: Vec<_> = writes
        .iter()
        .filter(|reply| reply["error"]["code"] == "queue_full")
        .collect();
    let written = writes.iter().filter(|reply| reply["ok"] == true).count();
    assert_eq!(
        full.len() + written,
        40,
        "every write gets exactly one reply"
    );
    assert!((1..=12).contains(&written), "{written} writes fit 16 MiB");
    assert!(full.iter().all(|reply| reply["error"]["retryable"] == true));
    for (index, reply) in writes.iter().enumerate() {
        assert_eq!(reply["id"], format!("w{index}"), "replies stay in order");
    }
}

#[test]
fn a_flood_of_malformed_lines_behind_a_wait_gets_small_replies() {
    use std::io::{BufRead, BufReader};

    // Unknown command and field names near 1 MiB each: their serde errors
    // quote the name, so uncapped replies would be as large as the input.
    let name = "n".repeat(1024 * 1024);
    let mut child = sio()
        .args(["stdio", "--loopback"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut next = || {
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        line
    };
    stdin
        .write_all(
            concat!(
                "{\"cmd\":\"open\",\"port\":\"loop://\"}\n",
                "{\"id\":\"wait\",\"cmd\":\"expect\",\"pattern\":\"never\",\"timeout_ms\":30000}\n",
            )
            .as_bytes(),
        )
        .unwrap();
    assert!(next().contains("\"open\""));
    std::thread::sleep(std::time::Duration::from_millis(200));
    for index in 0..30 {
        if index % 2 == 0 {
            writeln!(stdin, r#"{{"id":{index},"cmd":"{name}"}}"#).unwrap();
        } else {
            writeln!(stdin, r#"{{"id":{index},"cmd":"close","{name}":1}}"#).unwrap();
        }
    }
    stdin
        .write_all(b"{\"cmd\":\"cancel\",\"target\":\"wait\"}\n")
        .unwrap();
    drop(stdin);

    let waited: Value = serde_json::from_str(&next()).unwrap();
    assert_eq!(waited["result"]["end"], "cancelled");
    for index in 0..30 {
        let line = next();
        assert!(line.len() < 2048, "reply {index} is {} bytes", line.len());
        let reply: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(reply["id"], index);
        assert_eq!(reply["error"]["code"], "invalid_request");
    }
    let cancel: Value = serde_json::from_str(&next()).unwrap();
    assert_eq!(cancel["result"]["cancelled"], true);
    assert!(child.wait().unwrap().success());
}

#[test]
fn an_oversized_payload_is_rejected_with_its_id() {
    let payload = "A".repeat(1_500_000);
    let line = format!("{{\"id\":\"big\",\"cmd\":\"write\",\"data_b64\":\"{payload}\"}}\n");
    let (_, responses, _) = stdio(&["--loopback"], &line);
    assert_eq!(responses[0]["id"], "big");
    assert_eq!(responses[0]["command"], "write");
    assert_eq!(responses[0]["error"]["code"], "invalid_request");
}

#[test]
fn invalid_settings_never_reach_a_device() {
    let usage = output(sio().args([
        "transact",
        "--port",
        "/dev/this-must-not-be-opened",
        "--bits",
        "9",
        "--text",
        "x",
    ]));
    assert_eq!(usage.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&usage.stderr).contains("bits must be 5, 6, 7, or 8"));

    let (_, responses, _) = stdio(
        &[],
        "{\"cmd\":\"open\",\"port\":\"/dev/this-must-not-be-opened\",\"bits\":9}\n",
    );
    assert_eq!(responses[0]["command"], "open");
    assert_eq!(responses[0]["error"]["code"], "invalid_request");
}

#[test]
fn a_missing_device_is_reported_as_port_not_found() {
    let result = output(sio().args([
        "transact",
        "--port",
        "/dev/sio-test-no-such-port",
        "--text",
        "x",
        "--json",
    ]));
    assert!(!result.status.success());
    let response: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(response["command"], "open");
    assert_eq!(response["error"]["code"], "port_not_found");
}

#[test]
fn read_only_rejects_mutations_but_keeps_stream_alive() {
    let (status, responses, _) = stdio(
        &["--loopback", "--read-only"],
        concat!(
            "{\"cmd\":\"open\",\"port\":\"loop://\"}\n",
            "{\"cmd\":\"write\",\"text\":\"x\"}\n",
            "{\"cmd\":\"modem\"}\n"
        ),
    );
    assert!(status.success());
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
