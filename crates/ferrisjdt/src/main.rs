//! FerrisJDT: a lightweight Java language server.

mod capabilities;
mod handlers;
mod memory;
mod selfcheck;
mod server;
mod state;
mod status;

use std::path::PathBuf;
use tracing_subscriber::EnvFilter;

const USAGE: &str = "\
ferrisjdt (ferrisjdt) \u{2014} a lightweight Java language server

USAGE:
    ferrisjdt [--stdio]              run as a language server over stdin/stdout
    ferrisjdt --self-check <dir>     index a project and report findings
    ferrisjdt --version
    ferrisjdt --help

ENVIRONMENT:
    FERRISJDT_LOG   log filter, e.g. `info`, `debug`, `ferrisjdt=trace`
                    (logs always go to stderr; stdout carries the protocol)
";

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut verbose = false;
    let mut self_check: Option<PathBuf> = None;

    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--help" | "-h" => {
                print!("{USAGE}");
                return std::process::ExitCode::SUCCESS;
            }
            "--version" | "-V" => {
                println!("ferrisjdt {}", env!("CARGO_PKG_VERSION"));
                return std::process::ExitCode::SUCCESS;
            }
            "--stdio" => {}
            "--verbose" | "-v" => verbose = true,
            "--self-check" => {
                index += 1;
                match args.get(index) {
                    Some(path) => self_check = Some(PathBuf::from(path)),
                    None => {
                        eprintln!("--self-check needs a directory");
                        return std::process::ExitCode::FAILURE;
                    }
                }
            }
            other => {
                eprintln!("unknown argument: {other}\n\n{USAGE}");
                return std::process::ExitCode::FAILURE;
            }
        }
        index += 1;
    }

    init_logging();

    if let Some(root) = self_check {
        let root = match root.canonicalize() {
            Ok(path) => path,
            Err(error) => {
                eprintln!("cannot read {}: {error}", root.display());
                return std::process::ExitCode::FAILURE;
            }
        };
        let code = selfcheck::self_check(&root, verbose);
        return std::process::ExitCode::from(code as u8);
    }

    let (connection, io_threads) = lsp_server::Connection::stdio();
    if let Err(error) = server::run(connection) {
        tracing::error!(%error, "server stopped with an error");
        return std::process::ExitCode::FAILURE;
    }
    if let Err(error) = io_threads.join() {
        tracing::error!(%error, "io threads stopped with an error");
        return std::process::ExitCode::FAILURE;
    }
    tracing::info!("server stopped");
    std::process::ExitCode::SUCCESS
}

/// Logging goes to stderr without exception: anything on stdout would be
/// interleaved with the JSON-RPC stream and would break the client.
fn init_logging() {
    let filter = EnvFilter::try_from_env("FERRISJDT_LOG")
        .unwrap_or_else(|_| EnvFilter::new("warn,ferrisjdt=info,ferrisjdt=info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(true)
        .with_ansi(false)
        .init();
}
