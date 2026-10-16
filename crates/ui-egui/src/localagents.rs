//! Agents on this computer (desktop): the assistant hands a task to a coding agent the user has
//! (Claude Code, Codex, Gemini CLI…) through ACP ([`rustgis_assistant::acp`]). The agent reaches
//! the open map through the app's MCP relay with a token for this run only, works in a folder of
//! its own that is kept for the user, and asks the user (through the assistant pane) before
//! acting.
//!
//! [`Run`] runs one agent on a task, passes its lines through the ACP session, and stops it (with
//! every process it started) when dropped; its token is revoked with it.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rustgis_assistant::acp;

/// Windows' flag for a console program started without a console window.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// The most of an agent's error output kept, for a report when it fails without one.
const MAX_STDERR: usize = 4096;

/// An agent working on a task.
pub struct Run {
    /// Where it works; kept afterwards.
    pub work: PathBuf,
    events: Arc<Mutex<Vec<acp::Event>>>,
    session: Arc<Mutex<acp::Session>>,
    stdin: Arc<Mutex<Option<ChildStdin>>>,
    child: Arc<Mutex<Option<Child>>>,
    pid: u32,
    /// Its relay token: revoked when the run is dropped.
    _grant: rustgis_assistant::relay::Grant,
}

/// Writes one line to the agent.
fn send(stdin: &Mutex<Option<ChildStdin>>, line: &str) {
    if let Ok(mut s) = stdin.lock()
        && let Some(s) = s.as_mut()
    {
        let _ = s.write_all(line.as_bytes()).and_then(|()| s.write_all(b"\n")).and_then(|()| s.flush());
    }
}

/// `yyyy-mm-dd-hhmmss` (UTC) for a folder name.
fn stamp() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let (days, rest) = ((secs / 86_400) as i64, secs % 86_400);
    // Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}-{:02}{:02}{:02}", rest / 3600, rest / 60 % 60, rest % 60)
}

/// A new folder for a task: `Documents/RustGIS/Agent work/<time>-<agent>`.
pub fn work_folder(agent: &str) -> Result<PathBuf, String> {
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from).ok_or("no home folder")?;
    let docs = Some(home.join("Documents")).filter(|d| d.is_dir()).unwrap_or(home);
    let dir = docs.join("RustGIS").join("Agent work").join(format!("{}-{agent}", stamp()));
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    Ok(dir)
}

