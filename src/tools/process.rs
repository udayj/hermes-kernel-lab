//! One foreground command owns its output, deadline, and process-group cleanup.
use super::sandbox::Sandbox;
use rustix::{
    io::Errno,
    process::{Pid, WaitId, WaitIdOptions, waitid},
};
use serde::Serialize;
use signal_hook::{
    consts::signal::{SIGHUP, SIGINT, SIGKILL, SIGTERM},
    flag,
};
use std::{
    io::{self, ErrorKind, Write},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
use subprocess::{Communicator, ExecExt, JobExt, Redirection};

const MAX_OUTPUT: usize = 32 * 1024;
const READ_SLICE: usize = 64 * 1024;
const TICK: Duration = Duration::from_millis(20);
const DEADLINE: Duration = Duration::from_secs(30);
const DRAIN_DEADLINE: Duration = Duration::from_secs(1);

#[derive(Serialize)]
pub(super) struct CommandResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<u32>,
    pub signal: Option<i32>,
    pub timed_out: bool,
    pub truncated: bool,
}

#[derive(Default)]
struct Output {
    bytes: Vec<u8>,
    truncated: bool,
    read: usize,
}
impl Write for Output {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let keep = bytes.len().min(MAX_OUTPUT - self.bytes.len());
        self.bytes.extend_from_slice(&bytes[..keep]);
        self.truncated |= keep < bytes.len();
        self.read += bytes.len();
        // Acknowledge discarded bytes so a full buffer cannot block the child.
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl Output {
    fn text(self) -> (String, bool) {
        let mut text = String::from_utf8_lossy(&self.bytes).into_owned();
        let truncated = self.truncated || text.len() > MAX_OUTPUT;
        if text.len() > MAX_OUTPUT {
            let mut end = MAX_OUTPUT;
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            text.truncate(end);
        }
        (text, truncated)
    }
}

pub(super) struct Runner {
    sandbox: Sandbox,
    idle: Arc<AtomicBool>,
    signals: Vec<(i32, Arc<AtomicBool>)>,
}
impl Runner {
    pub fn open(sandbox: Sandbox) -> Result<Self, String> {
        let idle = Arc::new(AtomicBool::new(true));
        let mut signals = Vec::new();
        // Registrations last for the CLI's lifetime. While idle, use the default
        // action; during execution, flags request cleanup before that action.
        for signal in [SIGINT, SIGTERM, SIGHUP] {
            let received = Arc::new(AtomicBool::new(false));
            flag::register_conditional_default(signal, idle.clone())
                .and_then(|_| flag::register(signal, received.clone()))
                .map_err(|_| "could not install cleanup signal handlers")?;
            signals.push((signal, received));
        }
        let runner = Self {
            sandbox,
            idle,
            signals,
        };
        let probe = runner.bash("printf seatbelt-ready".into())?;
        if probe.exit_code != Some(0) || probe.stdout != "seatbelt-ready" {
            return Err("Seatbelt initialization failed; no commands will run".into());
        }
        Ok(runner)
    }

    pub fn bash(&self, command: String) -> Result<CommandResult, String> {
        if command.trim().is_empty() || command.len() > 16 * 1024 || command.contains('\0') {
            return Err("command must be nonblank, NUL-free, and at most 16 KiB".into());
        }
        self.run(&command, DEADLINE)
    }

    fn run(&self, command: &str, lifetime: Duration) -> Result<CommandResult, String> {
        for (_, flag) in &self.signals {
            flag.store(false, Ordering::SeqCst);
        }
        self.idle.store(false, Ordering::SeqCst);
        let result = self.execute(command, lifetime);
        self.idle.store(true, Ordering::SeqCst);
        if let Some((signal, _)) = self
            .signals
            .iter()
            .find(|(_, flag)| flag.load(Ordering::SeqCst))
        {
            signal_hook::low_level::emulate_default_handler(*signal)
                .map_err(|_| "could not restore the default signal action")?;
        }
        result.map_err(|e| format!("could not execute sandboxed command: {e}"))
    }

