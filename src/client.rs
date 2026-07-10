//! `Pmr` — the client handle. This IS the programmatic API; the CLI is just its
//! first consumer. Synchronous std unix-socket I/O so library users don't
//! inherit a tokio dependency.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::de::DeserializeOwned;

use crate::config::AppConfig;
use crate::ipc::{
    Event, EventFrame, Method, PingReply, ProcessSnapshot, Request, Response, Target,
};
use crate::paths;

/// Connection to the pmr daemon.
pub struct Pmr {
    stream: BufReader<UnixStream>,
    next_id: u64,
}

impl Pmr {
    /// Connect to the daemon, spawning it first if it isn't running.
    pub fn connect() -> Result<Pmr> {
        match Self::try_connect() {
            Ok(pmr) => Ok(pmr),
            Err(_) => {
                first_run_banner();
                spawn_daemon()?;
                let mut pmr = wait_connect(Duration::from_secs(5))?;
                pmr.check_version()?;
                Ok(pmr)
            }
        }
    }

    /// Connect only if the daemon is already running.
    pub fn try_connect() -> Result<Pmr> {
        let stream = UnixStream::connect(paths::rpc_sock())
            .with_context(|| format!("daemon not reachable at {}", paths::rpc_sock().display()))?;
        let mut pmr = Pmr {
            stream: BufReader::new(stream),
            next_id: 0,
        };
        pmr.check_version()?;
        Ok(pmr)
    }

    fn check_version(&mut self) -> Result<()> {
        let ping: PingReply = self.call(Method::Ping)?;
        if ping.version != crate::VERSION {
            eprintln!(
                "[pmr] warning: daemon is v{} but client is v{} — run `pmr kill && pmr resurrect` to update",
                ping.version,
                crate::VERSION
            );
        }
        Ok(())
    }

    /// Low-level RPC call. Prefer the typed helpers below.
    pub fn call<T: DeserializeOwned>(&mut self, method: Method) -> Result<T> {
        self.next_id += 1;
        let req = Request {
            id: self.next_id,
            call: method,
        };
        let mut line = serde_json::to_string(&req)?;
        line.push('\n');
        self.stream.get_mut().write_all(line.as_bytes())?;

        let mut reply = String::new();
        self.stream
            .read_line(&mut reply)
            .context("daemon closed the connection")?;
        if reply.is_empty() {
            bail!("daemon closed the connection");
        }
        let resp: Response = serde_json::from_str(&reply)
            .with_context(|| format!("invalid daemon reply: {}", reply.trim()))?;
        if let Some(err) = resp.error {
            bail!("{err}");
        }
        let value = resp.result.unwrap_or(serde_json::Value::Null);
        serde_json::from_value(value).context("unexpected daemon reply shape")
    }

    // --- typed API ---

    pub fn ping(&mut self) -> Result<PingReply> {
        self.call(Method::Ping)
    }

    /// Start one app config (daemon expands `instances`).
    pub fn start(&mut self, app: AppConfig) -> Result<Vec<ProcessSnapshot>> {
        self.start_many(vec![app])
    }

    pub fn start_many(&mut self, apps: Vec<AppConfig>) -> Result<Vec<ProcessSnapshot>> {
        for app in &apps {
            app.validate()?;
        }
        self.call(Method::Start { apps })
    }

    pub fn stop(&mut self, target: impl Into<TargetArg>) -> Result<Vec<ProcessSnapshot>> {
        let target = target.into().0;
        self.call(Method::Stop { target })
    }

    pub fn restart(&mut self, target: impl Into<TargetArg>) -> Result<Vec<ProcessSnapshot>> {
        let target = target.into().0;
        self.call(Method::Restart { target })
    }

    pub fn delete(&mut self, target: impl Into<TargetArg>) -> Result<Vec<ProcessSnapshot>> {
        let target = target.into().0;
        self.call(Method::Delete { target })
    }

    pub fn reset(&mut self, target: impl Into<TargetArg>) -> Result<Vec<ProcessSnapshot>> {
        let target = target.into().0;
        self.call(Method::Reset { target })
    }

    pub fn list(&mut self) -> Result<Vec<ProcessSnapshot>> {
        self.call(Method::List)
    }

    pub fn describe(&mut self, target: impl Into<TargetArg>) -> Result<Vec<ProcessSnapshot>> {
        let target = target.into().0;
        self.call(Method::Describe { target })
    }

    pub fn scale(&mut self, name: &str, instances: u32) -> Result<Vec<ProcessSnapshot>> {
        self.call(Method::Scale {
            name: name.into(),
            instances,
        })
    }

    pub fn send_signal(&mut self, target: impl Into<TargetArg>, signal: &str) -> Result<u32> {
        let target = target.into().0;
        #[derive(serde::Deserialize)]
        struct Sent {
            sent: u32,
        }
        let r: Sent = self.call(Method::SendSignal {
            target,
            signal: signal.into(),
        })?;
        Ok(r.sent)
    }

    pub fn flush(&mut self, target: Option<Target>) -> Result<()> {
        let _: serde_json::Value = self.call(Method::Flush { target })?;
        Ok(())
    }

    pub fn reload_logs(&mut self) -> Result<()> {
        let _: serde_json::Value = self.call(Method::ReloadLogs)?;
        Ok(())
    }

    pub fn save(&mut self) -> Result<String> {
        #[derive(serde::Deserialize)]
        struct Saved {
            path: String,
        }
        let r: Saved = self.call(Method::Save)?;
        Ok(r.path)
    }

