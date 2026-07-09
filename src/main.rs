use clap::Parser;
use pmr::cli::{Cli, Cmd};

fn main() {
    // Die quietly on closed pipes (`pmr logs | head`) instead of panicking.
    unsafe {
        let _ = nix::sys::signal::signal(
            nix::sys::signal::Signal::SIGPIPE,
            nix::sys::signal::SigHandler::SigDfl,
        );
    }
    let cli = Cli::parse();
    let result = match cli.command {
        Cmd::Daemon => pmr::daemon::run(),
        cmd => pmr::cli::commands::dispatch(cmd),
    };
    if let Err(e) = result {
        eprintln!("[pmr] error: {e:#}");
        std::process::exit(1);
    }
}
