//! One supervisor task per process instance: spawn → watch → kill/restart.
//! All pm2 restart semantics live in the pure `decide_restart`.

use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use anyhow::{Context as _, Result};
use nix::sys::signal::{Signal, kill, killpg};
use nix::unistd::Pid;
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot};

use crate::config::AppConfig;
use crate::daemon::state::{Ctx, SupervisorCmd, now_ms};
use crate::daemon::{dlog, logs};
use crate::ipc::Status;

/// Exponential backoff cap, same as pm2.
const BACKOFF_CAP_MS: u64 = 15_000;

/// What to do after an unexpected exit.
#[derive(Debug, PartialEq)]
pub enum RestartDecision {
    /// Clean stop, don't restart.
    Stop,
    /// Too many unstable restarts, give up.
    GiveUp,
    /// Respawn after this delay; carries updated counters.
    Restart {
        delay_ms: u64,
        unstable_restarts: u32,
        prev_restart_delay: u64,
    },
}

/// Inputs to the restart decision, captured from Proc + exit.
pub struct ExitFacts {
    pub status: Status,
    pub autorestart: bool,
    pub exit_code: Option<i32>,
    pub stop_exit_codes: Vec<i32>,
    pub min_uptime_ms: u64,
    pub max_restarts: u32,
    pub unstable_restarts: u32,
    pub created_at_ms: i64,
    pub online_since_ms: i64,
    pub now_ms: i64,
    pub restart_delay_ms: u64,
    pub backoff_base_ms: u64,
    pub prev_restart_delay_ms: u64,
}

/// pm2 `God.handleExit` semantics (God.js:404-523).
pub fn decide_restart(f: &ExitFacts) -> RestartDecision {
    let stopping = matches!(
        f.status,
        Status::Stopping | Status::Stopped | Status::Errored
    ) || !f.autorestart
        || f.exit_code.is_some_and(|c| f.stop_exit_codes.contains(&c));
    if stopping {
        return RestartDecision::Stop;
    }

    // Unstable-restart detection.
    let mut unstable = f.unstable_restarts;
    let window = f.min_uptime_ms.saturating_mul(f.max_restarts as u64) as i64;
    if (f.now_ms - f.created_at_ms) < window
        && (f.now_ms - f.online_since_ms) < f.min_uptime_ms as i64
    {
        unstable += 1;
    }
    if unstable >= f.max_restarts {
        return RestartDecision::GiveUp;
    }

    // Delay: exponential backoff wins over fixed delay when configured.
    let (delay, prev) = if f.backoff_base_ms > 0 {
        let next = if f.prev_restart_delay_ms == 0 {
            f.backoff_base_ms
        } else {
            BACKOFF_CAP_MS.min(f.prev_restart_delay_ms * 3 / 2)
        };
        (next, next)
    } else {
        (f.restart_delay_ms, 0)
    };

    RestartDecision::Restart {
        delay_ms: delay,
        unstable_restarts: unstable,
        prev_restart_delay: prev,
    }
}

/// Spawn a supervisor task for `pm_id`. The Proc row must already exist.
/// `spawn_ack` reports the first spawn attempt so `start` RPC can respond
/// with a real error when the binary is missing.
pub fn spawn(ctx: Arc<Ctx>, pm_id: u32, spawn_ack: oneshot::Sender<Result<(), String>>) {
    let (tx, rx) = mpsc::channel(8);
    {
        let mut table = ctx.table.lock().unwrap();
        let Some(proc) = table.procs.get_mut(&pm_id) else {
            let _ = spawn_ack.send(Err(format!("process {pm_id} vanished before start")));
            return;
        };
        proc.cmd_tx = Some(tx);
        proc.status = Status::Launching;
    }
    tokio::spawn(run(ctx, pm_id, rx, spawn_ack));
}

