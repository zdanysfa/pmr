//! End-to-end tests: each test gets its own PMR_HOME (short path under /tmp —
//! unix socket paths are length-limited), runs the real binary, asserts on
//! `jlist` output, and kills its daemon on drop.

use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::{Duration, Instant};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_pmr")
}

struct Home {
    dir: PathBuf,
}

impl Home {
    fn new(tag: &str) -> Home {
        // Short, unique path: /tmp/pmr-t-<tag>-<pid>
        let dir = PathBuf::from(format!("/tmp/pmr-t-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Home { dir }
    }

    fn pmr(&self, args: &[&str]) -> Output {
        Command::new(bin())
            .args(args)
            .env("PMR_HOME", &self.dir)
            .env("PMR_WORKER_INTERVAL", "300")
            .output()
            .expect("pmr binary runs")
    }

    fn pmr_ok(&self, args: &[&str]) -> String {
        let out = self.pmr(args);
        assert!(
            out.status.success(),
            "pmr {args:?} failed:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn jlist(&self) -> serde_json::Value {
        let raw = self.pmr_ok(&["jlist"]);
        serde_json::from_str(&raw).expect("jlist is valid JSON")
    }

    /// Poll `jlist` until the predicate holds or the timeout hits.
    fn wait_for(&self, what: &str, timeout: Duration, pred: impl Fn(&serde_json::Value) -> bool) {
        let deadline = Instant::now() + timeout;
        loop {
            let list = self.jlist();
            if pred(&list) {
                return;
            }
            if Instant::now() >= deadline {
                panic!("timeout waiting for {what}; last jlist: {list:#}");
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn script(&self, name: &str, body: &str) -> String {
        let path = self.dir.join(name);
        std::fs::write(&path, format!("#!/bin/bash\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        path.display().to_string()
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = Command::new(bin())
            .args(["kill"])
            .env("PMR_HOME", &self.dir)
            .output();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn field<'a>(proc: &'a serde_json::Value, key: &str) -> &'a serde_json::Value {
    &proc[key]
}

#[test]
fn cold_start_roundtrip() {
    let h = Home::new("round");
    let script = h.script("sleeper.sh", "sleep 600");

    // Cold start auto-spawns the daemon.
    h.pmr_ok(&["start", &script, "--name", "sleeper"]);
    h.wait_for("sleeper online", Duration::from_secs(5), |l| {
        l[0]["status"] == "online" && l[0]["pid"].as_u64().unwrap() > 0
    });

    // ping reports version.
    let ping = h.pmr_ok(&["ping"]);
    assert!(ping.contains("pong"), "{ping}");

    // stop → stopped, pid 0.
    h.pmr_ok(&["stop", "sleeper"]);
    h.wait_for("sleeper stopped", Duration::from_secs(5), |l| {
        l[0]["status"] == "stopped" && l[0]["pid"] == 0
    });

    // restart brings it back and bumps the counter.
    h.pmr_ok(&["restart", "sleeper"]);
    h.wait_for("sleeper online again", Duration::from_secs(5), |l| {
        l[0]["status"] == "online" && l[0]["restarts"] == 1
    });

    // delete empties the table and removes the log files.
    let out_log = h.dir.join("logs/sleeper-0-out.log");
    assert!(out_log.exists(), "out log should exist while managed");
    h.pmr_ok(&["delete", "all"]);
    h.wait_for("table empty", Duration::from_secs(5), |l| {
        l.as_array().unwrap().is_empty()
    });
    assert!(!out_log.exists(), "delete must remove log files");
}

#[test]
fn crash_loop_ends_errored() {
    let h = Home::new("crash");
    let script = h.script("crasher.sh", "exit 7");

    h.pmr_ok(&["start", &script, "--name", "crasher", "--max-restarts", "3"]);
    h.wait_for("crasher errored", Duration::from_secs(10), |l| {
        l[0]["status"] == "errored"
    });
    let list = h.jlist();
    assert_eq!(field(&list[0], "exit_code"), 7);
    assert!(list[0]["unstable_restarts"].as_u64().unwrap() >= 3);
}

#[test]
fn stop_exit_codes_prevent_restart() {
    let h = Home::new("sec");
    let script = h.script("quitter.sh", "sleep 0.2; exit 42");

    let cfg = h.dir.join("eco.json");
    std::fs::write(
        &cfg,
        format!(r#"{{"apps":[{{"script":"{script}","name":"quitter","stop_exit_codes":[42]}}]}}"#),
    )
    .unwrap();
    h.pmr_ok(&["start", cfg.to_str().unwrap()]);
    h.wait_for(
        "quitter stopped (not restarting)",
        Duration::from_secs(5),
        |l| l[0]["status"] == "stopped",
    );
    let list = h.jlist();
    assert_eq!(
        list[0]["restarts"], 0,
        "stop_exit_codes must suppress restart"
    );
}

#[test]
fn instances_get_distinct_indices() {
    let h = Home::new("inst");
    let script = h.script("sleeper.sh", "sleep 600");

    h.pmr_ok(&["start", &script, "--name", "multi", "-i", "3"]);
    h.wait_for("3 online", Duration::from_secs(5), |l| {
        l.as_array().unwrap().len() == 3
            && l.as_array()
                .unwrap()
                .iter()
                .all(|p| p["status"] == "online")
    });
    let list = h.jlist();
    let mut idx: Vec<u64> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["instance"].as_u64().unwrap())
        .collect();
    idx.sort();
    assert_eq!(idx, vec![0, 1, 2]);

    // scale down
    h.pmr_ok(&["scale", "multi", "1"]);
    h.wait_for("scaled to 1", Duration::from_secs(5), |l| {
        l.as_array().unwrap().len() == 1
    });
}

#[test]
fn logs_written_and_flushed() {
    let h = Home::new("logs");
    let script = h.script("talker.sh", "echo hello-out; echo hello-err >&2; sleep 600");

    h.pmr_ok(&["start", &script, "--name", "talker"]);
    h.wait_for("talker online", Duration::from_secs(5), |l| {
        l[0]["status"] == "online"
    });

    let out_file = h.dir.join("logs/talker-0-out.log");
    let err_file = h.dir.join("logs/talker-0-error.log");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let out = std::fs::read_to_string(&out_file).unwrap_or_default();
        let err = std::fs::read_to_string(&err_file).unwrap_or_default();
        if out.contains("hello-out") && err.contains("hello-err") {
            break;
        }
        assert!(Instant::now() < deadline, "log files never got the lines");
        std::thread::sleep(Duration::from_millis(100));
    }

    // logs --nostream prints the tail.
    let shown = h.pmr_ok(&["logs", "talker", "--nostream", "--lines", "5"]);
    assert!(shown.contains("hello-out"), "{shown}");

    // flush truncates.
    h.pmr_ok(&["flush"]);
    assert_eq!(std::fs::metadata(&out_file).unwrap().len(), 0);
}

#[test]
fn save_kill_resurrect_preserves_table() {
    let h = Home::new("resur");
    let script = h.script("sleeper.sh", "sleep 600");

    h.pmr_ok(&["start", &script, "--name", "keeper", "-i", "2"]);
    h.wait_for("2 online", Duration::from_secs(5), |l| {
        l.as_array().unwrap().len() == 2
            && l.as_array()
                .unwrap()
                .iter()
                .all(|p| p["status"] == "online")
    });
    let before = h.jlist();
    let ids_before: Vec<u64> = before
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["pm_id"].as_u64().unwrap())
        .collect();

    h.pmr_ok(&["save"]);
    h.pmr_ok(&["kill"]);
    std::thread::sleep(Duration::from_millis(300));

    h.pmr_ok(&["resurrect"]);
    h.wait_for("resurrected online", Duration::from_secs(5), |l| {
        l.as_array().unwrap().len() == 2
            && l.as_array()
                .unwrap()
                .iter()
                .all(|p| p["status"] == "online")
    });
    let after = h.jlist();
    let ids_after: Vec<u64> = after
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["pm_id"].as_u64().unwrap())
        .collect();
    assert_eq!(ids_before, ids_after, "pm_ids must survive resurrect");
}

#[test]
fn kill_timeout_escalates_to_sigkill() {
    let h = Home::new("sigkill");
    let script = h.script(
        "stubborn.sh",
        "trap '' INT TERM\nwhile true; do sleep 1; done",
    );

    h.pmr_ok(&[
        "start",
        &script,
        "--name",
        "stubborn",
        "--kill-timeout",
        "500",
    ]);
    h.wait_for("stubborn online", Duration::from_secs(5), |l| {
        l[0]["status"] == "online"
    });

    let t0 = Instant::now();
    h.pmr_ok(&["stop", "stubborn"]);
    let took = t0.elapsed();
    assert!(
        took >= Duration::from_millis(400) && took < Duration::from_secs(3),
        "stop should take ~kill_timeout then SIGKILL, took {took:?}"
    );
    let list = h.jlist();
    assert_eq!(list[0]["status"], "stopped");
}

#[test]
fn stale_socket_recovery() {
    let h = Home::new("stale");
    let script = h.script("sleeper.sh", "sleep 600");

    h.pmr_ok(&["start", &script, "--name", "orphan"]);
    h.wait_for("online", Duration::from_secs(5), |l| {
        l[0]["status"] == "online"
    });
    let child_pid = h.jlist()[0]["pid"].as_i64().unwrap() as i32;

    // Murder the daemon so socket + pid file go stale.
    let daemon_pid: i32 = std::fs::read_to_string(h.dir.join("pmr.pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    unsafe { libc_kill(daemon_pid, 9) };
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        h.dir.join("rpc.sock").exists(),
        "socket should be stale now"
    );

    // Next command must auto-spawn a fresh daemon over the stale files.
    let out = h.pmr_ok(&["ls"]);
    assert!(out.contains("id"), "{out}");
    h.pmr_ok(&["ping"]);

    // The murdered daemon's child is orphaned (same as pm2 after kill -9) —
    // reap it so test runs don't accumulate stray sleepers.
    unsafe { libc_kill(child_pid, 9) };
}

unsafe extern "C" {
    #[link_name = "kill"]
    fn libc_kill(pid: i32, sig: i32) -> i32;
}

#[test]
fn watch_restarts_on_change() {
    let h = Home::new("watch");
    let dir = h.dir.join("app");
    std::fs::create_dir_all(&dir).unwrap();
    let script = dir.join("w.sh");
    std::fs::write(&script, "#!/bin/bash\nsleep 600\n").unwrap();
    std::fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();

    h.pmr_ok(&[
        "start",
        script.to_str().unwrap(),
        "--name",
        "watched",
        "--watch",
    ]);
    h.wait_for("watched online", Duration::from_secs(5), |l| {
        l[0]["status"] == "online"
    });
    std::thread::sleep(Duration::from_millis(500)); // let the watcher settle

    std::fs::write(dir.join("change.txt"), "boop").unwrap();
    h.wait_for("watch restart", Duration::from_secs(10), |l| {
        l[0]["restarts"].as_u64().unwrap() >= 1 && l[0]["status"] == "online"
    });

    // pm2 stopWatch parity: a stopped process must not be revived by a change.
    h.pmr_ok(&["stop", "watched"]);
    h.wait_for("watched stopped", Duration::from_secs(5), |l| {
        l[0]["status"] == "stopped"
    });
    std::fs::write(dir.join("change2.txt"), "boop").unwrap();
    std::thread::sleep(Duration::from_millis(800)); // > debounce; a live watcher would have fired
    let list = h.jlist();
    assert_eq!(
        list[0]["status"], "stopped",
        "stopped watched proc must stay stopped on file change"
    );
}

#[test]
fn yaml_ecosystem_with_env_profile() {
    let h = Home::new("eco");
    let script = h.script("envdump.sh", "echo \"MODE=$MODE\"; sleep 600");
    let cfg = h.dir.join("eco.yaml");
    std::fs::write(
        &cfg,
        format!(
            "apps:\n  - script: {script}\n    name: envy\n    env:\n      MODE: dev\n    env_production:\n      MODE: prod\n"
        ),
    )
    .unwrap();

    h.pmr_ok(&["start", cfg.to_str().unwrap(), "--env", "production"]);
    h.wait_for("envy online", Duration::from_secs(5), |l| {
        l[0]["status"] == "online"
    });

    let deadline = Instant::now() + Duration::from_secs(5);
    let log = h.dir.join("logs/envy-0-out.log");
    loop {
        let content = std::fs::read_to_string(&log).unwrap_or_default();
        if content.contains("MODE=prod") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "env_production not applied; log: {content}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn log_rotation_by_size() {
    let h = Home::new("rotate");
    let script = h.script(
        "chatty.sh",
        "while true; do echo 'a long log line to fill the file quickly 0123456789'; sleep 0.01; done",
    );

    // 2KB limit fills within a second; worker interval is 300ms in tests.
    h.pmr_ok(&["start", &script, "--name", "chatty", "--max-log-size", "2K"]);
    let rotated = h.dir.join("logs/chatty-0-out.log.old");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !rotated.exists() {
        assert!(Instant::now() < deadline, "log never rotated");
        std::thread::sleep(Duration::from_millis(100));
    }
    // The live file keeps receiving lines after rotation.
    let live = h.dir.join("logs/chatty-0-out.log");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let len = std::fs::metadata(&live).map(|m| m.len()).unwrap_or(0);
        if len > 0 {
            break;
        }
        assert!(Instant::now() < deadline, "live log empty after rotation");
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn health_check_restarts_hung_process() {
    let h = Home::new("health");
    let script = h.script("hung.sh", "sleep 600"); // "online" but never healthy

    let cfg = h.dir.join("eco.json");
    std::fs::write(
        &cfg,
        format!(
            r#"{{"apps":[{{"script":"{script}","name":"hung",
                "health_check":{{"command":"exit 1","interval":200,"timeout":1000,"max_fails":2}}}}]}}"#
        ),
    )
    .unwrap();
    h.pmr_ok(&["start", cfg.to_str().unwrap()]);
    h.wait_for("hung online", Duration::from_secs(5), |l| {
        l[0]["status"] == "online"
    });
    // 2 fails × 200ms → restart within a few seconds.
    h.wait_for("health-check restart", Duration::from_secs(10), |l| {
        l[0]["restarts"].as_u64().unwrap() >= 1
    });
}

#[test]
fn no_log_file_still_streams_live() {
    let h = Home::new("nolog");
    let script = h.script("ticker.sh", "while true; do echo beep; sleep 0.1; done");

    h.pmr_ok(&["start", &script, "--name", "quiet", "--no-log-file"]);
    h.wait_for("quiet online", Duration::from_secs(5), |l| {
        l[0]["status"] == "online"
    });
    std::thread::sleep(Duration::from_millis(600));

    // Nothing on disk...
    let out_log = h.dir.join("logs/quiet-0-out.log");
    assert!(
        !out_log.exists() || std::fs::metadata(&out_log).unwrap().len() == 0,
        "no-log-file must not write to disk"
    );

    // ...but the live stream (bus) still delivers lines.
    let mut child = Command::new(bin())
        .args(["logs", "quiet", "--lines", "0", "--raw"])
        .env("PMR_HOME", &h.dir)
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        use std::io::Read;
        let mut acc = String::new();
        let mut buf = [0u8; 256];
        loop {
            match stdout.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    acc.push_str(&String::from_utf8_lossy(&buf[..n]));
                    if acc.contains("beep") {
                        break;
                    }
                }
            }
        }
        let _ = tx.send(acc);
    });
    let got = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("live log stream produced nothing");
    let _ = child.kill();
    let _ = child.wait();
    assert!(got.contains("beep"), "expected live lines, got: {got:?}");
}

