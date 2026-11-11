//! Safe subprocess execution with Unix process-group isolation (`process_group(0)`)
//! and `killpg` (`kill(-pgid, SIGKILL)`) cleanup on timeout or early JSON-RPC completion.

use anyhow::{anyhow, Context, Result};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::process::CommandExt;

#[derive(Debug, Clone)]
pub struct CommandSpec {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub envs: Vec<(String, String)>,
    pub stdin_data: Option<Vec<u8>>,
    pub timeout: Duration,
}

impl CommandSpec {
    pub fn new(program: impl Into<PathBuf>, timeout: Duration) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            envs: Vec::new(),
            stdin_data: None,
            timeout: timeout.max(Duration::from_millis(50)),
        }
    }

    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    pub fn env(mut self, key: impl Into<String>, val: impl Into<String>) -> Self {
        self.envs.push((key.into(), val.into()));
        self
    }

    pub fn stdin_bytes(mut self, data: impl Into<Vec<u8>>) -> Self {
        self.stdin_data = Some(data.into());
        self
    }
}

#[derive(Debug, Clone)]
pub struct CommandOutput {
    pub status: ExitStatus,
    pub stdout: String,
    pub stderr: String,
    pub duration_ms: u64,
}

impl CommandOutput {
    /// Combines non-empty `stderr` and `stdout` cleanly for CLI diagnostics.
    pub fn combined_text(&self) -> String {
        let out = self.stdout.trim();
        let err = self.stderr.trim();
        if err.is_empty() {
            self.stdout.clone()
        } else if out.is_empty() {
            self.stderr.clone()
        } else {
            format!("{err}\n{out}")
        }
    }
}

fn spawn_isolated(spec: &CommandSpec) -> Result<Child> {
    let mut cmd = Command::new(&spec.program);
    cmd.args(&spec.args);
    for (k, v) in &spec.envs {
        cmd.env(k, v);
    }

    if spec.stdin_data.is_some() {
        cmd.stdin(Stdio::piped());
    } else {
        cmd.stdin(Stdio::null());
    }
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

    #[cfg(unix)]
    {
        // Put child in its own process group (PGID == child PID) so timeouts kill
        // the entire process tree (including Node/shell grandchildren) without touching us.
        cmd.process_group(0);
    }

    #[cfg(target_os = "linux")]
    {
        let expected_ppid = unsafe { libc::getpid() };
        // Ensure child receives SIGKILL immediately from the Linux kernel if the spawning
        // `gnome-agent-status` process/thread dies abruptly (e.g. `Gio.Subprocess.force_exit()`
        // during GNOME Shell screen-lock `disable()`).
        unsafe {
            cmd.pre_exec(move || {
                let _ = libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
                if libc::getppid() != expected_ppid {
                    libc::_exit(1);
                }
                Ok(())
            });
        }
    }

    cmd.spawn()
        .with_context(|| format!("Failed to spawn '{}'", spec.program.display()))
}

/// Terminates the entire process group led by `child_pid` (and falls back to `child.kill()`).
pub fn terminate_process_group(child: &mut Child) {
    #[cfg(unix)]
    {
        let pid = child.id() as libc::pid_t;
        if pid > 0 {
            // Negative PID sends SIGKILL to every process in process group `pid`.
            unsafe {
                let _ = libc::kill(-pid, libc::SIGKILL);
            }
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Executes `spec` to completion within `spec.timeout`, killing the entire process group on timeout.
pub fn run_command(spec: &CommandSpec) -> Result<CommandOutput> {
    let start = Instant::now();
    let mut child = spawn_isolated(spec)?;

    if let Some(ref input) = spec.stdin_data {
        if let Some(mut stdin_pipe) = child.stdin.take() {
            let payload = input.clone();
            thread::spawn(move || {
                let _ = stdin_pipe.write_all(&payload);
                let _ = stdin_pipe.flush();
                // Dropping `stdin_pipe` sends EOF to child.
            });
        }
    }

    let mut stdout_pipe = child.stdout.take().context("Failed to capture stdout")?;
    let mut stderr_pipe = child.stderr.take().context("Failed to capture stderr")?;

    let stdout_handle = thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout_pipe.read_to_end(&mut buf);
        String::from_utf8_lossy(&buf).into_owned()
    });

    let stderr_handle = thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr_pipe.read_to_end(&mut buf);
        String::from_utf8_lossy(&buf).into_owned()
    });

    let poll_interval = Duration::from_millis(20);
    let status = loop {
        if let Some(exit_status) = child.try_wait()? {
            break exit_status;
        }
        if start.elapsed() >= spec.timeout {
            terminate_process_group(&mut child);
            let _ = stdout_handle.join();
            let _ = stderr_handle.join();
            return Err(anyhow!(
                "Command '{}' timed out after {:.1}s",
                spec.program.display(),
                spec.timeout.as_secs_f64()
            ));
        }
        thread::sleep(poll_interval);
    };

    let stdout = stdout_handle.join().unwrap_or_default();
    let stderr = stderr_handle.join().unwrap_or_default();
    let duration_ms = start.elapsed().as_millis() as u64;

    Ok(CommandOutput {
        status,
        stdout,
        stderr,
        duration_ms,
    })
}

