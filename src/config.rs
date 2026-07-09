//! App configuration: the `AppConfig` struct (serde + builder), ecosystem file
//! loading (JSON/YAML/TOML), `env_<profile>` merging and interpreter detection.
//!
//! Defaults mirror pm2's `lib/API/schema.json` where they exist.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

fn default_true() -> bool {
    true
}
fn default_instances() -> u32 {
    1
}
fn default_max_restarts() -> u32 {
    16
}
fn default_min_uptime() -> u64 {
    1000
}
fn default_kill_timeout() -> u64 {
    1600
}
fn default_kill_retry_time() -> u64 {
    100
}
fn default_kill_signal() -> String {
    "SIGINT".into()
}
fn default_namespace() -> String {
    "default".into()
}
fn default_instance_var() -> String {
    "NODE_APP_INSTANCE".into()
}

/// Configuration for one app. Field names accept pm2 spellings via serde aliases.
///
/// Unknown fields land in `env_profiles` (via flatten) and are rejected by
/// [`AppConfig::validate`] unless they start with `env_` — this is how pm2-only
/// options like `exec_mode: cluster` get a clear error instead of silence.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AppConfig {
    /// Path to the script or binary to run.
    #[serde(alias = "exec")]
    pub script: String,
    /// Process name. Defaults to the script file name without extension.
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default = "default_namespace")]
    pub namespace: String,
    /// Working directory. Defaults to the daemon's idea of the script's parent dir.
    #[serde(default)]
    pub cwd: Option<PathBuf>,
    /// Arguments passed to the script.
    #[serde(default)]
    pub args: Vec<String>,
    /// Interpreter (e.g. `node`, `python3`). `None` = auto-detect from extension,
    /// `Some("none")` = execute the script directly.
    #[serde(default, alias = "exec_interpreter")]
    pub interpreter: Option<String>,
    #[serde(default, alias = "node_args", alias = "interpreterArgs")]
    pub interpreter_args: Vec<String>,
    /// Number of instances (fork mode). Each gets `NODE_APP_INSTANCE=<i>`.
    #[serde(default = "default_instances")]
    pub instances: u32,
    /// Env var name carrying the instance index.
    #[serde(default = "default_instance_var")]
    pub instance_var: String,
    /// Base environment variables.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Per-profile env overlays: `env_production`, `env_dev`, ... applied with `--env <profile>`.
    #[serde(flatten)]
    pub env_profiles: BTreeMap<String, serde_json::Value>,

    // --- logs ---
    #[serde(default, alias = "out", alias = "output", alias = "out_log")]
    pub out_file: Option<PathBuf>,
    #[serde(
        default,
        alias = "error",
        alias = "err",
        alias = "err_file",
        alias = "err_log"
    )]
    pub error_file: Option<PathBuf>,
    /// Prefix each log line with a timestamp in this chrono format.
    /// `pm2 --time` equivalent: "YYYY-MM-DDTHH:mm:ss" → `%Y-%m-%dT%H:%M:%S`.
    #[serde(default)]
    pub log_date_format: Option<String>,
    /// Share one log file across instances (no `-<id>` suffix).
    #[serde(default, alias = "combine_logs")]
    pub merge_logs: bool,
    /// Keep logs OFF disk entirely: `pmr logs` still streams live (in-memory
    /// bus), but nothing is written to log files. Zero disk I/O on the log
    /// path. CLI: `--no-log-file`.
    #[serde(default)]
    pub disable_log_files: bool,
    #[serde(default, alias = "pid")]
    pub pid_file: Option<PathBuf>,

    // --- lifecycle ---
    #[serde(default = "default_true")]
    pub autostart: bool,
    #[serde(default = "default_true")]
    pub autorestart: bool,
    #[serde(default = "default_max_restarts")]
    pub max_restarts: u32,
    /// Milliseconds an app must stay up to be considered stable.
    #[serde(default = "default_min_uptime")]
    pub min_uptime: u64,
    /// Fixed restart delay in ms.
    #[serde(default)]
    pub restart_delay: u64,
    /// Exponential backoff base delay in ms (0 = disabled). Grows *1.5, capped at 15000.
    #[serde(default)]
    pub exp_backoff_restart_delay: u64,
    /// Cron expression triggering a scheduled restart.
    #[serde(default, alias = "cron")]
    pub cron_restart: Option<String>,
    /// Restart when process memory exceeds this (bytes; parse "50M" etc. at CLI level).
    #[serde(default)]
    pub max_memory_restart: Option<u64>,
    /// Exit codes that mean "stop, don't restart".
    #[serde(default)]
    pub stop_exit_codes: Vec<i32>,
    #[serde(default = "default_kill_timeout")]
    pub kill_timeout: u64,
    #[serde(default = "default_kill_retry_time")]
    pub kill_retry_time: u64,
    #[serde(default = "default_kill_signal")]
    pub kill_signal: String,
    /// Kill the whole process tree (bottom-up) instead of just the main pid.
    #[serde(default = "default_true")]
    pub treekill: bool,

    /// Rotate a log file once it exceeds this many bytes (checked every worker
    /// tick): current file renamed to `<file>.old`, fresh file opened.
    /// pm2 needs the pm2-logrotate module for this; pmr does it natively.
    #[serde(default)]
    pub max_log_size: Option<u64>,
    /// Periodic health check; too many consecutive failures restart the
    /// process. Catches "online but hung". Not available in pm2.
    #[serde(default)]
    pub health_check: Option<HealthCheck>,

    // --- watch ---
    #[serde(default)]
    pub watch: bool,
    #[serde(default)]
    pub ignore_watch: Vec<String>,
    /// Debounce in ms before a watch-triggered restart.
    #[serde(default)]
    pub watch_delay: Option<u64>,

    // --- user ---
    #[serde(default, alias = "user")]
    pub uid: Option<String>,
    #[serde(default)]
    pub gid: Option<String>,
}