#[test]
fn disable_logs_means_no_pipe_at_all() {
    let h = Home::new("dislog");
    let script = h.script("ticker.sh", "while true; do echo noise; sleep 0.1; done");

    h.pmr_ok(&["start", &script, "--name", "silent", "--disable-logs"]);
    h.wait_for("silent online", Duration::from_secs(5), |l| {
        l[0]["status"] == "online"
    });
    let pid = h.jlist()[0]["pid"].as_u64().unwrap();

    // The child's stdout is /dev/null — no pipe to the daemon exists.
    let fd1 = std::fs::read_link(format!("/proc/{pid}/fd/1")).unwrap();
    assert_eq!(fd1.to_str(), Some("/dev/null"), "stdout must be /dev/null");

    std::thread::sleep(Duration::from_millis(500));
    assert!(
        !h.dir.join("logs/silent-0-out.log").exists(),
        "no log file must be created"
    );
}

#[test]
fn cluster_mode_rejected_clearly() {
    let h = Home::new("cluster");
    let cfg = h.dir.join("eco.json");
    std::fs::write(
        &cfg,
        r#"{"apps":[{"script":"a.js","exec_mode":"cluster"}]}"#,
    )
    .unwrap();
    let out = h.pmr(&["start", cfg.to_str().unwrap()]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("fork-only"),
        "want clear fork-only error, got: {err}"
    );
}
