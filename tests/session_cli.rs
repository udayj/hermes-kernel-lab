use std::{
    fs::{read, read_dir, write},
    io::Write,
    process::{Command, Stdio},
};

use tempfile::tempdir;

// Synthetic credentials: startup checks make no model requests.
#[test]
fn cli_resolves_credentials_and_checkpoint_before_reading() {
    let directory = tempdir().unwrap();
    let invoke = |args: &[&str], input: &str| {
        let mut child = Command::new(env!("CARGO_BIN_EXE_hermes-kernel-lab"))
            .current_dir(directory.path())
            .env_remove("ANTHROPIC_API_KEY")
            .env("HOME", directory.path())
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
        child.wait_with_output().unwrap()
    };
    write(
        directory.path().join(".env"),
        "ANTHROPIC_API_KEY=synthetic-key\n",
    )
    .unwrap();
    let output = invoke(&[], "/exit\n");
    assert!(output.status.success(), "{:?}", output);
    let notice = String::from_utf8(output.stderr).unwrap();
    assert_eq!(notice.matches("Session will be saved to").count(), 1);
    assert_eq!(
        read_dir(directory.path().join(".hermes-kernel-lab/sessions"))
            .unwrap()
            .count(),
        0
    );
    let path = directory.path().join("saved.json");
    let saved = br#"{"version":1,"provider":"anthropic","model":"claude-haiku-4-5-20251001",
        "system":"synthetic saved instructions","messages":[
        {"role":"user","content":[{"type":"text","text":"hello"}]},
        {"role":"assistant","content":[{"type":"text","text":"answer"}]}]}"#;
    write(&path, saved).unwrap();
    let output = invoke(&["--resume-session", "saved.json"], "");
    assert!(output.status.success());
    assert_eq!(read(&path).unwrap(), saved);
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("saved.json")
    );
    write(directory.path().join(".env"), "ANTHROPIC_API_KEY=\n").unwrap();
    let output = invoke(&[], "/exit\n");
    assert!(!output.status.success());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("ANTHROPIC_API_KEY must be nonempty")
    );
    assert_eq!(read(&path).unwrap(), saved);
}