    pub fn resurrect(&mut self) -> Result<Vec<ProcessSnapshot>> {
        self.call(Method::Resurrect)
    }

    /// Ask the daemon to stop everything and exit.
    pub fn kill_daemon(&mut self) -> Result<()> {
        let _: serde_json::Value = self.call(Method::Kill)?;
        Ok(())
    }

    /// Switch this connection into an event stream. Consumes the handle:
    /// a subscribed connection speaks nothing else.
    pub fn subscribe(mut self, topics: &[&str], target: Option<Target>) -> Result<EventStream> {
        let _: serde_json::Value = self.call(Method::Subscribe {
            topics: topics.iter().map(|s| s.to_string()).collect(),
            target,
        })?;
        Ok(EventStream {
            stream: self.stream,
        })
    }
}

/// Blocking iterator over daemon events (log lines, lifecycle events).
pub struct EventStream {
    stream: BufReader<UnixStream>,
}

impl Iterator for EventStream {
    type Item = Result<Event>;

    fn next(&mut self) -> Option<Self::Item> {
        let mut line = String::new();
        match self.stream.read_line(&mut line) {
            Ok(0) => None, // daemon gone
            Ok(_) => {
                let parse = || -> Result<Event> {
                    let frame: EventFrame = serde_json::from_str(&line)?;
                    Ok(serde_json::from_value(frame.data)?)
                };
                Some(parse())
            }
            Err(e) => Some(Err(e.into())),
        }
    }
}

/// Ergonomic conversion so `pmr.stop("bot")`, `pmr.stop(3)` and
/// `pmr.stop(Target::All)` all work.
pub struct TargetArg(pub Target);

impl From<&str> for TargetArg {
    fn from(s: &str) -> Self {
        TargetArg(Target::parse(s))
    }
}
impl From<String> for TargetArg {
    fn from(s: String) -> Self {
        TargetArg(Target::parse(&s))
    }
}
impl From<u32> for TargetArg {
    fn from(id: u32) -> Self {
        TargetArg(Target::Ids(vec![id]))
    }
}
impl From<Target> for TargetArg {
    fn from(t: Target) -> Self {
        TargetArg(t)
    }
}

/// pm2-style welcome banner, shown once: only when `~/.pmr` doesn't exist yet
/// and we're on an interactive terminal.
fn first_run_banner() {
    use std::io::IsTerminal;
    if crate::paths::home().exists() || !std::io::stdout().is_terminal() {
        return;
    }
    let cyan = "\x1b[36m";
    let bold = "\x1b[1m";
    let dim = "\x1b[2m";
    let reset = "\x1b[0m";
    println!(
        r#"{cyan}{bold}
        ____  ____ ___  _____
       / __ \/ __ `__ \/ ___/
      / /_/ / / / / / / /
     / .___/_/ /_/ /_/_/
    /_/{reset}
    {bold}pmr v{version}{reset} — efficient, fast, production-grade process manager
    {dim}the pm2 workflow in one small binary: ~14x less memory, no Node.js needed{reset}

      pmr start app.js         start an app (auto-restart on crash)
      pmr ls                   process table
      pmr logs                 tail + live stream
      pmr save && pmr startup  survive reboots

    {dim}docs: https://github.com/zdanysfa/pmr{reset}
"#,
        version = crate::VERSION,
    );
}

/// Spawn the daemon detached, stdio appended to `pmr.log`.
fn spawn_daemon() -> Result<()> {
    paths::ensure_dirs()?;
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(paths::daemon_log())?;
    let exe = find_pmr_bin()?;
    let mut cmd = Command::new(exe);
    cmd.arg("daemon")
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    cmd.spawn().context("failed to spawn pmr daemon")?;
    Ok(())
}

/// Locate the `pmr` binary to run as the daemon. When pmr is used as a
/// library, `current_exe()` is the host application — spawning that with
/// `daemon` would loop forever, so resolve an actual pmr binary instead.
fn find_pmr_bin() -> Result<PathBuf> {
    if let Ok(p) = std::env::var("PMR_BIN")
        && !p.is_empty()
    {
        return Ok(PathBuf::from(p));
    }
    if let Ok(exe) = std::env::current_exe() {
        if exe.file_name().is_some_and(|n| n == "pmr") {
            return Ok(exe);
        }
        // Cargo layouts: examples live in target/debug/examples/, test and
        // host binaries in target/debug{,/deps}/ — look for a sibling `pmr`.
        for dir in exe.ancestors().skip(1).take(3) {
            let candidate = dir.join("pmr");
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    // PATH lookup.
    if let Ok(path) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path) {
            let candidate = dir.join("pmr");
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    bail!(
        "cannot find the `pmr` binary to launch the daemon — \
         install it (`cargo install pmr`) or set PMR_BIN to its path"
    )
}

/// Poll-connect until the freshly spawned daemon binds its socket.
fn wait_connect(timeout: Duration) -> Result<Pmr> {
    let deadline = Instant::now() + timeout;
    loop {
        match UnixStream::connect(paths::rpc_sock()) {
            Ok(stream) => {
                return Ok(Pmr {
                    stream: BufReader::new(stream),
                    next_id: 0,
                });
            }
            Err(e) => {
                if Instant::now() >= deadline {
                    bail!(
                        "daemon did not come up within {:?} ({e}); check {}",
                        timeout,
                        paths::daemon_log().display()
                    );
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
}