fn default_hc_interval() -> u64 {
    30_000
}
fn default_hc_timeout() -> u64 {
    5_000
}
fn default_hc_max_fails() -> u32 {
    3
}

/// Command-based health check. The command runs via `sh -c` (so
/// `curl -fsS localhost:3000/health` works); exit code 0 = healthy.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HealthCheck {
    pub command: String,
    /// ms between checks.
    #[serde(default = "default_hc_interval")]
    pub interval: u64,
    /// ms before a hanging check counts as failed.
    #[serde(default = "default_hc_timeout")]
    pub timeout: u64,
    /// Consecutive failures before the process is restarted.
    #[serde(default = "default_hc_max_fails")]
    pub max_fails: u32,
}

impl AppConfig {
    /// Builder entry point for programmatic use:
    /// `AppConfig::new("bot.js").name("bot").instances(2)`.
    pub fn new(script: impl Into<String>) -> Self {
        // Route through serde so every default lives in one place.
        let mut cfg: AppConfig =
            serde_json::from_value(serde_json::json!({ "script": script.into() }))
                .expect("minimal config is valid");
        cfg.env_profiles.clear();
        cfg
    }

    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    pub fn cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args = args.into_iter().map(Into::into).collect();
        self
    }

    pub fn instances(mut self, n: u32) -> Self {
        self.instances = n;
        self
    }

    pub fn interpreter(mut self, interp: impl Into<String>) -> Self {
        self.interpreter = Some(interp.into());
        self
    }

    pub fn env(mut self, key: impl Into<String>, val: impl Into<String>) -> Self {
        self.env.insert(key.into(), val.into());
        self
    }

    pub fn autorestart(mut self, yes: bool) -> Self {
        self.autorestart = yes;
        self
    }

    pub fn max_restarts(mut self, n: u32) -> Self {
        self.max_restarts = n;
        self
    }

    pub fn max_memory_restart(mut self, bytes: u64) -> Self {
        self.max_memory_restart = Some(bytes);
        self
    }

    pub fn watch(mut self, yes: bool) -> Self {
        self.watch = yes;
        self
    }

    pub fn max_log_size(mut self, bytes: u64) -> Self {
        self.max_log_size = Some(bytes);
        self
    }

    /// Live-only logs: stream over `pmr logs`, never write to disk.
    pub fn disable_log_files(mut self, yes: bool) -> Self {
        self.disable_log_files = yes;
        self
    }

    /// Health check with defaults (30s interval, 5s timeout, 3 fails → restart).
    pub fn health_check(mut self, command: impl Into<String>) -> Self {
        self.health_check = Some(HealthCheck {
            command: command.into(),
            interval: default_hc_interval(),
            timeout: default_hc_timeout(),
            max_fails: default_hc_max_fails(),
        });
        self
    }

    /// Effective process name.
    pub fn effective_name(&self) -> String {
        match &self.name {
            Some(n) => n.clone(),
            None => Path::new(&self.script)
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| self.script.clone()),
        }
    }

    /// Resolve the interpreter: explicit > extension-based auto-detect > direct exec.
    pub fn effective_interpreter(&self) -> Option<String> {
        match self.interpreter.as_deref() {
            Some("none") | Some("") => None,
            Some(i) => Some(i.to_string()),
            None => detect_interpreter(&self.script),
        }
    }

    /// Apply `env_<profile>` overlay onto `env`. Errors when the profile is missing.
    pub fn apply_env_profile(&mut self, profile: &str) -> Result<()> {
        let key = format!("env_{profile}");
        let Some(overlay) = self.env_profiles.get(&key) else {
            bail!(
                "app '{}' has no '{}' section in its config",
                self.effective_name(),
                key
            );
        };
        let map: BTreeMap<String, serde_json::Value> = serde_json::from_value(overlay.clone())
            .with_context(|| format!("'{key}' is not an object of key/value pairs"))?;
        for (k, v) in map {
            let s = match v {
                serde_json::Value::String(s) => s,
                other => other.to_string(),
            };
            self.env.insert(k, s);
        }
        Ok(())
    }

    /// Validate constraints that serde can't express.
    pub fn validate(&self) -> Result<()> {
        if self.script.is_empty() {
            bail!("script must not be empty");
        }
        if self.instances == 0 {
            bail!("instances must be >= 1 (got 0)");
        }
        for key in self.env_profiles.keys() {
            if key == "exec_mode" {
                bail!(
                    "exec_mode is not supported: pmr is fork-only. \
                     Use `instances: N` — each instance gets {}=<i>",
                    self.instance_var
                );
            }
            if !key.starts_with("env_") {
                bail!("unknown config field '{key}'");
            }
        }
        if let Some(expr) = &self.cron_restart {
            croner::Cron::new(expr)
                .with_seconds_optional()
                .parse()
                .map_err(|e| anyhow::anyhow!("invalid cron_restart '{expr}': {e}"))?;
        }
        if let Some(hc) = &self.health_check {
            if hc.command.trim().is_empty() {
                bail!("health_check.command must not be empty");
            }
            if hc.max_fails == 0 || hc.interval == 0 {
                bail!("health_check interval and max_fails must be >= 1");
            }
        }
        Ok(())
    }
}

