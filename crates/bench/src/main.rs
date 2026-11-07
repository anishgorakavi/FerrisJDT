//! A latency and memory harness for any LSP server that speaks stdio.
//!
//! It is deliberately server-agnostic: FerrisJDT and JDTLS are driven through
//! the same requests, with the same waits, and measured the same way. A
//! benchmark that only one implementation can run proves nothing.
//!
//! Resident set size is read from the operating system (`ps -o rss`) for the
//! server process and all of its children, because a JVM-based server does its
//! work in a child process and because virtual size and heap usage would both
//! mislead.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

const USAGE: &str = "\
ferrisjdt-bench \u{2014} measure a Java language server

USAGE:
    ferrisjdt-bench --project <dir> --target <file relative to project> \\
                    --symbol <identifier in that file> \\
                    [--receiver <expression ending in a dot>] \\
                    [--insert-line <n>] [--runs <n>] [--label <name>] \\
                    [--command <program> [args...]]

The command defaults to `ferrisjdt`. Everything after --command is the server
invocation, so JDTLS is measured with `--command jdtls`.
";

struct Options {
    project: PathBuf,
    target: PathBuf,
    symbol: String,
    receiver: Option<String>,
    insert_line: Option<u32>,
    runs: usize,
    label: String,
    command: Vec<String>,
    ready_timeout: Duration,
}

fn main() {
    let options = match parse_options() {
        Ok(options) => options,
        Err(message) => {
            eprintln!("{message}\n\n{USAGE}");
            std::process::exit(2);
        }
    };
    match run(&options) {
        Ok(report) => report.print(&options.label),
        Err(message) => {
            eprintln!("benchmark failed: {message}");
            std::process::exit(1);
        }
    }
}

fn parse_options() -> Result<Options, String> {
    let mut args = std::env::args().skip(1);
    let mut project = None;
    let mut target = None;
    let mut symbol = None;
    let mut receiver = None;
    let mut insert_line = None;
    let mut runs = 50usize;
    let mut label = None;
    let mut command: Vec<String> = Vec::new();
    let mut ready_timeout = Duration::from_secs(180);

    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--project" => project = args.next().map(PathBuf::from),
            "--target" => target = args.next().map(PathBuf::from),
            "--symbol" => symbol = args.next(),
            "--receiver" => receiver = args.next(),
            "--insert-line" => {
                insert_line = args.next().and_then(|v| v.parse().ok());
            }
            "--runs" => runs = args.next().and_then(|v| v.parse().ok()).unwrap_or(runs),
            "--label" => label = args.next(),
            "--ready-timeout" => {
                if let Some(seconds) = args.next().and_then(|v| v.parse().ok()) {
                    ready_timeout = Duration::from_secs(seconds);
                }
            }
            "--command" => {
                command = args.by_ref().collect();
                break;
            }
            "--help" | "-h" => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    if command.is_empty() {
        command.push("ferrisjdt".to_string());
    }
    Ok(Options {
        project: project.ok_or("--project is required")?,
        target: target.ok_or("--target is required")?,
        symbol: symbol.ok_or("--symbol is required")?,
        receiver,
        insert_line,
        runs,
        label: label.unwrap_or_else(|| command[0].clone()),
        command,
        ready_timeout,
    })
}

struct Report {
    cold_start: Duration,
    ready: Duration,
    /// Immediately after the measured workload.
    working_rss: u64,
    /// After the process has been left alone, which on macOS lets the system
    /// compress or reclaim pages the server is no longer touching.
    idle_rss: u64,
    peak_rss: u64,
    completion: Vec<Duration>,
    hover: Vec<Duration>,
    definition: Vec<Duration>,
    document_symbol: Vec<Duration>,
    edit_to_diagnostics: Vec<Duration>,
    completion_items: usize,
    idle_cpu_percent: f64,
}

