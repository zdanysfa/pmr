//! # pmr — process manager (pm2 rewritten in Rust, fork mode)
//!
//! Use as a CLI (`pmr start app.js`) or as a library:
//!
//! ```no_run
//! use pmr::{AppConfig, Pmr};
//!
//! # fn main() -> anyhow::Result<()> {
//! let mut pmr = Pmr::connect()?; // auto-spawns the daemon when needed
//! pmr.start(AppConfig::new("bot.js").name("bot").instances(2))?;
//! for p in pmr.list()? {
//!     println!("{} {} {}", p.pm_id, p.name, p.status);
//! }
//! pmr.stop("bot")?;
//! # Ok(())
//! # }
//! ```
//!
//! The client is synchronous (plain unix-socket I/O); the daemon runs on tokio.
//! In async code, wrap calls in `spawn_blocking`.

pub mod client;
pub mod config;
pub mod ipc;
pub mod paths;

#[doc(hidden)]
pub mod cli;
#[doc(hidden)]
pub mod daemon;
#[doc(hidden)]
pub mod monit;

pub use client::{EventStream, Pmr};
pub use config::AppConfig;
pub use ipc::{Event, Monit, ProcessSnapshot, Status, Target};

/// Crate version — also the daemon protocol version carried in `ping`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