/// Interpreter auto-detection by extension, mirroring pm2's interpreter.json.
pub fn detect_interpreter(script: &str) -> Option<String> {
    let ext = Path::new(script).extension()?.to_str()?;
    let interp = match ext {
        "js" | "cjs" | "mjs" => "node",
        "ts" | "tsx" => "node", // assumes ts runtime configured via interpreter_args; explicit interpreter wins
        "py" => "python3",
        "sh" => "bash",
        "rb" => "ruby",
        "pl" => "perl",
        "php" => "php",
        _ => return None,
    };
    Some(interp.into())
}

/// Top-level ecosystem file: `{ "apps": [ ... ] }` (bare array also accepted for JSON/YAML).
#[derive(Debug, Deserialize)]
struct Ecosystem {
    apps: Vec<AppConfig>,
}

/// Load an ecosystem config file. Format chosen by extension.
pub fn load_ecosystem(path: &Path) -> Result<Vec<AppConfig>> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("cannot read config file {}", path.display()))?;
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    let apps = match ext.as_str() {
        "json" => parse_json(&raw)?,
        "yaml" | "yml" => parse_yaml(&raw)?,
        "toml" => {
            let eco: Ecosystem = toml::from_str(&raw).context("invalid TOML config")?;
            eco.apps
        }
        "js" | "cjs" | "mjs" => bail!(
            "JS config files are not supported by pmr (cannot evaluate JavaScript). \
             Convert {} to JSON/YAML/TOML, e.g. `pmr init` for a sample.",
            path.display()
        ),
        other => bail!("unsupported config extension '.{other}' (use .json/.yaml/.toml)"),
    };

    if apps.is_empty() {
        bail!("config file {} declares no apps", path.display());
    }
    for app in &apps {
        app.validate()
            .with_context(|| format!("invalid config for app '{}'", app.effective_name()))?;
    }
    Ok(apps)
}