async fn run(
    ctx: Arc<Ctx>,
    pm_id: u32,
    mut cmd_rx: mpsc::Receiver<SupervisorCmd>,
    spawn_ack: oneshot::Sender<Result<(), String>>,
) {
    let mut first_ack = Some(spawn_ack);

    'lifecycle: loop {
        // Snapshot what we need under the lock.
        let (config, instance, name, env, out_file, error_file, pid_file) = {
            let table = ctx.table.lock().unwrap();
            let Some(p) = table.procs.get(&pm_id) else {
                return;
            };
            (
                p.config.clone(),
                p.instance,
                p.name(),
                p.child_env(),
                p.out_file.clone(),
                p.error_file.clone(),
                p.pid_file.clone(),
            )
        };
        let _ = instance;

        let mut child = match spawn_child(&config, &env) {
            Ok(c) => c,
            Err(e) => {
                dlog!("[{name}:{pm_id}] spawn failed: {e:#}");
                set_dead(&ctx, pm_id, Status::Errored, None);
                ctx.publish_process_event(pm_id, &name, "errored");
                if let Some(ack) = first_ack.take() {
                    let _ = ack.send(Err(format!("{e:#}")));
                }
                clear_cmd_tx(&ctx, pm_id);
                return;
            }
        };

        let pid = child.id().unwrap_or(0);
        {
            let mut table = ctx.table.lock().unwrap();
            if let Some(p) = table.procs.get_mut(&pm_id) {
                p.pid = pid;
                p.status = Status::Online;
                p.uptime_ms = Some(now_ms());
                p.exit_code = None;
            }
        }
        let _ = std::fs::write(&pid_file, pid.to_string());
        ctx.publish_process_event(pm_id, &name, "online");
        dlog!("[{name}:{pm_id}] online (pid {pid})");
        if let Some(ack) = first_ack.take() {
            let _ = ack.send(Ok(()));
        }

        // Log pumps own the child's stdout/stderr until it dies.
        logs::pump(&ctx, pm_id, &name, &mut child, &out_file, &error_file);

        // Wait for exit or a command.
        let exited = tokio::select! {
            status = child.wait() => Some(status),
            cmd = cmd_rx.recv() => {
                let cmd = match cmd {
                    Some(c) => c,
                    None => {
                        // Daemon dropped us (shutdown path handles killing separately).
                        return;
                    }
                };
                let terminal = handle_cmd(&ctx, pm_id, &name, &config, &mut child, cmd, &pid_file).await;
                if terminal {
                    return;
                }
                None // Restart command: fall through and respawn
            }
        };

        if let Some(status) = exited {
            let exit_code = status.as_ref().ok().and_then(|s| s.code());
            let _ = std::fs::remove_file(&pid_file);
            let facts = {
                let mut table = ctx.table.lock().unwrap();
                let Some(p) = table.procs.get_mut(&pm_id) else {
                    return;
                };
                p.pid = 0;
                p.exit_code = exit_code;
                ExitFacts {
                    status: p.status,
                    autorestart: p.config.autorestart,
                    exit_code,
                    stop_exit_codes: p.config.stop_exit_codes.clone(),
                    min_uptime_ms: p.config.min_uptime,
                    max_restarts: p.config.max_restarts,
                    unstable_restarts: p.unstable_restarts,
                    created_at_ms: p.created_at,
                    online_since_ms: p.uptime_ms.unwrap_or(p.created_at),
                    now_ms: now_ms(),
                    restart_delay_ms: p.config.restart_delay,
                    backoff_base_ms: p.config.exp_backoff_restart_delay,
                    prev_restart_delay_ms: p.prev_restart_delay,
                }
            };
            ctx.publish_process_event(pm_id, &name, "exit");

            if ctx.shutting_down.load(Ordering::Relaxed) {
                return;
            }

            match decide_restart(&facts) {
                RestartDecision::Stop => {
                    dlog!("[{name}:{pm_id}] exited (code {exit_code:?}), not restarting");
                    set_dead(&ctx, pm_id, Status::Stopped, exit_code);
                    ctx.publish_process_event(pm_id, &name, "stop");
                    clear_cmd_tx(&ctx, pm_id);
                    return;
                }
                RestartDecision::GiveUp => {
                    dlog!("[{name}:{pm_id}] too many unstable restarts, marking errored");
                    {
                        let mut table = ctx.table.lock().unwrap();
                        if let Some(p) = table.procs.get_mut(&pm_id) {
                            p.status = Status::Errored;
                            p.unstable_restarts = p.config.max_restarts;
                        }
                    }
                    ctx.publish_process_event(pm_id, &name, "restart overlimit");
                    clear_cmd_tx(&ctx, pm_id);
                    return;
                }
                RestartDecision::Restart {
                    delay_ms,
                    unstable_restarts,
                    prev_restart_delay,
                } => {
                    {
                        let mut table = ctx.table.lock().unwrap();
                        if let Some(p) = table.procs.get_mut(&pm_id) {
                            p.unstable_restarts = unstable_restarts;
                            p.prev_restart_delay = prev_restart_delay;
                            p.restarts += 1;
                            p.status = if delay_ms > 0 {
                                Status::WaitingRestart
                            } else {
                                Status::Launching
                            };
                        }
                    }
                    dlog!(
                        "[{name}:{pm_id}] exited (code {exit_code:?}), restarting in {delay_ms}ms"
                    );
                    ctx.publish_process_event(pm_id, &name, "restart");

                    if delay_ms > 0 {
                        // The delay races incoming commands so delete/stop work
                        // during a backoff wait.
                        tokio::select! {
                            _ = tokio::time::sleep(Duration::from_millis(delay_ms)) => {}
                            cmd = cmd_rx.recv() => {
                                match cmd {
                                    Some(SupervisorCmd::Restart(ack)) => { let _ = ack.send(()); }
                                    Some(SupervisorCmd::Stop(ack)) => {
                                        set_dead(&ctx, pm_id, Status::Stopped, exit_code);
                                        ctx.publish_process_event(pm_id, &name, "stop");
                                        clear_cmd_tx(&ctx, pm_id);
                                        let _ = ack.send(());
                                        return;
                                    }
                                    Some(SupervisorCmd::Delete(ack)) => {
                                        remove_proc(&ctx, pm_id, &name);
                                        let _ = ack.send(());
                                        return;
                                    }
                                    None => return,
                                }
                            }
                        }
                    }
                    continue 'lifecycle;
                }
            }
        }
    }
}

