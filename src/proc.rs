//! Bounded subprocess execution.
//!
//! Every external process this CLI runs (`git`, `jj`, `$EDITOR`, the OS keyring
//! helpers, `gh`) goes through here. That matters on a 24/7 server: a helper
//! that tries to read from a closed stdin, or a keyring daemon that accepts a
//! connection and then never answers, must not be able to hang an agent's
//! command forever. Each call therefore has a deadline, and stdin is `null`
//! unless the caller explicitly supplies input.
//!
//! Exit-status, stdout and stderr are captured; nothing is inherited from the
//! parent terminal, so a command can never block on a TTY prompt.

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Default deadline for short-lived helpers (git, jj, keyring).
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Longest we wait for an editor, which is interactive by design.
pub const EDITOR_TIMEOUT: Duration = Duration::from_secs(3600);

#[derive(Debug)]
pub struct ProcOutput {
    pub success: bool,
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl ProcOutput {
    pub fn stdout_string(&self) -> String {
        String::from_utf8_lossy(&self.stdout).to_string()
    }

    pub fn stdout_trimmed(&self) -> String {
        self.stdout_string().trim().to_string()
    }

    pub fn stderr_string(&self) -> String {
        String::from_utf8_lossy(&self.stderr).to_string()
    }
}

/// Options for [`run`].
#[derive(Debug, Clone, Default)]
pub struct RunOptions {
    /// Data written to the child's stdin. `None` means stdin is closed.
    pub stdin: Option<Vec<u8>>,
    /// Extra environment variables.
    pub env: Vec<(String, String)>,
    /// Working directory.
    pub cwd: Option<std::path::PathBuf>,
}

/// Run a program with a deadline, capturing output.
///
/// Returns `Ok(None)` when the deadline expires (the child is killed) or when
/// the program cannot be spawned at all — callers treat both as "this helper is
/// unavailable", which is never fatal.
pub fn run(
    program: &str,
    args: &[&str],
    options: &RunOptions,
    timeout: Duration,
) -> Option<ProcOutput> {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(if options.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in &options.env {
        command.env(key, value);
    }
    if let Some(cwd) = &options.cwd {
        command.current_dir(cwd);
    }

    let mut child = command.spawn().ok()?;

    if let Some(input) = &options.stdin {
        if let Some(mut stdin) = child.stdin.take() {
            // A helper that exits without reading must not raise SIGPIPE on us.
            let _ = stdin.write_all(input);
            drop(stdin);
        }
    }

    // Drain the pipes on separate threads so a chatty child cannot deadlock
    // against a full OS pipe buffer while we wait for it to exit.
    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();
    let stdout_reader = std::thread::spawn(move || read_all(stdout_pipe));
    let stderr_reader = std::thread::spawn(move || read_all(stderr_pipe));

    let deadline = Instant::now() + timeout;
    let mut status = None;
    loop {
        match child.try_wait() {
            Ok(Some(exit_status)) => {
                status = Some(exit_status);
                break;
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(_) => break,
        }
    }

    let stdout = stdout_reader.join().unwrap_or_default();
    let stderr = stderr_reader.join().unwrap_or_default();

    let status = status?;
    Some(ProcOutput {
        success: status.success(),
        code: status.code(),
        stdout,
        stderr,
    })
}

/// Run a program that must own the terminal, applying the same deadline as
/// [`run`].
///
/// Used for an editor or a pager: both write directly to the terminal, so
/// capturing their output (as [`run`] does) would break them. `stdin_input`
/// is piped to the child when supplied (a pager reads the document this way);
/// otherwise stdin is inherited so an editor can read keystrokes.
///
/// Returns `Some(success)` on exit, or `None` when the program cannot be
/// spawned or the deadline expires (the child is killed) — the same "helper
/// unavailable" contract as [`run`].
pub fn run_inherit(
    program: &str,
    args: &[&str],
    stdin_input: Option<&[u8]>,
    timeout: Duration,
) -> Option<bool> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(if stdin_input.is_some() {
            Stdio::piped()
        } else {
            Stdio::inherit()
        })
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .ok()?;

    if let Some(input) = stdin_input {
        if let Some(mut stdin) = child.stdin.take() {
            // A pager that exits without reading must not raise SIGPIPE on us.
            let _ = stdin.write_all(input);
            drop(stdin);
        }
    }

    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status.success()),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(_) => return None,
        }
    }
}

fn read_all(pipe: Option<impl Read>) -> Vec<u8> {
    let Some(mut pipe) = pipe else {
        return Vec::new();
    };
    let mut buffer = Vec::new();
    let _ = pipe.read_to_end(&mut buffer);
    buffer
}

/// Run a helper and return its trimmed stdout when it exits successfully.
pub fn stdout_of(program: &str, args: &[&str]) -> Option<String> {
    let output = run(program, args, &RunOptions::default(), DEFAULT_TIMEOUT)?;
    output.success.then(|| output.stdout_trimmed())
}

/// Whether `program` exists on `PATH` (or as an explicit path).
pub fn exists(program: &str) -> bool {
    if program.contains('/') {
        return std::path::Path::new(program).is_file();
    }
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|dir| dir.join(program).is_file()))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captures_stdout_and_status() {
        let output = run("echo", &["hello"], &RunOptions::default(), DEFAULT_TIMEOUT).unwrap();
        assert!(output.success);
        assert_eq!(output.stdout_trimmed(), "hello");
    }

    #[test]
    fn missing_program_is_none() {
        assert!(run(
            "definitely-not-a-real-program-xyz",
            &[],
            &RunOptions::default(),
            DEFAULT_TIMEOUT
        )
        .is_none());
    }

    #[test]
    fn timeout_kills_the_child() {
        let output = run(
            "sleep",
            &["30"],
            &RunOptions::default(),
            Duration::from_millis(120),
        );
        assert!(output.is_none(), "sleep should have been killed");
    }

    #[test]
    fn stdin_is_closed_by_default() {
        // `cat` with no stdin input sees EOF immediately instead of blocking.
        let output = run("cat", &[], &RunOptions::default(), DEFAULT_TIMEOUT).unwrap();
        assert!(output.success);
        assert!(output.stdout.is_empty());
    }

    #[test]
    fn writes_stdin_when_provided() {
        let options = RunOptions {
            stdin: Some(b"piped value".to_vec()),
            ..Default::default()
        };
        let output = run("cat", &[], &options, DEFAULT_TIMEOUT).unwrap();
        assert_eq!(output.stdout_trimmed(), "piped value");
    }
}
