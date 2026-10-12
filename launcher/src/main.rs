//! `tern-yazi-launch`: supervises Yazi for Tern. Inside a Tern shell pane it draws the browser as
//! a program surface in that pane; for a standalone native block it feeds the plugin's browser.
//! The same binary also serves the helper modes Yazi reaches through its PATH shim.

mod backend;
mod inbox;
mod native;
mod standalone;
mod supervisor;
mod surface;
mod sys;

use std::env;
use std::ffi::OsStr;
use std::io;
use std::path::Path;
use std::process;

use supervisor::{launch_args, LaunchArgs, Plan};
use sys::invalid;

fn run() -> io::Result<i32> {
    let first = env::args_os().next();
    if first.as_deref().and_then(|v| Path::new(v).file_name()) == Some(OsStr::new("sh")) {
        return native::native_shell(&env::args_os().skip(1).collect::<Vec<_>>());
    }
    match env::args_os().nth(1).as_deref() {
        Some(mode) if mode == "--rename-no-replace" => {
            return native::rename_no_replace(&env::args_os().skip(2).collect::<Vec<_>>());
        }
        Some(mode) if mode == "--native-shell" => {
            return native::native_shell(&env::args_os().skip(2).collect::<Vec<_>>());
        }
        Some(mode) if mode == "--native-terminal" => {
            let path = env::args_os()
                .nth(2)
                .ok_or_else(|| invalid("missing native shell request path"))?;
            if env::args_os().nth(3).is_some() {
                return Err(invalid("unexpected native terminal arguments"));
            }
            return native::native_terminal(Path::new(&path));
        }
        _ => {}
    }
    let LaunchArgs {
        real,
        args,
        standalone_pane,
    } = launch_args(env::args_os().skip(1))?;
    if let Some(pane) = standalone_pane {
        return match supervisor::plan(&real, &args, true)? {
            Plan::Supervise(launch) => standalone::run(&launch, pane),
            Plan::Passthrough => supervisor::passthrough(&real, &args),
        };
    }
    let in_tern = env::var_os("TERN_PANE").is_some_and(|v| !v.is_empty());
    if !in_tern {
        return supervisor::passthrough(&real, &args);
    }
    match supervisor::plan(&real, &args, false)? {
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
