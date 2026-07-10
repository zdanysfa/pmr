//! `pmr logs` — tail the last N lines from files, then stream live from the bus.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::Path;

use anyhow::Result;

use crate::client::Pmr;
use crate::ipc::{Event, ProcessSnapshot, Target};

const COLORS: [&str; 6] = [
    "\x1b[36m", "\x1b[33m", "\x1b[32m", "\x1b[35m", "\x1b[34m", "\x1b[31m",
];
const RESET: &str = "\x1b[0m";

pub fn run(
    target: Option<String>,
    lines: usize,
    only_err: bool,
    only_out: bool,
    nostream: bool,
    timestamp: bool,
    raw: bool,
) -> Result<()> {
    let mut pmr = Pmr::connect()?;
    let procs = match &target {
        Some(t) => pmr.describe(t.as_str())?,
        None => pmr.list()?,
    };
    if procs.is_empty() {
        println!("[pmr] no processes");
        return Ok(());
    }

    // Tail files first, like pm2.
    for p in &procs {
        if !only_err {
            print_tail(p, &p.out_file, "out", lines, raw);
        }
        if !only_out {
            print_tail(p, &p.error_file, "err", lines, raw);
        }
    }

    if nostream {
        return Ok(());
    }

    let topics: Vec<&str> = if only_err {
        vec!["log:err"]
    } else if only_out {
        vec!["log:out"]
    } else {
        vec!["log:out", "log:err"]
    };
    let sub_target = target.as_deref().map(Target::parse);
    let stream = pmr.subscribe(&topics, sub_target)?;

    println!();
    for event in stream {
        match event {
            Ok(Event::Log {
                pm_id,
                name,
                stream,
                line,
                at,
            }) => {
                let prefix = if raw {
                    String::new()
                } else {
                    let color = COLORS[pm_id as usize % COLORS.len()];
                    format!("{color}{pm_id}|{name}{RESET} | ")
                };
                let ts = if timestamp {
                    let dt = chrono::DateTime::from_timestamp_millis(at)
                        .unwrap_or_default()
                        .with_timezone(&chrono::Local);
                    format!("{} ", dt.format("%Y-%m-%dT%H:%M:%S"))
                } else {
                    String::new()
                };
                let line = if stream == "err" && !raw {
                    format!("\x1b[31m{line}{RESET}")
                } else {
                    line
                };
                println!("{prefix}{ts}{line}");
            }
            Ok(_) => {}
            Err(e) => {
                eprintln!("[pmr] log stream ended: {e}");
                break;
            }
        }
    }
    Ok(())
}

fn print_tail(p: &ProcessSnapshot, file: &Path, kind: &str, lines: usize, raw: bool) {
    if lines == 0 {
        return;
    }
    let Ok(tail) = tail_lines(file, lines) else {
        return;
    };
    if tail.is_empty() {
        return;
    }
    if !raw {
        println!(
            "\x1b[90m{} last {} lines of {} ({kind}):\x1b[0m",
            p.name,
            tail.len(),
            file.display()
        );
    }
    let color = COLORS[p.pm_id as usize % COLORS.len()];
    for line in tail {
        if raw {
            println!("{line}");
        } else {
            println!("{color}{}|{}{RESET} | {line}", p.pm_id, p.name);
        }
    }
}

/// Read the last `n` lines of a file without loading it whole:
/// seek to a window near the end sized by an estimated line length.
pub fn tail_lines(path: &Path, n: usize) -> std::io::Result<Vec<String>> {
    let mut f = std::fs::File::open(path)?;
    let size = f.metadata()?.len();
    // ponytail: estimate 200 bytes/line like pm2; double window until enough lines or BOF
    let mut window = (n as u64) * 200;
    loop {
        let start = size.saturating_sub(window);
        f.seek(SeekFrom::Start(start))?;
        let reader = BufReader::new(&mut f);
        let mut buf: VecDeque<String> = VecDeque::with_capacity(n + 1);
        // Bytes + lossy UTF-8: one binary byte in a log file must not blank
        // the whole tail (`lines()` errors on invalid UTF-8).
        for chunk in reader.split(b'\n') {
            let mut chunk = chunk?;
            if chunk.last() == Some(&b'\r') {
                chunk.pop();
            }
            if buf.len() == n + 1 {
                buf.pop_front();
            }
            buf.push_back(String::from_utf8_lossy(&chunk).into_owned());
        }
        // First line of a mid-file window is probably partial — drop it.
        let complete = start == 0;
        if buf.len() > n || complete {
            if !complete && buf.len() > n {
                buf.pop_front();
            }
            while buf.len() > n {
                buf.pop_front();
            }
            return Ok(buf.into());
        }
        if start == 0 {
            return Ok(buf.into());
        }
        window *= 2;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn tail_reads_last_lines() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        for i in 0..100 {
            writeln!(f, "line {i}").unwrap();
        }
        let lines = tail_lines(f.path(), 3).unwrap();
        assert_eq!(lines, vec!["line 97", "line 98", "line 99"]);
        let all = tail_lines(f.path(), 500).unwrap();
        assert_eq!(all.len(), 100);

        // Binary garbage must not blank the tail (lossy, not an error).
        f.write_all(b"\xff\xfe binary\nlast line\n").unwrap();
        let lines = tail_lines(f.path(), 2).unwrap();
        assert_eq!(lines[1], "last line");
        assert!(lines[0].contains("binary"));
    }
}
