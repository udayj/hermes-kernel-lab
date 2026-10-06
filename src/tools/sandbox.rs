//! macOS command policy. The parent runtime never enters this sandbox.
use std::path::{Path, PathBuf};
use subprocess::Exec;

pub(super) struct Sandbox {
    root: PathBuf,
    scratch: PathBuf,
    profile: String,
    parameters: Vec<(String, PathBuf)>,
}

impl Sandbox {
    pub fn new(root: &Path, writable: bool) -> Result<Self, String> {
        let canonical = root
            .canonicalize()
            .map_err(|_| "sandbox workspace must exist")?;
        let root = canonical.as_path();
        if root == Path::new("/")
            || [
                "/System",
                "/Library",
                "/usr",
                "/bin",
                "/sbin",
                "/opt",
                "/Applications",
            ]
            .iter()
            .any(|p| Path::new(p).starts_with(root) || root.starts_with(p))
        {
            return Err(
                "workspace must be a narrow project directory, outside system/tool roots".into(),
            );
        }
        if std::env::var_os("HOME")
            .is_some_and(|home| Path::new(&home).canonicalize().ok().as_deref() == Some(root))
        {
            return Err("real HOME cannot be the workspace".into());
        }
        if !Path::new("/usr/bin/sandbox-exec").is_file() {
            return Err("Seatbelt is unavailable; no commands will run".into());
        }
        let scratch = tempfile::Builder::new()
            .prefix(".hermes-scratch-")
            .tempdir_in(root)
            .map_err(|_| "could not create private workspace scratch")?
            .keep();
        for name in ["home", "tmp", "cache"] {
            std::fs::create_dir(scratch.join(name)).map_err(|_| "could not initialize scratch")?;
        }
        // Paths are passed as -D parameters, never interpolated into policy source.
        let mut parameters = vec![
            ("WORKSPACE".into(), root.to_owned()),
            ("SCRATCH".into(), scratch.clone()),
        ];
        let mut escaped = String::new();
        for c in root
            .to_str()
            .ok_or("workspace path must be Unicode")?
            .chars()
        {
            if ".+*?()[]{}^$|\\".contains(c) {
                escaped.push('\\');
            }
            escaped.push(c);
        }
        parameters.push((
            "HIDDEN".into(),
            PathBuf::from(format!("^{escaped}/(.*/)?\\.[^/]+(/|$)")),
        ));
        let mut profile = r##"(version 1)
(deny default)
(allow process-fork process-exec)
(allow signal (target same-sandbox))
(allow process-info* (target same-sandbox))
(allow sysctl-read
  (sysctl-name "hw.ncpu") (sysctl-name "hw.activecpu") (sysctl-name "hw.memsize")
  (sysctl-name "hw.pagesize") (sysctl-name "hw.pagesize_compat") (sysctl-name "hw.logicalcpu")
  (sysctl-name "kern.ostype") (sysctl-name "kern.osrelease") (sysctl-name "kern.osversion")
  (sysctl-name "kern.osproductversion") (sysctl-name "kern.osvariant_status")
  (sysctl-name "kern.argmax") (sysctl-name "kern.secure_kernel")
  (sysctl-name "sysctl.proc_cputype"))
(allow file-read* file-test-existence file-map-executable
  (subpath "/bin") (subpath "/usr/bin") (subpath "/usr/lib") (subpath "/usr/share")
  (subpath "/System/Library") (literal "/dev/null") (literal "/dev/urandom")
  (subpath (param "WORKSPACE")))
(allow file-read-metadata (literal "/") (literal "/private") (literal "/private/tmp") (literal "/Users"))
; dyld ignition opens the root directory. This grants no descendant contents.
(allow file-read* file-test-existence (literal "/"))
(allow file-read-metadata file-test-existence (path-ancestors (param "WORKSPACE")))
(allow file-write-data (literal "/dev/null"))
"##.to_owned();
        if writable {
            profile.push_str("(allow file-write* (subpath (param \"WORKSPACE\")))\n");
        }
        profile.push_str(
            r##"
(allow file-write* (subpath (param "SCRATCH")))
; Protection follows allowances, including the shell scratch exception.
(deny file-read-data file-map-executable file-write*
  (require-all (regex (param "HIDDEN")) (require-not (subpath (param "SCRATCH")))))
(deny file-write* (regex #"(^|/)AGENTS\.md(/|$)"))
"##,
        );
        Ok(Self {
            root: root.to_owned(),
            scratch,
            profile,
            parameters,
        })
    }

    pub fn command(&self, script: &str) -> Exec {
        let mut command = Exec::cmd("/usr/bin/sandbox-exec")
            .arg("-p")
            .arg(&self.profile);
        for (key, value) in &self.parameters {
            let mut argument = std::ffi::OsString::from(format!("{key}="));
            argument.push(value);
            command = command.arg("-D").arg(argument);
        }
        command
            .arg("/bin/bash")
            .args(["--noprofile", "--norc", "-c", script])
            .cwd(&self.root)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("PWD", &self.root)
            .env("HOME", self.scratch.join("home"))
            .env("TMPDIR", self.scratch.join("tmp"))
            .env("XDG_CACHE_HOME", self.scratch.join("cache"))
            .env("LC_ALL", "C")
    }
}