impl Report {
    fn print(&self, label: &str) {
        println!("== {label} ==");
        println!(
            "cold start (initialize response) : {:>8.1} ms",
            millis(self.cold_start)
        );
        println!(
            "ready (first diagnostics)        : {:>8.1} ms",
            millis(self.ready)
        );
        println!(
            "resident after workload          : {:>8.1} MiB",
            mib(self.working_rss)
        );
        println!(
            "resident when idle (after 4.5 s)  : {:>7.1} MiB",
            mib(self.idle_rss)
        );
        println!(
            "peak resident during workload    : {:>8.1} MiB",
            mib(self.peak_rss)
        );
        println!(
            "idle cpu                         : {:>8.1} %",
            self.idle_cpu_percent
        );
        println!(
            "completion items returned        : {:>8}",
            self.completion_items
        );
        for (name, samples) in [
            ("completion", &self.completion),
            ("hover", &self.hover),
            ("definition", &self.definition),
            ("documentSymbol", &self.document_symbol),
            ("edit -> diagnostics", &self.edit_to_diagnostics),
        ] {
            let stats = Stats::of(samples);
            println!(
                "{name:<33}: p50 {:>7.2} ms  p95 {:>7.2} ms  max {:>7.2} ms  (n={})",
                stats.p50,
                stats.p95,
                stats.max,
                samples.len()
            );
        }
    }
}

struct Stats {
    p50: f64,
    p95: f64,
    max: f64,
}

impl Stats {
    fn of(samples: &[Duration]) -> Stats {
        if samples.is_empty() {
            return Stats {
                p50: f64::NAN,
                p95: f64::NAN,
                max: f64::NAN,
            };
        }
        let mut sorted: Vec<f64> = samples.iter().map(|d| millis(*d)).collect();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
        Stats {
            p50: percentile(&sorted, 0.50),
            p95: percentile(&sorted, 0.95),
            max: *sorted.last().unwrap(),
        }
    }
}

/// Nearest-rank percentile, which needs no interpolation and is honest about
/// small sample counts.
fn percentile(sorted: &[f64], fraction: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    let rank = (fraction * sorted.len() as f64).ceil() as usize;
    sorted[rank.saturating_sub(1).min(sorted.len() - 1)]
}

