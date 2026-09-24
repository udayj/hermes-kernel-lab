use std::{
    fs::{create_dir_all, read, write},
    io::Write,
    process::{Command, Stdio},
};

use tempfile::tempdir;

// Synthetic checkpoints and no credentials: startup and no-turn exits must stay offline.
#[test]
fn cli_validates_before_credentials_and_no_turn_exits_never_write() {
    let directory = tempdir().unwrap();
    let invoke = |args: &[&str], input: &str| {
        let mut child = Command::new(env!("CARGO_BIN_EXE_hermes-kernel-lab"))
            .current_dir(directory.path())
            .env_remove("ANTHROPIC_API_KEY")
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
    for input in ["", "\n", "/exit\n"] {
        let output = invoke(&[], input);
        assert!(output.status.success(), "{:?}", output);
        assert!(output.stderr.is_empty());
    }
    let path = directory.path().join("saved.json");
    let saved = br#"{"version":1,"provider":"anthropic","model":"claude-haiku-4-5-20251001",
        "system":"synthetic saved instructions","messages":[
        {"role":"user","content":[{"type":"text","text":"hello"}]},
        {"role":"assistant","content":[{"type":"text","text":"answer"}]}]}"#;
    write(&path, saved).unwrap();
    // Resume must not attempt to load even an invalid root instruction file.
    write(directory.path().join("AGENTS.md"), [0xff]).unwrap();
    create_dir_all(directory.path().join("skills/sample")).unwrap();
    write(
        directory.path().join("skills/sample/SKILL.md"),
        "---\nname: sample\ndescription: Synthetic procedure\n---\nRead a file.\n",
    )
    .unwrap();
    for (args, input) in [
        (vec!["--resume-session", "saved.json"], ""),
        (
            vec!["--resume-session", "saved.json", "--skills-dir", "skills"],
            "",
        ),
        (
            vec!["--resume-session", "saved.json", "--workspace", "."],
            "\n/exit\n",
        ),
    ] {
        assert!(invoke(&args, input).status.success());
        assert_eq!(read(&path).unwrap(), saved);
    }
    write(directory.path().join("skills/sample/SKILL.md"), "invalid").unwrap();
    for args in [
        vec!["--skills-dir", "skills"],
        vec!["--resume-session", "saved.json", "--skills-dir", "skills"],
    ] {
        let output = invoke(&args, "Hello\n");
        assert!(!output.status.success());
        let error = String::from_utf8(output.stderr).unwrap();
        assert!(error.contains("skills catalog"), "{error}");
        assert!(!error.contains("ANTHROPIC_API_KEY"), "{error}");
        assert_eq!(read(&path).unwrap(), saved);
    }
    // An unselected invalid catalog is never loaded on resume.
    assert!(
        invoke(&["--resume-session", "saved.json"], "")
            .status
            .success()
    );
    write(&path, "not json").unwrap();
    let output = invoke(&["--resume-session", "saved.json"], "Hello\n");
    assert!(!output.status.success());
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(error.contains("checkpoint"), "{error}");
    assert!(!error.contains("ANTHROPIC_API_KEY"), "{error}");
}
