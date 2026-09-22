use std::{
    fs,
    io::Write,
    process::{Command, Stdio},
};

// Synthetic checkpoints and no credentials: startup and no-turn exits must stay offline.
#[test]
fn cli_validates_before_credentials_and_no_turn_exits_never_write() {
    let directory = tempfile::tempdir().unwrap();
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
        let output = invoke(&["--save-session", "new.json"], input);
        assert!(output.status.success(), "{:?}", output);
        assert!(!directory.path().join("new.json").exists());
    }
    let path = directory.path().join("saved.json");
    let saved = br#"{"version":1,"provider":"anthropic","model":"claude-haiku-4-5-20251001",
        "system":"synthetic saved instructions","messages":[
        {"role":"user","content":[{"type":"text","text":"hello"}]},
        {"role":"assistant","content":[{"type":"text","text":"answer"}]}]}"#;
    fs::write(&path, saved).unwrap();
    // Resume must not attempt to load even an invalid root instruction file.
    fs::write(directory.path().join("AGENTS.md"), [0xff]).unwrap();
    for input in ["", "\n", "/exit\n"] {
        for args in [
            vec!["--resume-session", "saved.json"],
            vec!["--resume-session", "saved.json", "--workspace", "."],
        ] {
            assert!(invoke(&args, input).status.success());
            assert_eq!(fs::read(&path).unwrap(), saved);
        }
    }
    for args in [
        vec![
            "--save-session",
            "new.json",
            "--resume-session",
            "saved.json",
        ],
        vec![
            "--resume-session",
            "saved.json",
            "--no-project-instructions",
        ],
        vec![
            "--resume-session",
            "saved.json",
            "--workspace",
            ".",
            "--instructions",
            "AGENTS.md",
        ],
        vec![
            "--workspace",
            ".",
            "--instructions",
            "AGENTS.md",
            "--no-project-instructions",
        ],
    ] {
        assert_eq!(invoke(&args, "").status.code(), Some(2));
    }
    for text in [
        "not json",
        "{}",
        r#"{"version":2,"provider":"other","model":"other","system":"s","messages":[]}"#,
    ] {
        fs::write(&path, text).unwrap();
        for args in [
            vec!["--resume-session", "saved.json"],
            vec!["--resume-session", "saved.json", "Hello"],
        ] {
            let output = invoke(&args, "");
            assert!(!output.status.success());
            let error = String::from_utf8(output.stderr).unwrap();
            assert!(error.contains("checkpoint"), "{error}");
            assert!(!error.contains("ANTHROPIC_API_KEY"), "{error}");
        }
    }
}
