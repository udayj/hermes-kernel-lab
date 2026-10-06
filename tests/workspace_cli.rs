//! Synthetic provider messages exercising the compiled CLI and real macOS tools.
#![cfg(target_os = "macos")]
use rustix::process::{Pid, Signal, kill_process, test_kill_process};
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    os::unix::process::ExitStatusExt,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};
use tempfile::tempdir;

fn launch(root: &std::path::Path, resume: Option<&std::path::Path>) -> Child {
    let mut command = Command::new(env!("CARGO_BIN_EXE_hermes-kernel-lab"));
    command
        .current_dir(root)
        .env("HOME", root.join("home"))
        .env_remove("ANTHROPIC_API_KEY")
        .args([
            "--workspace",
            "work",
            "--allow-workspace-writes",
            "--allow-shell",
            "--offline-script",
            "script.json",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(path) = resume {
        command.arg("--resume-session").arg(path);
    }
    command.spawn().unwrap()
}

#[test]
fn offline_workflow_and_ctrl_c_cleanup() {
    let root = tempdir().unwrap();
    fs::create_dir(root.path().join("home")).unwrap();
    fs::create_dir(root.path().join("work")).unwrap();
    fs::write(root.path().join("work/source.txt"), "amber\n").unwrap();
    // Native workspace startup has no shell/sandbox prerequisite.
    assert!(
        Command::new(env!("CARGO_BIN_EXE_hermes-kernel-lab"))
            .current_dir(root.path())
            .args(["--workspace", "work"])
            .stdin(Stdio::null())
            .status()
            .unwrap()
            .success()
    );
    fs::write(
        root.path().join("script.json"),
        include_bytes!("../examples/offline-workspace.json"),
    )
    .unwrap();
    let mut child = launch(root.path(), None);
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"Run the synthetic workflow\n/exit\n")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read(root.path().join("work/output.txt")).unwrap(),
        b"blue\n"
    );
    let checkpoint = fs::read_dir(root.path().join("home/.hermes-kernel-lab/sessions"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.extension().is_some_and(|ext| ext == "json"))
        .unwrap();
    let previous = fs::read(&checkpoint).unwrap();
    let saved: Value = serde_json::from_slice(&previous).unwrap();
    let results: Vec<_> = saved["messages"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|message| message["content"].as_array().unwrap())
        .filter(|block| block["type"] == "tool_result")
        .collect();
    assert_eq!(results.len(), 6);
    assert!(results.iter().all(|result| result["is_error"] != true));
    let search: Value = serde_json::from_str(results[0]["content"].as_str().unwrap()).unwrap();
    assert_eq!(search["stdout"], "1:amber\n");

    // Interrupt the parent while a model-requested command and descendant run.
    let script = json!([{"type":"message","role":"assistant","stop_reason":"tool_use","content":[{
        "type":"tool_use","id":"interrupt","name":"bash","input":{"command":"sleep 30 & echo $! > descendant.pid; echo $$ > running.pid; wait"}
    }]}]);
    fs::write(
        root.path().join("script.json"),
        serde_json::to_vec(&script).unwrap(),
    )
    .unwrap();
    let mut child = launch(root.path(), Some(&checkpoint));
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"Run until interrupted\n")
        .unwrap();
    let ready = root.path().join("work/running.pid");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !ready.exists() {
        assert!(
            child.try_wait().unwrap().is_none(),
            "CLI exited before command launch"
        );
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("command did not become ready");
        }
        thread::sleep(Duration::from_millis(20));
    }
    kill_process(Pid::from_raw(child.id() as i32).unwrap(), Signal::INT).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("CLI did not exit after Ctrl-C");
        }
        thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(child.wait_with_output().unwrap().status.signal(), Some(2));
    assert_eq!(fs::read(checkpoint).unwrap(), previous);
    for name in ["running.pid", "descendant.pid"] {
        let pid = Pid::from_raw(
            fs::read_to_string(root.path().join("work").join(name))
                .unwrap()
                .trim()
                .parse()
                .unwrap(),
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while test_kill_process(pid).is_ok() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        assert!(
            test_kill_process(pid).is_err(),
            "process {pid} survived Ctrl-C cleanup"
        );
    }
}
