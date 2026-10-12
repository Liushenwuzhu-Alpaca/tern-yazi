//! `tern-yazi-launch`: supervises Yazi for Tern. Inside a Tern pane it draws the browser as a
//! program surface in that pane; elsewhere it hands the arguments to the original Yazi. The
//! same binary also serves the no-replace rename the Yazi plugin needs.

mod backend;
mod helper;
mod inbox;
mod supervisor;
mod surface;
mod sys;

use std::env;
use std::ffi::OsStr;
use std::io;
use std::process;

use supervisor::{launch_args, LaunchArgs, Plan};

fn run() -> io::Result<i32> {
    if env::args_os().nth(1).as_deref() == Some(OsStr::new("--rename-no-replace")) {
        return helper::rename_no_replace(&env::args_os().skip(2).collect::<Vec<_>>());
    }
    let LaunchArgs { real, args } = launch_args(env::args_os().skip(1))?;
    let in_tern = env::var_os("TERN_PANE").is_some_and(|v| !v.is_empty());
    if !in_tern {
        return supervisor::passthrough(&real, &args);
    }
    match supervisor::plan(&real, &args)? {
        Plan::Supervise(launch) => surface::run(&launch, &real, &args),
        Plan::Passthrough => supervisor::passthrough(&real, &args),
    }
}

fn main() {
    process::exit(match run() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("tern-yazi: {error}");
            1
        }
    });
}