/// Handle a command received while the child runs.
/// Returns true when the supervisor should terminate.
async fn handle_cmd(
    ctx: &Arc<Ctx>,
    pm_id: u32,
    name: &str,
    config: &AppConfig,
    child: &mut Child,
    cmd: SupervisorCmd,
    pid_file: &std::path::Path,
) -> bool {
    match cmd {
        SupervisorCmd::Stop(ack) => {
            do_kill(ctx, pm_id, name, config, child).await;
            let _ = std::fs::remove_file(pid_file);
            set_dead(ctx, pm_id, Status::Stopped, None);
            ctx.publish_process_event(pm_id, name, "stop");
            clear_cmd_tx(ctx, pm_id);
            let _ = ack.send(());
            true
        }
        SupervisorCmd::Restart(ack) => {
            do_kill(ctx, pm_id, name, config, child).await;
            let _ = std::fs::remove_file(pid_file);
            {
                let mut table = ctx.table.lock().unwrap();
                if let Some(p) = table.procs.get_mut(&pm_id) {
                    p.reset_state(); // manual restart forgives instability, like pm2
                    p.restarts += 1;
                    p.status = Status::Launching;
                    p.pid = 0;
                }
            }
            ctx.publish_process_event(pm_id, name, "restart");
            let _ = ack.send(());
            false
        }
        SupervisorCmd::Delete(ack) => {
            do_kill(ctx, pm_id, name, config, child).await;
            let _ = std::fs::remove_file(pid_file);
            remove_proc(ctx, pm_id, name);
            let _ = ack.send(());
            true
        }
    }
}