fn parse_json(raw: &str) -> Result<Vec<AppConfig>> {
    // Accept {"apps":[...]} or a bare [...]
    let val: serde_json::Value = serde_json::from_str(raw).context("invalid JSON config")?;
    let apps_val = match &val {
        serde_json::Value::Array(_) => val.clone(),
        serde_json::Value::Object(o) => o
            .get("apps")
            .cloned()
            .context("JSON config must be an array or contain an \"apps\" array")?,
        _ => bail!("JSON config must be an array or an object with \"apps\""),
    };
    serde_json::from_value(apps_val).context("invalid app declaration")
}

fn parse_yaml(raw: &str) -> Result<Vec<AppConfig>> {
    let val: serde_yaml::Value = serde_yaml::from_str(raw).context("invalid YAML config")?;
    let json_val: serde_json::Value =
        serde_json::to_value(&val).context("YAML → JSON conversion failed")?;
    match &json_val {
        serde_json::Value::Array(_) => {
            Ok(serde_json::from_value(json_val).context("invalid app declaration")?)
        }
        serde_json::Value::Object(o) if o.contains_key("apps") => {
            Ok(serde_json::from_value(o["apps"].clone()).context("invalid app declaration")?)
        }
        _ => bail!("YAML config must be a list of apps or a map with an \"apps\" key"),
    }
}

/// Parse human memory sizes: "50M", "1G", "150K", plain bytes.
pub fn parse_memory(s: &str) -> Result<u64> {
    let s = s.trim();
    let (num, mult) = match s.chars().last() {
        Some('G') | Some('g') => (&s[..s.len() - 1], 1024 * 1024 * 1024),
        Some('M') | Some('m') => (&s[..s.len() - 1], 1024 * 1024),
        Some('K') | Some('k') => (&s[..s.len() - 1], 1024),
        _ => (s, 1),
    };
    let n: u64 = num
        .parse()
        .with_context(|| format!("invalid memory size '{s}'"))?;
    Ok(n * mult)
}

