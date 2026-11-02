//! Startup/stop launcher behaviours. See `docs/RUNTIME.md`.
//!
//! Killing the port holder is inherently platform-specific and cannot be
//! expressed portably without a dependency, so it shells out to WMI-equivalent
//! tools: `netstat`/`taskkill` on Windows and `lsof`/`ss`/`kill` elsewhere. If
//! neither tool is present the kill is skipped with a warning rather than
//! failing the start — the bind error that follows is a clearer diagnostic than
//! a missing `ps`.

use std::process::{Command, Stdio};
use std::time::Duration;

/// How many times a crashed server is restarted before giving up.
pub const RESTART_ATTEMPTS: u32 = 2;
/// Delay before each restart.
pub const RESTART_DELAY: Duration = Duration::from_secs(2);

/// Kill whatever is listening on `port`. Returns whether anything was killed.
pub fn kill_port_holder(port: u16) -> bool {
    let Some(pids) = pids_listening_on(port) else {
        tracing::warn!("could not enumerate listeners on port {port}; skipping the kill");
        return false;
    };
    let mut killed = false;
    for pid in pids {
        if pid == std::process::id() {
            continue;
        }
        if kill_pid(pid) {
            killed = true;
            tracing::info!("killed pid {pid} holding port {port}");
        }
    }
    killed
}

#[cfg(windows)]
fn pids_listening_on(port: u16) -> Option<Vec<u32>> {
    let out = Command::new("netstat")
        .args(["-ano", "-p", "TCP"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(pids_from_netstat(
        &String::from_utf8_lossy(&out.stdout),
        port,
    ))
}

/// The pids of the processes LISTENING on `port`, from `netstat -ano -p TCP`.
///
/// The state column is what makes this correct: netstat also prints a
/// `TIME_WAIT` or `CLOSED` line for every client that recently connected to the
/// port, and a local gateway collects those constantly. Those lines carry the
/// *client's* pid in the last column, so matching on the local address alone
/// would `taskkill` whatever process happened to talk to the gateway. Only
/// `LISTENING` rows are considered, and only their local address.
///
/// Split out from the command so the parse is testable from a string.
#[cfg(windows)]
fn pids_from_netstat(text: &str, port: u16) -> Vec<u32> {
    // `  TCP    0.0.0.0:20129    0.0.0.0:0    LISTENING    1234`
    let needle = format!(":{port}");
    let mut pids = Vec::new();
    for line in text.lines() {
        let cols: Vec<&str> = line.split_whitespace().collect();
        let [proto, local, _remote, state, rest @ ..] = cols.as_slice() else {
            continue;
        };
        if !proto.eq_ignore_ascii_case("TCP")
            || !state.eq_ignore_ascii_case("LISTENING")
            || !local.ends_with(&needle)
        {
            continue;
        }
        // `netstat -o` appends the pid; a `TIME_WAIT` row prints the pid as 0
        // and is already filtered by the state check.
        if let Some(pid) = rest.last().and_then(|p| p.parse::<u32>().ok())
            && pid != 0
        {
            pids.push(pid);
        }
    }
    pids.sort_unstable();
    pids.dedup();
    pids
}

#[cfg(unix)]
fn pids_listening_on(port: u16) -> Option<Vec<u32>> {
    // Prefer `lsof`; fall back to `ss`. Both are commonly present, neither is
    // guaranteed.
    if let Ok(out) = Command::new("lsof")
        .args(["-ti", &format!("tcp:{port}"), "-sTCP:LISTEN"])
        .output()
        && out.status.success()
    {
        let pids: Vec<u32> = String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| l.trim().parse().ok())
            .collect();
        return Some(pids);
    }
    let out = Command::new("ss")
        .args(["-lptn", &format!("sport = :{port}")])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mut pids = Vec::new();
    for line in text.lines() {
        // `users:(("node",pid=1234,fd=20))`
        for part in line.split("pid=").skip(1) {
            let digits: String = part.chars().take_while(char::is_ascii_digit).collect();
            if let Ok(pid) = digits.parse::<u32>() {
                pids.push(pid);
            }
        }
    }
    pids.sort_unstable();
    pids.dedup();
    Some(pids)
}

#[cfg(windows)]
fn kill_pid(pid: u32) -> bool {
    Command::new("taskkill")
        .args(["/F", "/PID", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

#[cfg(unix)]
fn kill_pid(pid: u32) -> bool {
    Command::new("kill")
        .args(["-9", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Run `serve` and restart it on an abnormal exit, up to `RESTART_ATTEMPTS`
/// times.
///
/// A clean exit (Ctrl-C, or the shutdown route) is not a crash and stops the
/// loop. A crash is restarted up to `RESTART_ATTEMPTS` times.
pub fn supervise(mut serve: impl FnMut() -> anyhow::Result<()>) -> anyhow::Result<()> {
    let mut attempts = 0;
    loop {
        match serve() {
            Ok(()) => return Ok(()),
            Err(e) => {
                if attempts >= RESTART_ATTEMPTS {
                    return Err(e);
                }
                attempts += 1;
                tracing::error!("server exited: {e:#}; restarting ({attempts}/{RESTART_ATTEMPTS})");
                std::thread::sleep(RESTART_DELAY);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn supervise_restarts_a_failing_server_then_gives_up() {
        let calls = Cell::new(0u32);
        let result = supervise(|| {
            calls.set(calls.get() + 1);
            Err(anyhow::anyhow!("boom"))
        });
        assert!(result.is_err());
        // Initial attempt plus RESTART_ATTEMPTS retries.
        assert_eq!(calls.get(), RESTART_ATTEMPTS + 1);
    }

    #[test]
    fn supervise_stops_on_a_clean_exit() {
        let calls = Cell::new(0u32);
        supervise(|| {
            calls.set(calls.get() + 1);
            Ok(())
        })
        .unwrap();
        assert_eq!(calls.get(), 1, "a clean exit must not restart");
    }

    #[test]
    fn supervise_recovers_from_a_crash() {
        let calls = Cell::new(0u32);
        supervise(|| {
            let n = calls.get() + 1;
            calls.set(n);
            if n == 1 {
                Err(anyhow::anyhow!("first run crashes"))
            } else {
                Ok(())
            }
        })
        .unwrap();
        assert_eq!(calls.get(), 2);
    }

    #[test]
    fn kill_port_holder_tolerates_an_unused_port() {
        // Port 1 is privileged and nothing binds it in a test runner.
        assert!(!kill_port_holder(1));
    }

    #[cfg(windows)]
    #[test]
    fn netstat_parse_takes_only_the_listener() {
        // Real `netstat -ano -p TCP` shape, with the client rows a busy local
        // gateway always accumulates: a TIME_WAIT row here belongs to pid 4242,
        // which is a *client*, and killing it would be the bug.
        let text = "  TCP    0.0.0.0:20129          0.0.0.0:0              LISTENING       1234\n  \
                    TCP    127.0.0.1:55321        127.0.0.1:20129        TIME_WAIT       4242\n  \
                    TCP    127.0.0.1:55322        127.0.0.1:20129        CLOSED          0\n  \
                    TCP    0.0.0.0:445            0.0.0.0:0              LISTENING       4\n  \
                    TCP    [::]:20129             [::]:0                 LISTENING       1234";
        assert_eq!(pids_from_netstat(text, 20129), vec![1234]);
        assert!(pids_from_netstat(text, 9999).is_empty());
    }
}