/// pm2 kill sequence: kill_signal → wait kill_timeout → SIGKILL → wait again.
async fn do_kill(ctx: &Arc<Ctx>, pm_id: u32, name: &str, config: &AppConfig, child: &mut Child) {
    {
        let mut table = ctx.table.lock().unwrap();
        if let Some(p) = table.procs.get_mut(&pm_id) {
            p.status = Status::Stopping;
        }
    }
    let Some(pid) = child.id() else {
        let _ = child.wait().await; // already dead, just reap
        return;
    };
    let sig: Signal = config.kill_signal.parse().unwrap_or(Signal::SIGINT);

    send_signal_tree(pid, sig, config.treekill);

    let timeout = Duration::from_millis(config.kill_timeout);
    if tokio::time::timeout(timeout, child.wait()).await.is_ok() {
        return;
    }

    dlog!(
        "[{name}:{pm_id}] did not exit after {sig} within {}ms, sending SIGKILL",
        config.kill_timeout
    );
    send_signal_tree(pid, Signal::SIGKILL, config.treekill);
    if tokio::time::timeout(timeout, child.wait()).await.is_err() {
        // Should be impossible after SIGKILL; reap whenever it lands.
        dlog!("[{name}:{pm_id}] survived SIGKILL?! leaving it to the reaper");
        let _ = child.wait().await;
    }
}

/// Signal one pid, or its whole process group when treekill is on.
/// Children are spawned in their own process group, so killpg covers the tree.
// ponytail: process-group kill; descendants that call setpgid escape — ps-tree walk if that bites
pub fn send_signal_tree(pid: u32, sig: Signal, treekill: bool) {
    let p = Pid::from_raw(pid as i32);
    if treekill {
        if killpg(p, sig).is_err() {
            let _ = kill(p, sig);
        }
    } else {
        let _ = kill(p, sig);
    }
}

fn spawn_child(
    config: &AppConfig,
    extra_env: &std::collections::BTreeMap<String, String>,
) -> Result<Child> {
    let interpreter = config.effective_interpreter();
    let mut cmd = match &interpreter {
        Some(interp) => {
            let mut c = Command::new(interp);
            c.args(&config.interpreter_args);
            c.arg(&config.script);
            c
        }
        None => Command::new(&config.script),
    };
    cmd.args(&config.args);

    let cwd = match &config.cwd {
        Some(c) => c.clone(),
        None => std::path::Path::new(&config.script)
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| "/".into())),
    };
    cmd.current_dir(&cwd);
    cmd.envs(extra_env);
    // disable_logs = no pipe at all: zero log-path coupling with the child.
    let (out, err) = if config.disable_logs {
        (Stdio::null(), Stdio::null())
    } else {
        (Stdio::piped(), Stdio::piped())
    };
    cmd.stdin(Stdio::null())
        .stdout(out)
        .stderr(err)
        .kill_on_drop(true);
    cmd.process_group(0); // own group → killpg reaches the tree, daemon signals don't

    if let Some(user) = &config.uid {
        let u = nix::unistd::User::from_name(user)
            .with_context(|| format!("cannot look up user '{user}'"))?
            .with_context(|| format!("no such user '{user}'"))?;
        cmd.uid(u.uid.as_raw());
        let gid = match &config.gid {
            Some(g) => nix::unistd::Group::from_name(g)
                .with_context(|| format!("cannot look up group '{g}'"))?
                .with_context(|| format!("no such group '{g}'"))?
                .gid
                .as_raw(),
            None => u.gid.as_raw(),
        };
        cmd.gid(gid);
    }

    cmd.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::PermissionDenied && config.uid.is_some() {
            anyhow::anyhow!("setting uid/gid requires running the pmr daemon as root")
        } else {
            anyhow::anyhow!(
                "cannot spawn '{}'{}: {e}",
                config.script,
                interpreter
                    .as_deref()
                    .map(|i| format!(" via {i}"))
                    .unwrap_or_default()
            )
        }
    })
}

fn set_dead(ctx: &Ctx, pm_id: u32, status: Status, exit_code: Option<i32>) {
    let mut table = ctx.table.lock().unwrap();
    if let Some(p) = table.procs.get_mut(&pm_id) {
        p.status = status;
        p.pid = 0;
        p.monit = Default::default();
        if exit_code.is_some() {
            p.exit_code = exit_code;
        }
    }
}

fn clear_cmd_tx(ctx: &Ctx, pm_id: u32) {
    let mut table = ctx.table.lock().unwrap();
    if let Some(p) = table.procs.get_mut(&pm_id) {
        p.cmd_tx = None;
    }
}