fn millis(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

fn mib(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

fn run(options: &Options) -> Result<Report, String> {
    let project = options
        .project
        .canonicalize()
        .map_err(|error| format!("cannot read {}: {error}", options.project.display()))?;
    let target = project.join(&options.target);
    let text = std::fs::read_to_string(&target)
        .map_err(|error| format!("cannot read {}: {error}", target.display()))?;

    let symbol_position = find_symbol(&text, &options.symbol)
        .ok_or_else(|| format!("{:?} does not appear in the target file", options.symbol))?;

    let mut server = Server::spawn(&options.command, &project)?;
    let cold_start = server.initialize(&project)?;
    let uri = file_uri(&target);
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": uri,
                "languageId": "java",
                "version": 1,
                "text": text
            }
        }),
    );

    let readiness = Instant::now();
    server.wait_for_diagnostics(&uri, options.ready_timeout)?;
    let ready = readiness.elapsed();

    // A few unmeasured requests first: the numbers that matter are warm ones.
    for _ in 0..5 {
        let _ = server.request(
            "textDocument/hover",
            json!({
                "textDocument": { "uri": uri },
                "position": { "line": symbol_position.0, "character": symbol_position.1 }
            }),
            Duration::from_secs(30),
        );
    }

    let mut report = Report {
        cold_start,
        ready,
        working_rss: 0,
        idle_rss: 0,
        peak_rss: 0,
        completion: Vec::new(),
        hover: Vec::new(),
        definition: Vec::new(),
        document_symbol: Vec::new(),
        edit_to_diagnostics: Vec::new(),
        completion_items: 0,
        idle_cpu_percent: 0.0,
    };

    let mut version = 1i64;
    for run in 0..options.runs {
        let started = Instant::now();
        let _ = server.request(
            "textDocument/hover",
            json!({
                "textDocument": { "uri": uri },
                "position": { "line": symbol_position.0, "character": symbol_position.1 }
            }),
            Duration::from_secs(30),
        )?;
        report.hover.push(started.elapsed());

        let started = Instant::now();
        let _ = server.request(
            "textDocument/definition",
            json!({
                "textDocument": { "uri": uri },
                "position": { "line": symbol_position.0, "character": symbol_position.1 }
            }),
            Duration::from_secs(30),
        )?;
        report.definition.push(started.elapsed());

        let started = Instant::now();
        let _ = server.request(
            "textDocument/documentSymbol",
            json!({ "textDocument": { "uri": uri } }),
            Duration::from_secs(30),
        )?;
        report.document_symbol.push(started.elapsed());

        // A keystroke, then completion at the caret it created: this is the
        // path that has to be fast, and measuring it on an unedited file would
        // flatter any server that caches per document version.
        if let (Some(receiver), Some(line)) = (&options.receiver, options.insert_line) {
            version += 1;
            let indent = indentation_of(&text, line);
            let column = indent;
            server.notify(
                "textDocument/didChange",
                json!({
                    "textDocument": { "uri": uri, "version": version },
                    "contentChanges": [{
                        "range": {
                            "start": { "line": line, "character": column },
                            "end": { "line": line, "character": column }
                        },
                        "text": receiver
                    }]
                }),
            );
            let started = Instant::now();
            let completion = server.request(
                "textDocument/completion",
                json!({
                    "textDocument": { "uri": uri },
                    "position": { "line": line, "character": column + receiver.chars().count() as u32 },
                    "context": { "triggerKind": 2, "triggerCharacter": "." }
                }),
                Duration::from_secs(30),
            )?;
            report.completion.push(started.elapsed());
            let count = completion
                .get("items")
                .and_then(|items| items.as_array())
                .map(|items| items.len())
                .or_else(|| completion.as_array().map(|items| items.len()))
                .unwrap_or(0);
            report.completion_items = report.completion_items.max(count);

            // Undo the keystroke and time the diagnostics that follow it.
            version += 1;
            let edited = Instant::now();
            server.notify(
                "textDocument/didChange",
                json!({
                    "textDocument": { "uri": uri, "version": version },
                    "contentChanges": [{
                        "range": {
                            "start": { "line": line, "character": column },
                            "end": { "line": line, "character": column + receiver.chars().count() as u32 }
                        },
                        "text": ""
                    }]
                }),
            );
            if server
                .wait_for_diagnostics(&uri, Duration::from_secs(30))
                .is_ok()
            {
                report.edit_to_diagnostics.push(edited.elapsed());
            }
        }

        if run % 10 == 0 {
            report.peak_rss = report.peak_rss.max(server.resident_bytes());
        }
    }

    report.working_rss = server.resident_bytes();
    report.peak_rss = report.peak_rss.max(report.working_rss);

    // Idle measurements, after everything has settled.
    std::thread::sleep(Duration::from_millis(1500));
    let before = server.cpu_seconds();
    let window = Duration::from_secs(3);
    std::thread::sleep(window);
    let after = server.cpu_seconds();
    report.idle_cpu_percent = ((after - before) / window.as_secs_f64() * 100.0).max(0.0);
    report.idle_rss = server.resident_bytes();
    report.peak_rss = report.peak_rss.max(report.idle_rss);

    server.shutdown();
    Ok(report)
}