/// Sends newline-delimited JSON-RPC messages via `spec.stdin_data` and returns as soon as a
/// JSON line with `"id": target_id` is emitted on `stdout`, immediately terminating the server
/// process group so persistent stdio servers (`codex app-server`, `grok agent stdio`) never stall.
pub fn run_jsonrpc_line_query(spec: &CommandSpec, target_id: u64) -> Result<String> {
    let start = Instant::now();
    let mut child = spawn_isolated(spec)?;

    let stdin_pipe = child.stdin.take();
    let payload = spec.stdin_data.clone().unwrap_or_default();
    // Keep stdin open briefly until server responds or we close it after writing:
    // Note: some stdio servers exit immediately if stdin hits EOF before processing async RPCs,
    // so we hold `stdin_pipe` after flushing until response arrives or timeout!
    let (close_stdin_tx, close_stdin_rx) = mpsc::channel::<()>();
    if let Some(mut pipe) = stdin_pipe {
        thread::spawn(move || {
            let _ = pipe.write_all(&payload);
            let _ = pipe.flush();
            let _ = close_stdin_rx.recv();
        });
    }

    let stdout_pipe = child.stdout.take().context("Failed to capture stdout")?;
    let mut stderr_pipe = child.stderr.take().context("Failed to capture stderr")?;

    let (line_tx, line_rx) = mpsc::channel::<String>();
    let stdout_handle = thread::spawn(move || {
        let reader = BufReader::new(stdout_pipe);
        for line in reader.lines().map_while(Result::ok) {
            if line_tx.send(line).is_err() {
                break;
            }
        }
    });

    let stderr_handle = thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr_pipe.read_to_end(&mut buf);
        String::from_utf8_lossy(&buf).into_owned()
    });

    let mut matched_line: Option<String> = None;
    while start.elapsed() < spec.timeout {
        let remaining = spec
            .timeout
            .checked_sub(start.elapsed())
            .unwrap_or(Duration::from_millis(1))
            .min(Duration::from_millis(40));

        match line_rx.recv_timeout(remaining) {
            Ok(line) => {
                let trimmed = line.trim();
                if trimmed.starts_with('{') {
                    if let Ok(val) = serde_json::from_str::<serde_json::Value>(trimmed) {
                        if val.get("id").and_then(|v| v.as_u64()) == Some(target_id) {
                            matched_line = Some(trimmed.to_string());
                            break;
                        }
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if child.try_wait()?.is_some() {
                    // Drain any buffered lines
                    while let Ok(line) = line_rx.try_recv() {
                        let trimmed = line.trim();
                        if trimmed.starts_with('{') {
                            if let Ok(val) = serde_json::from_str::<serde_json::Value>(trimmed) {
                                if val.get("id").and_then(|v| v.as_u64()) == Some(target_id) {
                                    matched_line = Some(trimmed.to_string());
                                    break;
                                }
                            }
                        }
                    }
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    let _ = close_stdin_tx.send(());
    terminate_process_group(&mut child);
    drop(line_rx);
    let _ = stdout_handle.join();
    let stderr_str = stderr_handle.join().unwrap_or_default();

    if let Some(json_line) = matched_line {
        return Ok(json_line);
    }

    if start.elapsed() >= spec.timeout {
        Err(anyhow!(
            "JSON-RPC command '{}' timed out after {:.1}s",
            spec.program.display(),
            spec.timeout.as_secs_f64()
        ))
    } else {
        let first_err = stderr_str.lines().next().unwrap_or("no response for target id");
        Err(anyhow!(
            "JSON-RPC command '{}' exited without response id {}: {}",
            spec.program.display(),
            target_id,
            first_err
        ))
    }
}

/// Runs a shell command (`sh -c <cmd>`) safely within a process group and timeout.
pub fn run_shell_command(shell_cmd: &str, timeout: Duration) -> Result<CommandOutput> {
    let spec = CommandSpec::new(Path::new("/bin/sh"), timeout).args(["-c", shell_cmd]);
    run_command(&spec)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn test_killpg_reaps_grandchild_on_timeout() {
        let tmp_dir = std::env::temp_dir().join(format!(
            "gas-proc-exec-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&tmp_dir).expect("create temp dir");
        let pid_file = tmp_dir.join("grandchild.pid");
        let script = tmp_dir.join("spawn_grandchild.sh");

        fs::write(
            &script,
            format!(
                "#!/bin/sh\nsleep 60 &\necho $! > \"{}\"\nwait\n",
                pid_file.display()
            ),
        )
        .expect("write script");
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("chmod");

        let spec = CommandSpec::new(&script, Duration::from_millis(200));
        let res = run_command(&spec);
        assert!(res.is_err(), "expected timeout error");

        // Verify grandchild PID was recorded and is now dead (ESRCH).
        let raw_pid = fs::read_to_string(&pid_file).expect("read grandchild pid");
        let grandchild_pid: libc::pid_t = raw_pid.trim().parse().expect("parse pid");

        // Give kernel a brief moment to finalize process table cleanup after SIGKILL.
        let mut dead = false;
        for _ in 0..20 {
            let rc = unsafe { libc::kill(grandchild_pid, 0) };
            if rc == -1 {
                dead = true;
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        let _ = fs::remove_dir_all(&tmp_dir);
        assert!(dead, "grandchild process {grandchild_pid} must be terminated by killpg");
    }

    #[test]
    fn test_jsonrpc_line_query_returns_immediately_without_waiting_for_sleep() {
        let tmp_dir = std::env::temp_dir().join(format!(
            "gas-jsonrpc-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&tmp_dir).expect("create temp dir");
        let script = tmp_dir.join("mock_rpc_server.sh");

        fs::write(
            &script,
            r#"#!/bin/sh
read _line1
echo '{"jsonrpc":"2.0","id":1,"result":{"serverInfo":{"name":"mock"}}}'
read _line2
read _line3
echo '{"jsonrpc":"2.0","id":2,"result":{"rateLimits":{"primary":{"usedPercent":14}}}}'
sleep 30
"#,
        )
        .expect("write mock rpc script");
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("chmod");

        let reqs = concat!(
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{}}\n",
            "{\"jsonrpc\":\"2.0\",\"method\":\"initialized\",\"params\":{}}\n",
            "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"account/rateLimits/read\",\"params\":{}}\n"
        );
        let spec = CommandSpec::new(&script, Duration::from_secs(5)).stdin_bytes(reqs.as_bytes());
        let start = Instant::now();
        let line = run_jsonrpc_line_query(&spec, 2).expect("rpc line 2");
        let elapsed = start.elapsed();
        let _ = fs::remove_dir_all(&tmp_dir);

        assert!(elapsed < Duration::from_secs(2), "returned quickly in {:?}", elapsed);
        let parsed: serde_json::Value = serde_json::from_str(&line).expect("valid json");
        assert_eq!(parsed["result"]["rateLimits"]["primary"]["usedPercent"], 14);
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore]
    fn pdeathsig_helper_entrypoint() {
        let script_path = std::env::var("GAS_PDEATHSIG_SCRIPT").expect("GAS_PDEATHSIG_SCRIPT");
        let spec = CommandSpec::new(PathBuf::from(script_path), Duration::from_secs(60));
        let _ = run_command(&spec);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_pdeathsig_reaps_child_when_parent_sigkilled() {
        let tmp_dir = std::env::temp_dir().join(format!(
            "gas-pdeathsig-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&tmp_dir).expect("create temp dir");
        let pid_file = tmp_dir.join("child.pid");
        let script = tmp_dir.join("child_sleep.sh");

        fs::write(
            &script,
            format!("#!/bin/sh\necho $$ > \"{}\"\nexec sleep 60\n", pid_file.display()),
        )
        .expect("write script");
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("chmod");

        let current_exe = std::env::current_exe().expect("current_exe");
        let mut middle = Command::new(current_exe)
            .args([
                "--exact",
                "proc_exec::tests::pdeathsig_helper_entrypoint",
                "--ignored",
                "--nocapture",
            ])
            .env("GAS_PDEATHSIG_SCRIPT", &script)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn middle helper");

        let mut child_pid: Option<libc::pid_t> = None;
        for _ in 0..100 {
            if let Ok(raw) = fs::read_to_string(&pid_file) {
                if let Ok(parsed) = raw.trim().parse::<libc::pid_t>() {
                    if parsed > 0 {
                        child_pid = Some(parsed);
                        break;
                    }
                }
            }
            thread::sleep(Duration::from_millis(15));
        }

        let target_pid = child_pid.expect("child should have written its PID");
        assert_eq!(unsafe { libc::kill(target_pid, 0) }, 0, "child alive before parent SIGKILL");

        // Simulate Gio.Subprocess.force_exit() sending SIGKILL to `gnome-agent-status`.
        let _ = middle.kill();
        let _ = middle.wait();

        let mut reaped = false;
        for _ in 0..40 {
            if unsafe { libc::kill(target_pid, 0) } == -1 {
                reaped = true;
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        let _ = fs::remove_dir_all(&tmp_dir);
        assert!(reaped, "child PID {target_pid} must die via PR_SET_PDEATHSIG when parent is SIGKILLed");
    }
}