fn remove_proc(ctx: &Ctx, pm_id: u32, name: &str) {
    {
        let mut table = ctx.table.lock().unwrap();
        if let Some(mut p) = table.procs.remove(&pm_id) {
            p.abort_tasks();
        }
    }
    ctx.watchers.lock().unwrap().remove(&pm_id);
    ctx.publish_process_event(pm_id, name, "delete");
    dlog!("[{name}:{pm_id}] deleted");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_facts() -> ExitFacts {
        ExitFacts {
            status: Status::Online,
            autorestart: true,
            exit_code: Some(1),
            stop_exit_codes: vec![],
            min_uptime_ms: 1000,
            max_restarts: 16,
            unstable_restarts: 0,
            created_at_ms: 0,
            online_since_ms: 100_000,
            now_ms: 200_000, // long-lived: outside unstable window
            restart_delay_ms: 0,
            backoff_base_ms: 0,
            prev_restart_delay_ms: 0,
        }
    }

    #[test]
    fn restarts_on_crash() {
        let d = decide_restart(&base_facts());
        assert_eq!(
            d,
            RestartDecision::Restart {
                delay_ms: 0,
                unstable_restarts: 0,
                prev_restart_delay: 0
            }
        );
    }

    #[test]
    fn no_restart_when_stopped_or_autorestart_off() {
        let mut f = base_facts();
        f.status = Status::Stopping;
        assert_eq!(decide_restart(&f), RestartDecision::Stop);

        let mut f = base_facts();
        f.autorestart = false;
        assert_eq!(decide_restart(&f), RestartDecision::Stop);
    }

    #[test]
    fn stop_exit_codes_respected() {
        let mut f = base_facts();
        f.stop_exit_codes = vec![0, 42];
        f.exit_code = Some(42);
        assert_eq!(decide_restart(&f), RestartDecision::Stop);
        f.exit_code = Some(1);
        assert!(matches!(
            decide_restart(&f),
            RestartDecision::Restart { .. }
        ));
    }

    #[test]
    fn unstable_counting_and_give_up() {
        // Fast crash inside the unstable window increments the counter.
        let mut f = base_facts();
        f.created_at_ms = 0;
        f.online_since_ms = 100;
        f.now_ms = 500; // < min_uptime after start, < window since created
        match decide_restart(&f) {
            RestartDecision::Restart {
                unstable_restarts, ..
            } => assert_eq!(unstable_restarts, 1),
            other => panic!("expected restart, got {other:?}"),
        }

        // At the limit → give up.
        f.unstable_restarts = 15;
        assert_eq!(decide_restart(&f), RestartDecision::GiveUp);
    }

    #[test]
    fn stable_run_does_not_count_unstable() {
        let mut f = base_facts();
        f.unstable_restarts = 10;
        // Ran for a long time — should restart without incrementing.
        match decide_restart(&f) {
            RestartDecision::Restart {
                unstable_restarts, ..
            } => assert_eq!(unstable_restarts, 10),
            other => panic!("expected restart, got {other:?}"),
        }
    }

    #[test]
    fn backoff_sequence() {
        let mut f = base_facts();
        f.backoff_base_ms = 100;
        let mut prev = 0;
        let mut delays = vec![];
        for _ in 0..15 {
            f.prev_restart_delay_ms = prev;
            match decide_restart(&f) {
                RestartDecision::Restart {
                    delay_ms,
                    prev_restart_delay,
                    ..
                } => {
                    delays.push(delay_ms);
                    prev = prev_restart_delay;
                }
                other => panic!("expected restart, got {other:?}"),
            }
        }
        assert_eq!(delays[0], 100);
        assert_eq!(delays[1], 150);
        assert_eq!(delays[2], 225);
        assert_eq!(*delays.last().unwrap(), BACKOFF_CAP_MS);
    }

    #[test]
    fn fixed_delay() {
        let mut f = base_facts();
        f.restart_delay_ms = 2000;
        match decide_restart(&f) {
            RestartDecision::Restart { delay_ms, .. } => assert_eq!(delay_ms, 2000),
            other => panic!("expected restart, got {other:?}"),
        }
    }
}
