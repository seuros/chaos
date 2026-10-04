//! Trusted test scripts only. This is deliberately not the hook Lua sandbox.

mod native;

use std::io::{self, BufRead, Write};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use mlua::Lua;

fn run() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut interactive = false;
    let mut scripts = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--interactive" => interactive = true,
            "--eval" => scripts.push(args.next().ok_or("--eval requires a script")?),
            _ => return Err(format!("unknown argument: {arg}").into()),
        }
    }
    if scripts.is_empty() && !interactive {
        return Err("usage: chaos-test-process [--interactive] [--eval SCRIPT]".into());
    }

    let lua = Lua::new();
    native::register(&lua)?;
    // Pipe output must be observable immediately, just like a terminal's output.
    lua.load("io.stdout:setvbuf('no'); io.stderr:setvbuf('no')")
        .exec()?;
    let exiting = Arc::new(AtomicBool::new(false));
    let exit_flag = Arc::clone(&exiting);
    lua.globals().set(
        "exit",
        lua.create_function(move |_, ()| {
            exit_flag.store(true, Ordering::Relaxed);
            Ok(())
        })?,
    )?;
    for script in scripts {
        lua.load(&script).exec()?;
    }
    if interactive {
        let stdin = io::stdin();
        let mut input = stdin.lock();
        while !exiting.load(Ordering::Relaxed) {
            io::stdout().write_all(b"> ")?;
            io::stdout().flush()?;
            let mut line = String::new();
            if input.read_line(&mut line)? == 0 {
                break;
            }
            if let Err(err) = lua.load(&line).exec() {
                writeln!(io::stderr(), "{err}")?;
            }
        }
    }
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            let _ = writeln!(io::stderr(), "{err}");
            ExitCode::FAILURE
        }
    }
}