impl Run {
    /// Starts `spec` (with `command`, from [`acp::command`]) on `task`, reaching the map through
    /// `relay` with a token of its own.
    /// `cli` is `rustgis-cli`, which serves the map to the agent over MCP; `work` is the folder
    /// it works in ([`work_folder`]).
    pub fn start(
        spec: &'static acp::AgentSpec,
        command: &[String],
        task: &str,
        relay: &rustgis_assistant::relay::Relay,
        cli: &std::path::Path,
        work: PathBuf,
        auto: bool,
        ctx: egui::Context,
    ) -> Result<Run, String> {
        // Its own token: what it may do on the map is this run's, whatever other agents may.
        let grant = relay.grant(true)?;
        let mcp = vec![
            cli.display().to_string(),
            "mcp".into(),
            "--attach".into(),
            relay.port().to_string(),
            "--token-file".into(),
            grant.token_file().display().to_string(),
            "--yes".into(),
        ];
        let (session, init) = acp::Session::new(spec.label, task, &work.display().to_string(), mcp, auto);
        let (program, args) = command.split_first().ok_or("no command")?;
        let mut cmd = Command::new(program);
        cmd.args(args).current_dir(&work).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        // Its own process group, so stopping it stops what it started (Unix).
        #[cfg(unix)]
        std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
        // No console window on Windows: what the agent does shows in its window in RustGIS.
        #[cfg(windows)]
        std::os::windows::process::CommandExt::creation_flags(&mut cmd, CREATE_NO_WINDOW);
        let mut child = cmd.spawn().map_err(|e| format!("{program}: {e}"))?;
        let (stdin, stdout, stderr) = (child.stdin.take(), child.stdout.take(), child.stderr.take());
        let pid = child.id();
        let run = Run {
            work,
            events: Arc::default(),
            session: Arc::new(Mutex::new(session)),
            stdin: Arc::new(Mutex::new(stdin)),
            child: Arc::new(Mutex::new(Some(child))),
            pid,
            _grant: grant,
        };
        send(&run.stdin, &init);
        let errors: Arc<Mutex<String>> = Arc::default();
        if let Some(mut stderr) = stderr {
            let errors = errors.clone();
            std::thread::spawn(move || {
                let mut buf = [0u8; 1024];
                while let Ok(n) = stderr.read(&mut buf) {
                    let Some(piece) = buf.get(..n).filter(|p| !p.is_empty()) else { break };
                    if let Ok(mut e) = errors.lock() {
                        e.push_str(&String::from_utf8_lossy(piece));
                        let over = e.len().saturating_sub(MAX_STDERR);
                        if over > 0 {
                            let cut = (over..e.len()).find(|i| e.is_char_boundary(*i)).unwrap_or(e.len());
                            e.drain(..cut);
                        }
                    }
                }
            });
        }
        let (events, session, stdin, child) = (run.events.clone(), run.session.clone(), run.stdin.clone(), run.child.clone());
        std::thread::spawn(move || {
            let push = |new: Vec<acp::Event>| {
                if !new.is_empty() {
                    if let Ok(mut e) = events.lock() {
                        e.extend(new);
                    }
                    ctx.request_repaint();
                }
            };
            if let Some(stdout) = stdout {
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    let (out, new) = match session.lock() {
                        Ok(mut s) => s.line(&line),
                        Err(_) => break,
                    };
                    for l in out {
                        send(&stdin, &l);
                    }
                    push(new);
                }
            }
            // Its output ended: wait for it to exit without holding the lock (Stop must not wait).
            let ok = loop {
                let status = child.lock().ok().and_then(|mut c| c.as_mut().map(Child::try_wait));
                match status {
                    Some(Ok(Some(s))) => break s.success(),
                    Some(Ok(None)) => std::thread::sleep(Duration::from_millis(50)),
                    _ => break false,
                }
            };
            let why = errors.lock().map(|e| e.clone()).unwrap_or_default();
            let new = session.lock().map(|mut s| s.exited(ok, &why)).unwrap_or_default();
            push(new);
        });
        Ok(run)
    }

    /// What it did and asked since last asked.
    pub fn events(&self) -> Vec<acp::Event> {
        self.events.lock().map(|mut e| std::mem::take(&mut *e)).unwrap_or_default()
    }

    /// A follow-up task, once it has finished the last one; false when it can't take one.
    pub fn follow_up(&self, task: &str) -> bool {
        let lines = self.session.lock().ok().and_then(|mut s| s.follow_up(task));
        match lines {
            Some(lines) => {
                for line in lines {
                    send(&self.stdin, &line);
                }
                true
            }
            None => false,
        }
    }

    /// Whether the user lets it act without asking (its full-access mode), from its next task on.
    pub fn set_auto(&self, auto: bool) {
        if let Ok(mut s) = self.session.lock() {
            s.set_auto(auto);
        }
    }

    /// It can't go on (it failed to start, or its process ended).
    pub fn finished(&self) -> bool {
        self.session.lock().map(|s| s.finished()).unwrap_or(true)
    }

    /// Answers a permission request.
    pub fn answer(&self, p: &acp::Permission, choice: acp::Choice) {
        let line = self.session.lock().ok().map(|s| s.answer(p, choice));
        if let Some(line) = line {
            send(&self.stdin, &line);
        }
    }
}