/// Line and UTF-16 column of the first standalone occurrence of a symbol.
fn find_symbol(text: &str, symbol: &str) -> Option<(u32, u32)> {
    for (line_index, line) in text.lines().enumerate() {
        let mut from = 0usize;
        while let Some(at) = line[from..].find(symbol) {
            let start = from + at;
            let end = start + symbol.len();
            let before_ok = start == 0 || !is_word_byte(line.as_bytes()[start - 1]);
            let after_ok = end >= line.len() || !is_word_byte(line.as_bytes()[end]);
            if before_ok && after_ok {
                let column = line[..start].chars().map(|c| c.len_utf16()).sum::<usize>();
                return Some((line_index as u32, column as u32));
            }
            from = start + 1;
        }
    }
    None
}

fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'$'
}

/// Indentation width of a line, so inserted text lands where code would.
fn indentation_of(text: &str, line: u32) -> u32 {
    text.lines()
        .nth(line as usize)
        .map(|line| line.len() - line.trim_start().len())
        .unwrap_or(0) as u32
}

fn file_uri(path: &Path) -> String {
    format!("file://{}", path.display())
}

struct Server {
    process: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: i64,
}

impl Server {
    fn spawn(command: &[String], project: &Path) -> Result<Server, String> {
        let mut process = Command::new(&command[0])
            .args(&command[1..])
            .current_dir(project)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("cannot start {}: {error}", command[0]))?;
        let stdin = process.stdin.take().unwrap();
        let stdout = BufReader::new(process.stdout.take().unwrap());
        Ok(Server {
            process,
            stdin,
            stdout,
            next_id: 1,
        })
    }

    fn initialize(&mut self, project: &Path) -> Result<Duration, String> {
        let uri = file_uri(project);
        let started = Instant::now();
        self.request(
            "initialize",
            json!({
                "processId": std::process::id(),
                "rootUri": uri,
                "workspaceFolders": [{ "uri": uri, "name": "bench" }],
                "capabilities": {
                    "workspace": { "configuration": true, "workspaceFolders": true },
                    "textDocument": {
                        "synchronization": { "didSave": true },
                        "publishDiagnostics": { "relatedInformation": true },
                        "completion": {
                            "completionItem": { "snippetSupport": true, "documentationFormat": ["markdown"] }
                        },
                        "hover": { "contentFormat": ["markdown"] },
                        "definition": { "linkSupport": true },
                        "documentSymbol": { "hierarchicalDocumentSymbolSupport": true }
                    }
                }
            }),
            Duration::from_secs(120),
        )?;
        let elapsed = started.elapsed();
        self.notify("initialized", json!({}));
        Ok(elapsed)
    }

    fn send(&mut self, message: &Value) {
        let body = serde_json::to_string(message).unwrap();
        let _ = write!(self.stdin, "Content-Length: {}\r\n\r\n{}", body.len(), body);
        let _ = self.stdin.flush();
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.send(&json!({ "jsonrpc": "2.0", "method": method, "params": params }));
    }

    fn request(&mut self, method: &str, params: Value, timeout: Duration) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        }));
        let deadline = Instant::now() + timeout;
        loop {
            if Instant::now() > deadline {
                return Err(format!("{method} timed out"));
            }
            let message = self.read_message()?;
            if message["id"] == json!(id) && message.get("method").is_none() {
                return Ok(message["result"].clone());
            }
            self.answer_if_request(&message);
        }
    }

    /// A server may send requests of its own; leaving them unanswered makes
    /// JDTLS in particular stall, so they are answered with an empty result.
    fn answer_if_request(&mut self, message: &Value) {
        let Some(id) = message.get("id") else { return };
        let Some(method) = message.get("method").and_then(|m| m.as_str()) else {
            return;
        };
        let result = match method {
            // One null per requested configuration section.
            "workspace/configuration" => {
                let count = message["params"]["items"]
                    .as_array()
                    .map(|items| items.len())
                    .unwrap_or(1);
                Value::Array(vec![Value::Null; count])
            }
            _ => Value::Null,
        };
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "result": result }));
    }

    fn wait_for_diagnostics(&mut self, uri: &str, timeout: Duration) -> Result<(), String> {
        let deadline = Instant::now() + timeout;
        loop {
            if Instant::now() > deadline {
                return Err("timed out waiting for diagnostics".to_string());
            }
            let message = self.read_message()?;
            self.answer_if_request(&message);
            if message["method"] == json!("textDocument/publishDiagnostics")
                && message["params"]["uri"] == json!(uri)
            {
                return Ok(());
            }
        }
    }

    fn read_message(&mut self) -> Result<Value, String> {
        let mut length = None;
        loop {
            let mut line = String::new();
            let read = self
                .stdout
                .read_line(&mut line)
                .map_err(|error| error.to_string())?;
            if read == 0 {
                return Err("the server closed its output".to_string());
            }
            let trimmed = line.trim_end();
            if trimmed.is_empty() {
                break;
            }
            if let Some(value) = trimmed.strip_prefix("Content-Length: ") {
                length = value.parse().ok();
            }
        }
        let length: usize = length.ok_or("a message arrived without a content length")?;
        let mut body = vec![0u8; length];
        self.stdout
            .read_exact(&mut body)
            .map_err(|error| error.to_string())?;
        serde_json::from_slice(&body).map_err(|error| error.to_string())
    }

    /// Resident memory of the server and every descendant process.
    fn resident_bytes(&self) -> u64 {
        let pids = self.process_tree();
        let mut total = 0u64;
        for pid in pids {
            let Ok(output) = Command::new("/bin/ps")
                .args(["-o", "rss=", "-p", &pid.to_string()])
                .output()
            else {
                continue;
            };
            if let Ok(kilobytes) = String::from_utf8_lossy(&output.stdout)
                .trim()
                .parse::<u64>()
            {
                total += kilobytes * 1024;
            }
        }
        total
    }

    fn cpu_seconds(&self) -> f64 {
        let mut total = 0.0;
        for pid in self.process_tree() {
            let Ok(output) = Command::new("/bin/ps")
                .args(["-o", "time=", "-p", &pid.to_string()])
                .output()
            else {
                continue;
            };
            total += parse_cpu_time(String::from_utf8_lossy(&output.stdout).trim());
        }
        total
    }

    /// The server process and its descendants. A JVM-based server runs its
    /// work in a child, so measuring only the launcher would understate it.
    fn process_tree(&self) -> Vec<u32> {
        let root = self.process.id();
        let mut found = vec![root];
        let Ok(output) = Command::new("/bin/ps").args(["-eo", "pid=,ppid="]).output() else {
            return found;
        };
        let listing = String::from_utf8_lossy(&output.stdout);
        let pairs: Vec<(u32, u32)> = listing
            .lines()
            .filter_map(|line| {
                let mut fields = line.split_whitespace();
                let pid = fields.next()?.parse().ok()?;
                let parent = fields.next()?.parse().ok()?;
                Some((pid, parent))
            })
            .collect();
        // Breadth-first, with a bound in case the table is inconsistent.
        let mut frontier = vec![root];
        for _ in 0..8 {
            let mut next = Vec::new();
            for (pid, parent) in &pairs {
                if frontier.contains(parent) && !found.contains(pid) {
                    found.push(*pid);
                    next.push(*pid);
                }
            }
            if next.is_empty() {
                break;
            }
            frontier = next;
        }
        found
    }

    fn shutdown(mut self) {
        let _ = self.request("shutdown", Value::Null, Duration::from_secs(10));
        self.notify("exit", Value::Null);
        std::thread::sleep(Duration::from_millis(300));
        let _ = self.process.kill();
        let _ = self.process.wait();
    }
}

/// Parses `ps -o time` output, which is `[[dd-]hh:]mm:ss`.
fn parse_cpu_time(text: &str) -> f64 {
    let text = text.rsplit('-').next().unwrap_or(text);
    let mut seconds = 0.0;
    for part in text.split(':') {
        seconds = seconds * 60.0 + part.parse::<f64>().unwrap_or(0.0);
    }
    seconds
}
