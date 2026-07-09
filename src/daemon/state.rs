//! Shared daemon state: the process table and the context handed to every task.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64};

use tokio::sync::{broadcast, mpsc, oneshot};

use crate::config::AppConfig;
use crate::ipc::{Event, Monit, ProcessSnapshot, Status};
use crate::paths;

/// Command sent to a process supervisor task. The oneshot acks completion so
/// RPC handlers reply only after the action actually happened.
#[derive(Debug)]
pub enum SupervisorCmd {
    Stop(oneshot::Sender<()>),
    Restart(oneshot::Sender<()>),
    Delete(oneshot::Sender<()>),
}

/// One managed process instance (a row in the table).
pub struct Proc {
    pub pm_id: u32,
    pub config: AppConfig,
    /// Instance index (value of `instance_var`).
    pub instance: u32,
    pub status: Status,
    /// OS pid, 0 when not running.
    pub pid: u32,
    /// Total respawns (manual + automatic), pm2's `restart_time`.
    pub restarts: u32,
    pub unstable_restarts: u32,
    /// Epoch ms when counters were last reset (pm2's `created_at`).
    pub created_at: i64,
    /// Epoch ms when the process last went online (pm2's `pm_uptime`).
    pub uptime_ms: Option<i64>,
    /// Last exponential backoff delay used (0 = none yet).
    pub prev_restart_delay: u64,
    pub exit_code: Option<i32>,
    pub monit: Monit,
    pub out_file: PathBuf,
    pub error_file: PathBuf,
    pub pid_file: PathBuf,
    /// Live supervisor command channel; `None` when no supervisor task runs
    /// (stopped/errored procs).
    pub cmd_tx: Option<mpsc::Sender<SupervisorCmd>>,
    /// Cron restart task, aborted when the proc is deleted.
    pub cron_task: Option<tokio::task::JoinHandle<()>>,
}

impl Proc {
    pub fn new(pm_id: u32, instance: u32, config: AppConfig) -> Proc {
        let name = config.effective_name();
        let merge = config.merge_logs;
        let out_file = config
            .out_file
            .clone()
            .unwrap_or_else(|| paths::default_log_path(&name, pm_id, "out", merge));
        let error_file = config
            .error_file
            .clone()
            .unwrap_or_else(|| paths::default_log_path(&name, pm_id, "error", merge));
        let pid_file = config
            .pid_file
            .clone()
            .unwrap_or_else(|| paths::default_pid_path(&name, pm_id));
        Proc {
            pm_id,
            instance,
            status: Status::Stopped,
            pid: 0,
            restarts: 0,
            unstable_restarts: 0,
            created_at: now_ms(),
            uptime_ms: None,
            prev_restart_delay: 0,
            exit_code: None,
            monit: Monit::default(),
            out_file,
            error_file,
            pid_file,
            cmd_tx: None,
            cron_task: None,
            config,
        }
    }

    pub fn name(&self) -> String {
        self.config.effective_name()
    }

    /// Environment handed to the child on top of the daemon's own env.
    pub fn child_env(&self) -> BTreeMap<String, String> {
        let mut env = self.config.env.clone();
        env.insert(self.config.instance_var.clone(), self.instance.to_string());
        env.insert("PMR_ID".into(), self.pm_id.to_string());
        env.insert("PMR_NAME".into(), self.name());
        env
    }

    pub fn snapshot(&self, with_env: bool) -> ProcessSnapshot {
        ProcessSnapshot {
            pm_id: self.pm_id,
            name: self.name(),
            namespace: self.config.namespace.clone(),
            status: self.status,
            pid: self.pid,
            instance: self.instance,
            restarts: self.restarts,
            unstable_restarts: self.unstable_restarts,
            uptime_ms: if self.status == Status::Online {
                self.uptime_ms
            } else {
                None
            },
            monit: self.monit,
            out_file: self.out_file.clone(),
            error_file: self.error_file.clone(),
            pid_file: self.pid_file.clone(),
            exit_code: self.exit_code,
            config: self.config.clone(),
            env: if with_env {
                self.child_env()
            } else {
                BTreeMap::new()
            },
        }
    }

    /// pm2's `resetState` — run on manual restart: forget instability history
    /// but keep the visible restart counter.
    pub fn reset_state(&mut self) {
        self.unstable_restarts = 0;
        self.prev_restart_delay = 0;
        self.created_at = now_ms();
    }

    /// pm2's `resetMetaProcessId` — the `pmr reset` command.
    pub fn reset_counters(&mut self) {
        self.restarts = 0;
        self.unstable_restarts = 0;
        self.prev_restart_delay = 0;
        self.created_at = now_ms();
    }
}

#[derive(Default)]
pub struct ProcessTable {
    pub procs: HashMap<u32, Proc>,
    next_id: u32,
}

impl ProcessTable {
    /// Monotonic id; resets to 0 when the table empties (pm2 behavior).
    pub fn alloc_id(&mut self) -> u32 {
        if self.procs.is_empty() {
            self.next_id = 0;
        }
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// Make sure future ids don't collide with explicitly inserted ones (resurrect).
    pub fn bump_next_id(&mut self, used: u32) {
        if used >= self.next_id {
            self.next_id = used + 1;
        }
    }

    pub fn resolve(&self, target: &crate::ipc::Target) -> Vec<u32> {
        use crate::ipc::Target;
        let mut ids: Vec<u32> = match target {
            Target::All => self.procs.keys().copied().collect(),
            Target::Ids(ids) => ids
                .iter()
                .filter(|i| self.procs.contains_key(i))
                .copied()
                .collect(),
            Target::Names(names) => self
                .procs
                .values()
                .filter(|p| names.iter().any(|n| *n == p.name()))
                .map(|p| p.pm_id)
                .collect(),
        };
        ids.sort_unstable();
        ids
    }
}

/// Context shared by every daemon task.
pub struct Ctx {
    pub table: Mutex<ProcessTable>,
    /// Event bus; subscribers that lag simply drop events.
    pub bus: broadcast::Sender<Event>,
    /// Bumped by SIGUSR2 / reload_logs; log pumps reopen files when stale.
    pub log_generation: AtomicU64,
    pub shutting_down: AtomicBool,
    /// Signal the accept loop to shut the daemon down (RPC `kill`).
    pub shutdown_tx: mpsc::Sender<()>,
    /// File watchers keyed by pm_id (kept out of `Proc` — not serde-friendly).
    pub watchers: Mutex<HashMap<u32, notify::RecommendedWatcher>>,
}

impl Ctx {
    pub fn publish(&self, event: Event) {
        let _ = self.bus.send(event); // no subscribers = fine
    }

    pub fn publish_process_event(&self, pm_id: u32, name: &str, event: &str) {
        self.publish(Event::Process {
            pm_id,
            name: name.to_string(),
            event: event.to_string(),
            at: now_ms(),
        });
    }
}

pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_alloc_resets_when_empty() {
        // Real usage allocates and inserts under one lock, so the table is
        // never empty between two allocations for live procs.
        let mut t = ProcessTable::default();
        let a = t.alloc_id();
        assert_eq!(a, 0);
        t.procs.insert(a, Proc::new(a, 0, AppConfig::new("a.sh")));
        let b = t.alloc_id();
        assert_eq!(b, 1);
        t.procs.insert(b, Proc::new(b, 0, AppConfig::new("b.sh")));
        t.procs.clear();
        assert_eq!(t.alloc_id(), 0, "counter resets once the table empties");
    }
}