impl Drop for Run {
    /// Stopping the assistant stops the agent and every process it started: an agent started
    /// through npx runs as `cmd.exe` → `node` on Windows, so the whole tree goes.
    fn drop(&mut self) {
        let running = self.child.lock().ok().and_then(|mut c| c.as_mut().map(|c| matches!(c.try_wait(), Ok(None)))).unwrap_or(false);
        if !running {
            return;
        }
        // Asked to stop first (ACP's cancel), then stopped regardless.
        if let Some(cancel) = self.session.lock().ok().and_then(|s| s.cancel()) {
            send(&self.stdin, &cancel);
        }
        #[cfg(windows)]
        {
            let mut kill = Command::new("taskkill");
            std::os::windows::process::CommandExt::creation_flags(&mut kill, CREATE_NO_WINDOW);
            let _ = kill.args(["/T", "/F", "/PID", &self.pid.to_string()]).stdout(Stdio::null()).stderr(Stdio::null()).status();
        }
        #[cfg(unix)]
        {
            // The process group the agent leads (it was started as one), by the system call: not
            // the `kill` program, whose reading of a negative id varies (procps-ng 4.0.2 takes
            // `kill -KILL -<pid>` as every process of the user). An id of 0 is refused, so this
            // never widens to the caller's own group.
            if let Some(pgid) = i32::try_from(self.pid).ok().and_then(rustix::process::Pid::from_raw) {
                let _ = rustix::process::kill_process_group(pgid, rustix::process::Signal::KILL);
            }
        }
        if let Ok(mut c) = self.child.lock()
            && let Some(child) = c.as_mut()
        {
            let _ = child.kill();
        }
    }
}

#[cfg(test)]
mod tests {
    //! A fake ACP agent, played by this test binary: the run's processes, lines, follow-ups,
    //! permission answers and Stop, for real.
    use std::io::{BufRead, Write};
    use std::time::{Duration, Instant};

    use rustgis_assistant::acp::{AgentSpec, Choice, Event};
    use serde_json::{Value, json};

    use super::Run;

    /// The argument that makes this binary the fake agent (also a test filter matching nothing).
    const CHILD_MARK: &str = "rustgis-acp-child-mode";