    fn execute(&self, command: &str, lifetime: Duration) -> io::Result<CommandResult> {
        let mut job = self
            .sandbox
            .command(command)
            .stdin(Redirection::Null)
            .stdout(Redirection::Pipe)
            .stderr(Redirection::Pipe)
            .setpgid()
            .start()?;
        let mut stdout = Output::default();
        let mut stderr = Output::default();
        let capture = (|| {
            let mut streams = job.communicate()?.limit_time(TICK).limit_size(READ_SLICE);
            let deadline = Instant::now() + lifetime;
            let mut eof = false;
            let timed_out = loop {
                if !eof {
                    eof = read_output(&mut streams, &mut stdout, &mut stderr)?;
                } else {
                    thread::sleep(TICK);
                }
                let timed_out = Instant::now() >= deadline;
                if exited(job.pid())?
                    || timed_out
                    || self
                        .signals
                        .iter()
                        .any(|(_, flag)| flag.load(Ordering::SeqCst))
                {
                    break timed_out;
                }
            };
            // Keep the leader unreaped until group signalling is finished.
            kill_group(&job)?;
            let drain_deadline = Instant::now() + DRAIN_DEADLINE;
            while !eof {
                eof = read_output(&mut streams, &mut stdout, &mut stderr)?;
                if !eof && Instant::now() >= drain_deadline {
                    return Err(io::Error::new(
                        ErrorKind::TimedOut,
                        "command pipes did not close after cleanup",
                    ));
                }
            }
            Ok(timed_out)
        })();
        if capture.is_err() {
            let _ = kill_group(&job);
        }
        // Reap on errors too; no tool result is returned before owned-child cleanup.
        let status = job.wait();
        let timed_out = capture?;
        let status = status?;
        let (stdout, out_truncated) = stdout.text();
        let (stderr, err_truncated) = stderr.text();
        Ok(CommandResult {
            stdout,
            stderr,
            exit_code: status.code(),
            signal: status.signal(),
            timed_out,
            truncated: out_truncated || err_truncated,
        })
    }
}

// One bounded read; false also covers a time slice ending before EOF.
fn read_output(
    streams: &mut Communicator,
    stdout: &mut Output,
    stderr: &mut Output,
) -> io::Result<bool> {
    stdout.read = 0;
    stderr.read = 0;
    match streams.read_to(&mut *stdout, &mut *stderr) {
        Ok(()) => Ok(stdout.read + stderr.read < READ_SLICE),
        Err(e) if matches!(e.kind(), ErrorKind::TimedOut | ErrorKind::Interrupted) => Ok(false),
        Err(e) => Err(e),
    }
}

fn kill_group(job: &subprocess::Job) -> io::Result<()> {
    match job.send_signal_group(SIGKILL) {
        Err(e) if e.raw_os_error() == Some(Errno::SRCH.raw_os_error()) => Ok(()),
        // Darwin can return EPERM for a group containing only zombies.
        Err(e) if e.raw_os_error() == Some(Errno::PERM.raw_os_error()) && exited(job.pid())? => {
            Ok(())
        }
        result => result,
    }
}

fn exited(pid: u32) -> io::Result<bool> {
    let pid = Pid::from_raw(pid as i32).expect("owned child has a positive PID");
    match waitid(
        WaitId::Pid(pid),
        WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
    ) {
        Ok(status) => Ok(status.is_some()),
        Err(Errno::INTR) => Ok(false),
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, net::TcpListener};
    use tempfile::tempdir;

    fn runner(root: &std::path::Path) -> Runner {
        Runner::open(Sandbox::new(root, true).unwrap()).unwrap()
    }

    #[test]
    fn bash_returns_output_and_child_signals() {
        let root = tempdir().unwrap();
        let runner = runner(root.path());
        let result = runner.bash("printf out; printf err >&2".into()).unwrap();
        assert_eq!(
            (
                result.stdout.as_str(),
                result.stderr.as_str(),
                result.exit_code
            ),
            ("out", "err", Some(0))
        );
        // exec must reset the parent's caught SIGINT disposition in this child.
        let result = runner.bash("kill -INT $$".into()).unwrap();
        assert_eq!(result.signal, Some(SIGINT));
    }

    #[test]
    fn sandbox_denies_outside_writes_and_network() {
        let root = tempdir().unwrap();
        let workspace = root.path().join("work");
        fs::create_dir(&workspace).unwrap();
        let outside = root.path().join("outside");
        fs::write(&outside, "original").unwrap();
        let runner = runner(&workspace);
        assert_ne!(
            runner
                .bash("printf bad > ../outside".into())
                .unwrap()
                .exit_code,
            Some(0)
        );
        assert_eq!(fs::read_to_string(outside).unwrap(), "original");
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let result = runner
            .bash(format!(
                "/usr/bin/nc -z -w 1 127.0.0.1 {}",
                listener.local_addr().unwrap().port()
            ))
            .unwrap();
        assert_ne!(result.exit_code, Some(0));
        assert_eq!(listener.accept().unwrap_err().kind(), ErrorKind::WouldBlock);
    }

    #[test]
    fn timeout_kills_descendants() {
        let root = tempdir().unwrap();
        let runner = runner(root.path());
        let result = runner
            .run(
                "sleep 30 & echo $! > child.pid; wait",
                Duration::from_millis(200),
            )
            .unwrap();
        assert!(result.timed_out);
        assert_eq!(result.signal, Some(SIGKILL));
        let pid = Pid::from_raw(
            fs::read_to_string(root.path().join("child.pid"))
                .unwrap()
                .trim()
                .parse()
                .unwrap(),
        )
        .unwrap();
        for _ in 0..100 {
            if rustix::process::test_kill_process(pid).is_err() {
                return;
            }
            thread::sleep(TICK);
        }
        panic!("descendant survived timeout cleanup");
    }
}