/// Sample ecosystem file written by `pmr init`.
pub const SAMPLE_ECOSYSTEM: &str = r#"# pmr ecosystem file — start with: pmr start ecosystem.yaml
apps:
  - script: ./app.js
    name: app
    instances: 1
    # interpreter: node          # auto-detected from extension
    # args: ["--port", "3000"]
    # cwd: /srv/app
    env:
      NODE_ENV: development
    env_production:
      NODE_ENV: production
    # autorestart: true
    # max_restarts: 16
    # min_uptime: 1000           # ms
    # restart_delay: 0           # ms
    # exp_backoff_restart_delay: 100
    # max_memory_restart: 300M   # use string form in CLI; bytes in file
    # cron_restart: "0 3 * * *"
    # watch: false
    # ignore_watch: ["node_modules", ".git"]
    # kill_timeout: 1600         # ms before SIGKILL
    # stop_exit_codes: [0]
    # max_log_size: 10485760     # rotate log at 10MB (bytes; CLI accepts 10M)
    # health_check:              # restart when online-but-hung (not in pm2)
    #   command: "curl -fsS http://localhost:3000/health"
    #   interval: 30000          # ms
    #   timeout: 5000            # ms
    #   max_fails: 3
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimal_defaults() {
        let cfg = AppConfig::new("bot.js");
        assert_eq!(cfg.effective_name(), "bot");
        assert_eq!(cfg.instances, 1);
        assert_eq!(cfg.max_restarts, 16);
        assert_eq!(cfg.min_uptime, 1000);
        assert_eq!(cfg.kill_timeout, 1600);
        assert_eq!(cfg.kill_signal, "SIGINT");
        assert!(cfg.autorestart);
        assert!(cfg.treekill);
        assert_eq!(cfg.effective_interpreter().as_deref(), Some("node"));
    }

    #[test]
    fn interpreter_detect() {
        assert_eq!(detect_interpreter("a.py").as_deref(), Some("python3"));
        assert_eq!(detect_interpreter("a.sh").as_deref(), Some("bash"));
        assert_eq!(detect_interpreter("a.rb").as_deref(), Some("ruby"));
        assert_eq!(detect_interpreter("mybinary"), None);
        let direct = AppConfig::new("script.py").interpreter("none");
        assert_eq!(direct.effective_interpreter(), None);
    }

    #[test]
    fn env_profile_merge() {
        let raw = r#"{
            "apps": [{
                "script": "web.js",
                "env": {"NODE_ENV": "dev", "KEEP": "1"},
                "env_production": {"NODE_ENV": "production", "EXTRA": "2"}
            }]
        }"#;
        let mut apps = parse_json(raw).unwrap();
        let app = &mut apps[0];
        app.apply_env_profile("production").unwrap();
        assert_eq!(app.env["NODE_ENV"], "production");
        assert_eq!(app.env["KEEP"], "1");
        assert_eq!(app.env["EXTRA"], "2");
        assert!(app.apply_env_profile("staging").is_err());
    }

    #[test]
    fn yaml_and_aliases() {
        let raw = "apps:\n  - exec: worker.py\n    interpreter: python3\n    combine_logs: true\n";
        let apps = parse_yaml(raw).unwrap();
        assert_eq!(apps[0].script, "worker.py");
        assert!(apps[0].merge_logs);
    }

    #[test]
    fn toml_config() {
        let raw = "[[apps]]\nscript = \"a.sh\"\nname = \"a\"\ninstances = 2\n";
        let eco: Ecosystem = toml::from_str(raw).unwrap();
        assert_eq!(eco.apps[0].instances, 2);
    }

    #[test]
    fn memory_parse() {
        assert_eq!(parse_memory("50M").unwrap(), 50 * 1024 * 1024);
        assert_eq!(parse_memory("1G").unwrap(), 1024 * 1024 * 1024);
        assert_eq!(parse_memory("123").unwrap(), 123);
        assert!(parse_memory("abc").is_err());
    }

    #[test]
    fn reject_unknown_fields() {
        let raw = r#"[{"script": "a.js", "exec_mode": "cluster"}]"#;
        let apps = parse_json(raw).unwrap();
        let err = apps[0].validate().unwrap_err().to_string();
        assert!(
            err.contains("exec_mode"),
            "cluster mode config must be rejected: {err}"
        );
    }

    #[test]
    fn sample_is_valid_yaml() {
        let apps = parse_yaml(SAMPLE_ECOSYSTEM).unwrap();
        assert_eq!(apps[0].effective_name(), "app");
    }
}