    /// The fake agent: ACP over standard input and output. A task with ASK asks for permission
    /// first; one with HANG works until it's cancelled; any other is done at once.
    #[test]
    fn fake_acp_agent() {
        if !std::env::args().any(|a| a == CHILD_MARK) {
            return;
        }
        let mut out = std::io::stdout();
        let _ = writeln!(out);
        let send = |out: &mut std::io::Stdout, v: Value| {
            let _ = writeln!(out, "{v}");
            let _ = out.flush();
        };
        let update = |u: Value| json!({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": "fake", "update": u}});
        let say = |t: &str| update(json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": t}}));
        let mut lines = std::io::stdin().lock().lines().map_while(Result::ok);
        let mut hanging: Option<Value> = None;
        while let Some(line) = lines.next() {
            let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
            let id = msg["id"].clone();
            match msg["method"].as_str() {
                Some("initialize") => send(
                    &mut out,
                    json!({"jsonrpc": "2.0", "id": id, "result": {"protocolVersion": 1, "agentCapabilities": {}, "authMethods": []}}),
                ),
                Some("session/new") => send(&mut out, json!({"jsonrpc": "2.0", "id": id, "result": {"sessionId": "fake"}})),
                Some("session/cancel") => {
                    if let Some(pid) = hanging.take() {
                        send(&mut out, json!({"jsonrpc": "2.0", "id": pid, "result": {"stopReason": "cancelled"}}));
                    }
                }
                Some("session/prompt") => {
                    let task = msg["params"]["prompt"][0]["text"].as_str().unwrap_or("").to_string();
                    if task.contains("HANG") {
                        send(
                            &mut out,
                            update(
                                json!({"sessionUpdate": "tool_call", "toolCallId": "h", "title": "Working forever", "status": "in_progress"}),
                            ),
                        );
                        hanging = Some(id);
                        continue;
                    }
                    if task.contains("ASK") {
                        send(
                            &mut out,
                            update(
                                json!({"sessionUpdate": "tool_call", "toolCallId": "t1", "title": "Fetch the gauges", "status": "pending"}),
                            ),
                        );
                        send(
                            &mut out,
                            json!({"jsonrpc": "2.0", "id": 100, "method": "session/request_permission", "params": {"sessionId": "fake",
                            "toolCall": {"toolCallId": "t1", "rawInput": {"url": "https://waterservices.usgs.gov/"}},
                            "options": [{"optionId": "yes", "name": "Allow", "kind": "allow_once"}, {"optionId": "no", "name": "Reject", "kind": "reject_once"}]}}),
                        );
                        let answer = lines.by_ref().find_map(|l| serde_json::from_str::<Value>(&l).ok().filter(|v| v["id"] == 100));
                        let chosen = answer.and_then(|a| a["result"]["outcome"]["optionId"].as_str().map(str::to_string));
                        send(&mut out, say(if chosen.as_deref() == Some("yes") { "Allowed: fetched 3 gauges." } else { "Declined." }));
                    } else {
                        send(&mut out, say("Did "));
                        send(&mut out, say("the task."));
                    }
                    send(&mut out, json!({"jsonrpc": "2.0", "id": id, "result": {"stopReason": "end_turn"}}));
                }
                _ => {}
            }
        }
    }

    /// Waits for the run's events up to the end of a task (or five seconds).
    fn until_done(run: &Run) -> Vec<Event> {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut all = Vec::new();
        while Instant::now() < deadline {
            let new = run.events();
            let done = new.iter().any(|e| matches!(e, Event::Done { .. } | Event::Permission(_)));
            all.extend(new);
            if done {
                return all;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        all
    }

    fn report(events: &[Event]) -> Option<(String, bool)> {
        events.iter().find_map(|e| match e {
            Event::Done { report, error } => Some((report.clone(), *error)),
            _ => None,
        })
    }

    #[test]
    fn an_agent_thread_works_takes_follow_ups_asks_and_stops() {
        let relay = rustgis_assistant::relay::Relay::start(0).unwrap();
        let exe = std::env::current_exe().unwrap();
        let command: Vec<String> = [
            exe.display().to_string().as_str(),
            "localagents::tests::fake_acp_agent",
            CHILD_MARK,
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let work = std::env::temp_dir().join(format!("rustgis-acp-test-{}", std::process::id()));
        std::fs::create_dir_all(&work).unwrap();
        let spec = AgentSpec::from_id("goose").unwrap();
        let run = Run::start(spec, &command, "Say hi", &relay, &exe, work.clone(), false, egui::Context::default()).unwrap();
        // The first task: its message, whole, and its report.
        let ev = until_done(&run);
        assert!(ev.contains(&Event::Text("Did the task.".into())), "{ev:?}");
        assert_eq!(report(&ev), Some(("Did the task.".into(), false)));
        assert!(!run.finished());
        // A follow-up that asks for permission: allowed.
        assert!(run.follow_up("ASK to fetch"));
        assert!(!run.follow_up("one at a time"), "a working thread takes no second task");
        let ev = until_done(&run);
        assert!(ev.contains(&Event::Tool("Fetch the gauges".into())), "{ev:?}");
        let Some(Event::Permission(p)) = ev.iter().find(|e| matches!(e, Event::Permission(_))).cloned() else {
            panic!("no request: {ev:?}")
        };
        assert!(p.detail.contains("waterservices.usgs.gov"));
        run.answer(&p, Choice::Allow);
        assert_eq!(report(&until_done(&run)), Some(("Allowed: fetched 3 gauges.".into(), false)));
        // Declined.
        assert!(run.follow_up("ASK again"));
        let Some(Event::Permission(p)) = until_done(&run).into_iter().find(|e| matches!(e, Event::Permission(_))) else {
            panic!("no request")
        };
        run.answer(&p, Choice::Reject);
        assert_eq!(report(&until_done(&run)), Some(("Declined.".into(), false)));
        // Its relay token is its own, revoked with it; Stop ends a task that never would.
        assert!(run.follow_up("HANG"));
        std::thread::sleep(Duration::from_millis(300));
        let pid = run.pid;
        let grant_file = run._grant.token_file().to_path_buf();
        assert!(grant_file.exists());
        drop(run);
        assert!(!grant_file.exists(), "the run's relay token is revoked with it");
        let deadline = Instant::now() + Duration::from_secs(10);
        while alive(pid) {
            assert!(Instant::now() < deadline, "the agent's process outlived Stop");
            std::thread::sleep(Duration::from_millis(50));
        }
        relay.stop();
        let _ = std::fs::remove_dir_all(&work);
    }

    /// Whether a process is still running.
    fn alive(pid: u32) -> bool {
        #[cfg(unix)]
        {
            std::path::Path::new(&format!("/proc/{pid}")).exists()
                && !std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|s| s.contains(") Z "))
        }
        #[cfg(windows)]
        {
            std::process::Command::new("tasklist")
                .args(["/FI", &format!("PID eq {pid}"), "/NH"])
                .output()
                .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains(&pid.to_string()))
        }
    }
}
