//! Thin binary: parse args, build the real facade, run.
//!
//! Usage: `lupin-tui [--server <path>] [--root <dir>] [--fake]
//! [--line] [--triggers <path>]`
//!
//! - `--server` names the `lupin` server binary to spawn (default:
//!   `lupin` on PATH, or `$LUPIN_SERVER`).
//! - `--root` is passed through to the server as its data root.
//! - This TUI's lane scope is fixed to `sfw` (the qompass lane of
//!   this build); there is no flag to widen it.
//! - `--fake` runs against the in-memory fake facade (smoke/demo).
//! - `--line` forces line mode; without a TTY, line mode is used
//!   automatically. Both modes share the same execute path.

#![forbid(unsafe_code)]

use std::io;
use std::process::ExitCode;

use lupin_tui::app::{App, run_fullscreen, run_line_mode};
use lupin_tui::facade::{FakeFacade, Lane, McpFacade};
use lupin_tui::triggers::{TriggerEngine, default_rules, parse_rules};

struct Args {
    server: String,
    server_args: Vec<String>,
    fake: bool,
    line: bool,
    triggers_path: Option<String>,
}

fn parse_args(argv: &[String]) -> Result<Args, String> {
    let mut server = std::env::var("LUPIN_SERVER").unwrap_or_else(|_| "lupin".to_string());
    let mut server_args: Vec<String> = Vec::new();
    let mut fake = false;
    let mut line = false;
    let mut triggers_path = None;
    let mut iter = argv.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--server" => {
                server = iter.next().cloned().ok_or("--server needs a path")?;
            }
            "--root" => {
                let root = iter.next().cloned().ok_or("--root needs a dir")?;
                server_args.push("--root".to_string());
                server_args.push(root);
            }
            "--fake" => fake = true,
            "--line" => line = true,
            "--triggers" => {
                triggers_path = Some(iter.next().cloned().ok_or("--triggers needs a path")?);
            }
            "--help" | "-h" => {
                return Err("usage".to_string());
            }
            other => return Err(format!("unknown arg {other}")),
        }
    }
    // The server side of this build is always sfw-scoped, matching
    // the TUI's own view lane.
    server_args.push("--lane-scope".to_string());
    server_args.push("sfw".to_string());
    Ok(Args {
        server,
        server_args,
        fake,
        line,
        triggers_path,
    })
}

fn load_engine(path: Option<&str>) -> TriggerEngine {
    let rules = match path {
        Some(path) => match std::fs::read_to_string(path) {
            Ok(text) => parse_rules(&text).unwrap_or_else(|error| {
                eprintln!("lupin-tui: {error}; using shipped rules");
                default_rules()
            }),
            Err(error) => {
                eprintln!("lupin-tui: cannot read triggers {path}: {error}; using shipped rules");
                default_rules()
            }
        },
        None => default_rules(),
    };
    TriggerEngine::new(rules)
}

fn run() -> io::Result<()> {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let args = match parse_args(&argv) {
        Ok(args) => args,
        Err(message) => {
            eprintln!("lupin-tui: {message}");
            eprintln!(
                "usage: lupin-tui [--server <path>] [--root <dir>] [--fake] [--line] [--triggers <path>]"
            );
            return Ok(());
        }
    };
    let engine = load_engine(args.triggers_path.as_deref());
    let stdin_is_tty = io::IsTerminal::is_terminal(&io::stdin());
    let stdout_is_tty = io::IsTerminal::is_terminal(&io::stdout());
    if args.fake {
        let mut app = App::new(FakeFacade::seeded(), Lane::Sfw, engine);
        if args.line || !stdout_is_tty {
            run_line_mode(&mut app, io::stdin().lock(), io::stdout())
        } else {
            run_fullscreen(&mut app)
        }
    } else {
        let facade = McpFacade::connect(&args.server, &args.server_args).map_err(|error| {
            io::Error::other(format!("cannot connect to lupin server: {error}"))
        })?;
        let mut app = App::new(facade, Lane::Sfw, engine);
        if args.line || !stdin_is_tty || !stdout_is_tty {
            run_line_mode(&mut app, io::stdin().lock(), io::stdout())
        } else {
            run_fullscreen(&mut app)
        }
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("lupin-tui: {error}");
            ExitCode::from(1)
        }
    }
}
