//! Wire protocol: newline-delimited JSON over the unix socket, plus the shared
//! `ProcessSnapshot` type used by `ls`/`jlist`/`describe`/`monit`/dump.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::config::AppConfig;

/// Client → daemon request.
#[derive(Debug, Serialize, Deserialize)]
pub struct Request {
    pub id: u64,
    #[serde(flatten)]
    pub call: Method,
}

/// All RPC methods with their parameters.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "method", content = "params", rename_all = "snake_case")]
pub enum Method {
    Ping,
    Version,
    /// Start apps. The daemon expands `instances: N` into N table entries.
    Start {
        apps: Vec<AppConfig>,
    },
    Stop {
        target: Target,
    },
    Restart {
        target: Target,
    },
    Delete {
        target: Target,
    },
    Reset {
        target: Target,
    },
    List,
    Describe {
        target: Target,
    },
    Scale {
        name: String,
        instances: u32,
    },
    SendSignal {
        target: Target,
        signal: String,
    },
    Flush {
        target: Option<Target>,
    },
    ReloadLogs,
    Save,
    Resurrect,
    /// Kill the daemon (stops all children first, writes dump).
    Kill,
    /// Switch this connection to event streaming.
    Subscribe {
        topics: Vec<String>,
        target: Option<Target>,
    },
}

/// Which processes an operation applies to.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Target {
    All,
    Ids(Vec<u32>),
    Names(Vec<String>),
}

impl Target {
    /// Parse a CLI token: "all" | numeric id | name.
    pub fn parse(token: &str) -> Target {
        if token == "all" {
            Target::All
        } else if let Ok(id) = token.parse::<u32>() {
            Target::Ids(vec![id])
        } else {
            Target::Names(vec![token.to_string()])
        }
    }
}

/// Daemon → client response.
#[derive(Debug, Serialize, Deserialize)]
pub struct Response {
    pub id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Daemon → client event frame (only on subscribed connections).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventFrame {
    pub event: String,
    pub data: serde_json::Value,
}

/// Events published on the daemon bus.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Event {
    /// A log line from a process.
    Log {
        pm_id: u32,
        name: String,
        /// "out" | "err"
        stream: String,
        line: String,
        at: i64,
    },
    /// Lifecycle event: start/online/stop/exit/restart/delete/errored...
    Process {
        pm_id: u32,
        name: String,
        event: String,
        at: i64,
    },
    /// Daemon shutting down.
    DaemonKill,
}

impl Event {
    /// Topic string used for subscription filtering.
    pub fn topic(&self) -> String {
        match self {
            Event::Log { stream, .. } => format!("log:{stream}"),
            Event::Process { .. } => "process:event".into(),
            Event::DaemonKill => "pmr:kill".into(),
        }
    }

    pub fn pm_id(&self) -> Option<u32> {
        match self {
            Event::Log { pm_id, .. } | Event::Process { pm_id, .. } => Some(*pm_id),
            Event::DaemonKill => None,
        }
    }

    pub fn proc_name(&self) -> Option<&str> {
        match self {
            Event::Log { name, .. } | Event::Process { name, .. } => Some(name),
            Event::DaemonKill => None,
        }
    }
}

/// Process status, mirroring pm2's state machine.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Launching,
    Online,
    Stopping,
    Stopped,
    Errored,
    WaitingRestart,
}

impl std::fmt::Display for Status {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Status::Launching => "launching",
            Status::Online => "online",
            Status::Stopping => "stopping",
            Status::Stopped => "stopped",
            Status::Errored => "errored",
            Status::WaitingRestart => "waiting restart",
        };
        f.write_str(s)
    }
}

/// CPU/memory sample for one process.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Monit {
    /// Percent, 0-100 per core basis (like pm2).
    pub cpu: f32,
    /// Bytes.
    pub memory: u64,
}

/// Full public view of one managed process. Returned by list/describe,
/// stored (with config) in the dump file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessSnapshot {
    pub pm_id: u32,
    pub name: String,
    pub namespace: String,
    pub status: Status,
    /// OS pid; 0 when not running.
    pub pid: u32,
    /// Instance index (NODE_APP_INSTANCE value).
    pub instance: u32,
    pub restarts: u32,
    pub unstable_restarts: u32,
    /// Epoch ms when the process last went online; None when not running.
    pub uptime_ms: Option<i64>,
    pub monit: Monit,
    pub out_file: PathBuf,
    pub error_file: PathBuf,
    pub pid_file: PathBuf,
    pub exit_code: Option<i32>,
    pub config: AppConfig,
    /// Effective environment handed to the child (config env; full env in describe).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
}

/// Result payload for `ping`.
#[derive(Debug, Serialize, Deserialize)]
pub struct PingReply {
    pub pong: bool,
    pub version: String,
    pub pid: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_wire_shape() {
        let req = Request {
            id: 7,
            call: Method::Stop {
                target: Target::Names(vec!["web".into()]),
            },
        };
        let s = serde_json::to_string(&req).unwrap();
        assert!(s.contains("\"id\":7"), "{s}");
        assert!(s.contains("\"method\":\"stop\""), "{s}");
        let back: Request = serde_json::from_str(&s).unwrap();
        assert_eq!(back.id, 7);
    }

    #[test]
    fn target_parse() {
        assert_eq!(Target::parse("all"), Target::All);
        assert_eq!(Target::parse("3"), Target::Ids(vec![3]));
        assert_eq!(Target::parse("bot"), Target::Names(vec!["bot".into()]));
    }

    #[test]
    fn event_topics() {
        let e = Event::Log {
            pm_id: 0,
            name: "a".into(),
            stream: "out".into(),
            line: "hi".into(),
            at: 0,
        };
        assert_eq!(e.topic(), "log:out");
    }
}
