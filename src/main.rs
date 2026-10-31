//! lupin — MCP server for the Revenue Alignment Engine (stdio).
//!
//! Serves the Revenue Alignment Engine's ledger and capability registry.
//! Posture (MCP-SPEC.md): loopback/operator use;
//! lane-scoped (an `sfw`-scoped caller can never see `nsfw` entries);
//! read-mostly — the only write files a candidate into scans/, never the
//! validated ledger.
//!
//! As of 0.2.0 (WS2 of the MCP standardization program, decision
//! D-113) this server runs on rmcp, the official Rust MCP SDK, and
//! speaks the modern protocol era only (2026-07-28): sessions open
//! with `server/discover`, and every request carries its protocol
//! version in `_meta` — there is no `initialize` handshake. Legacy
//! contact is refused at the stdio front door (see `guard`) with
//! the typed `-32022` unsupported-version error, naming 2026-07-28
//! as the one supported version. The ledger/capability markdown
//! files and the on-disk inbox format are unchanged.
//!
//! Usage: `lupin [--root <dir>] [--lane-scope operator|sfw]`.
//! The root defaults to `$REVENUE_ROOT`, then
//! `$HOME/workspace/revenue-alignment-20261019`.

#![forbid(unsafe_code)]

mod guard;
mod ledger;
mod server;

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use rmcp::ServiceExt;
use rmcp::transport::async_rw::AsyncRwTransport;
use server::Config;
use tokio::sync::Mutex;

/// Duplex buffer between the era guard and the SDK service. Request
/// lines can reach the 4 MiB line bound; the SDK consumes
/// incrementally, so this only needs to cover response bursts.
const DUPLEX_BUFFER_BYTES: usize = 1024 * 1024;

/// Parse the baseline's CLI: `--root`, `--lane-scope`, the
/// `REVENUE_ROOT` fallback, unknown args warned about and ignored.
/// A bad lane scope is exit 2, as the baseline had it.
fn parse_config(args: &[String]) -> Result<Config, ExitCode> {
    let mut root: Option<PathBuf> = std::env::var_os("REVENUE_ROOT").map(PathBuf::from);
    let mut scope = "operator".to_string();
    let mut argv = args.iter();
    while let Some(arg) = argv.next() {
        match arg.as_str() {
            "--root" => root = argv.next().map(PathBuf::from),
            "--lane-scope" => {
                if let Some(v) = argv.next() {
                    scope = v.clone();
                }
            }
            other => eprintln!("lupin: ignoring unknown arg {other}"),
        }
    }
    if scope != "operator" && scope != "sfw" {
        eprintln!("lupin: --lane-scope must be 'operator' or 'sfw'");
        return Err(ExitCode::from(2));
    }
    let root = root.unwrap_or_else(|| {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default();
        home.join("workspace").join("revenue-alignment-20261019")
    });
    Ok(Config { root, scope })
}

/// Wire the pieces together: the guard owns stdio, the SDK service
/// sits behind an in-process duplex, and a pump copies its responses
/// back to stdout through the same lock the guard writes with.
async fn run(cfg: Config) -> std::io::Result<()> {
    let (client_side, server_side) = tokio::io::duplex(DUPLEX_BUFFER_BYTES);
    let (server_read, server_write) = tokio::io::split(server_side);
    let (client_read, client_write) = tokio::io::split(client_side);
    let stdout: guard::SharedStdout = Arc::new(Mutex::new(tokio::io::stdout()));

    let ordering = guard::Ordering::new();
    let handler = server::LupinServer::new(cfg);
    let serve = tokio::spawn(async move {
        let transport = AsyncRwTransport::new_server(server_read, server_write);
        match handler.serve(transport).await {
            Ok(running) => running
                .waiting()
                .await
                .map(|_| ())
                .map_err(|e| std::io::Error::other(e.to_string())),
            // The client closed the connection before selecting a
            // lifecycle (a probe, a spawn check, a client that only
            // ever wanted the refusal): the baseline treated bare
            // EOF as a clean end of session, and so do we.
            Err(rmcp::service::ServerInitializeError::ConnectionClosed(_)) => Ok(()),
            Err(e) => Err(std::io::Error::other(e.to_string())),
        }
    });
    let mut pump = tokio::spawn(guard::pump(
        client_read,
        Arc::clone(&stdout),
        Arc::clone(&ordering),
    ));

    // When stdin reaches EOF the guard shuts the duplex write side
    // down, which ends the SDK service; the pump ends when the
    // service's transport drops. The pump wait is bounded as a
    // backstop: a response still in flight when the service ends
    // gets its chance to drain, but a wedged pump cannot hang exit.
    let guard_result = guard::run(tokio::io::stdin(), client_write, stdout, ordering).await;
    let serve_result = match serve.await {
        Ok(result) => result,
        Err(e) => Err(std::io::Error::other(e.to_string())),
    };
    if tokio::time::timeout(std::time::Duration::from_secs(2), &mut pump)
        .await
        .is_err()
    {
        pump.abort();
    }
    guard_result.and(serve_result)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cfg = match parse_config(&args) {
        Ok(cfg) => cfg,
        Err(code) => return code,
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            eprintln!("lupin: cannot start runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(run(cfg)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("lupin: serve loop ended: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_config_accepts_forms() {
        let cfg = parse_config(&[]).expect("defaults parse");
        assert_eq!(cfg.scope, "operator");
        let cfg = parse_config(&[
            "--root".to_string(),
            "/tmp/x".to_string(),
            "--lane-scope".to_string(),
            "sfw".to_string(),
        ])
        .expect("explicit parse");
        assert_eq!(cfg.root, PathBuf::from("/tmp/x"));
        assert_eq!(cfg.scope, "sfw");
        assert!(parse_config(&["--lane-scope".to_string(), "nsfw".to_string()]).is_err());
    }
}
