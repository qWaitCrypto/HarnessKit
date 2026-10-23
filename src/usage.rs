use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;
use std::{env, io};

use serde::{Deserialize, Serialize};

use crate::fs_utils::{home_dir, now_unix, simple_content_hash};
use crate::output::println;

// One CLI invocation owns the recorder. Taking it on exit prevents duplicate
// records, including exits caused by a closed stdout/stderr pipe.
static RECORDER: Mutex<Option<Recorder>> = Mutex::new(None);

struct Recorder {
    started: Instant,
    path: PathBuf,
    full: bool,
    record: UsageRecord,
}

#[derive(Serialize, Deserialize)]
struct UsageRecord {
    timestamp_unix: u64,
    version: String,
    command: String,
    project_root_hash: String,
    duration_ms: u64,
    exit_code: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    result_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    zero_results: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    query_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    coverage_ratio: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    check_new_findings: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    query: Option<String>,
}

fn log_path() -> Option<PathBuf> {
    env::var_os("HARNESSKIT_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            home_dir()
                .filter(|path| !path.as_os_str().is_empty())
                .map(|path| path.join(".harnesskit"))
        })
        .map(|path| path.join("usage.jsonl"))
}

// Never persist argv: document paths, messages, unknown commands and query text
// can all contain private data. Only known command names enter the minimal log.
fn command_name() -> String {
    let mut args = env::args_os().skip(1);
    let first = args.next();
    match first.as_deref().and_then(|arg| arg.to_str()) {
        Some("--version" | "-V") => "--version".to_string(),
        Some("--help" | "-h" | "help") | None => "--help".to_string(),
        Some("history") => {
            let next = args.next();
            match next.as_deref().and_then(|arg| arg.to_str()) {
                Some(
                    command @ ("snapshot" | "list" | "status" | "diff" | "restore" | "export"
                    | "import" | "show"),
                ) => format!("history {}", command),
                _ => "history".to_string(),
            }
        }
        Some(
            command @ ("init" | "index" | "check" | "rank" | "list-docs" | "inspect" | "refs"
            | "query" | "graph" | "context" | "doctor" | "update"),
        ) => command.to_string(),
        _ => "unknown".to_string(),
    }
}

fn root_hash(path: &Path) -> String {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        env::current_dir().unwrap_or_default().join(path)
    };
    let resolved = absolute.canonicalize().unwrap_or_else(|_| {
        // init may target a directory that does not exist yet. Resolve existing
        // ancestors too, so a symlinked parent still identifies the same root.
        let mut result = PathBuf::new();
        for component in absolute.components() {
            match component {
                Component::CurDir => {}
                Component::ParentDir => {
                    result.pop();
                }
                other => {
                    result.push(other.as_os_str());
                    if let Ok(canonical) = result.canonicalize() {
                        result = canonical;
                    }
                }
            }
        }
        result
    });
    simple_content_hash(&resolved.to_string_lossy())
}

pub(crate) fn start() {
    let mode = env::var("HARNESSKIT_USAGE_LOG").unwrap_or_default();
    if mode == "off" {
        return;
    }
    let Some(path) = log_path() else {
        return;
    };
    if let Ok(mut recorder) = RECORDER.lock() {
        *recorder = Some(Recorder {
            started: Instant::now(),
            path,
            full: mode == "full",
            record: UsageRecord {
                timestamp_unix: now_unix(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                command: command_name(),
                project_root_hash: root_hash(Path::new(".")),
                duration_ms: 0,
                exit_code: 0,
                result_count: None,
                zero_results: None,
                query_mode: None,
                coverage_ratio: None,
                check_new_findings: None,
                query: None,
            },
        });
    }
}

fn record_with(update: impl FnOnce(&mut Recorder)) {
    if let Ok(mut recorder) = RECORDER.lock() {
        if let Some(recorder) = recorder.as_mut() {
            update(recorder);
        }
    }
}

pub(crate) fn target(path: &Path) {
    record_with(|recorder| recorder.record.project_root_hash = root_hash(path));
}

pub(crate) fn query_text(text: &str) {
    record_with(|recorder| {
        if recorder.full {
            recorder.record.query = Some(text.to_string());
        }
    });
}

pub(crate) fn results(count: usize) {
    record_with(|recorder| recorder.record.result_count = Some(count));
}

pub(crate) fn query_results(count: usize, relaxed: bool) {
    record_with(|recorder| {
        recorder.record.result_count = Some(count);
        recorder.record.zero_results = Some(count == 0);
        recorder.record.query_mode = Some(if relaxed { "relaxed" } else { "strict" }.to_string());
    });
}

pub(crate) fn coverage(ratio: f64) {
    record_with(|recorder| recorder.record.coverage_ratio = Some(ratio));
}

pub(crate) fn check_new_findings(count: usize) {
    record_with(|recorder| recorder.record.check_new_findings = Some(count));
}

pub(crate) fn finish(exit_code: i32) {
    let recorder = RECORDER
        .lock()
        .ok()
        .and_then(|mut recorder| recorder.take());
    if let Some(mut recorder) = recorder {
        recorder.record.duration_ms = recorder
            .started
            .elapsed()
            .as_millis()
            .min(u128::from(u64::MAX)) as u64;
        recorder.record.exit_code = exit_code;
        // Telemetry is best effort. No write/serialization failure may change
        // command output, stderr, or the process exit code.
        let _ = append_record(&recorder.path, &recorder.record);
    }
}

fn append_record(path: &Path, record: &UsageRecord) -> crate::Result<()> {
    let mut line = serde_json::to_vec(record)?;
    line.push(b'\n');
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?
        .write_all(&line)?;
    Ok(())
}

pub(crate) fn exit(code: i32) -> ! {
    finish(code);
    std::process::exit(code);
}

#[derive(Serialize)]
pub(crate) struct UsageReport {
    pub(crate) days: Option<u32>,
    #[serde(flatten)]
    summary: UsageStats,
    projects: Vec<ProjectUsage>,
    patterns: UsagePatterns,
    ignored_lines: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    read_error: Option<String>,
    pub(crate) hints: Vec<String>,
}

#[derive(Default, Serialize)]
struct UsageStats {
    calls: usize,
    query_calls: usize,
    zero_result_queries: usize,
    relaxed_queries: usize,
    #[serde(serialize_with = "serialize_metric")]
    zero_hit_rate: f64,
    #[serde(serialize_with = "serialize_metric")]
    relaxed_hit_rate: f64,
    #[serde(serialize_with = "serialize_metric")]
    average_duration_ms: f64,
    most_used_commands: Vec<CommandCount>,
}

#[derive(Serialize)]
struct CommandCount {
    command: String,
    calls: usize,
}

#[derive(Serialize)]
struct ProjectUsage {
    project_root_hash: String,
    #[serde(flatten)]
    summary: UsageStats,
}

#[derive(Default, Serialize)]
struct UsagePatterns {
    index_then_check: usize,
    version_calls: usize,
    #[serde(serialize_with = "serialize_metric")]
    version_rate: f64,
    frequent_version: bool,
}

fn serialize_metric<S: serde::Serializer>(
    value: &f64,
    serializer: S,
) -> std::result::Result<S::Ok, S::Error> {
    // The legacy CLI formatter rounds confidence scores to two decimals. Usage
    // rates and averages must retain their denominator precision, like coverage.
    serde_json::value::to_raw_value(value)
        .map_err(serde::ser::Error::custom)?
        .serialize(serializer)
}

#[derive(Default)]
struct Accumulator {
    stats: UsageStats,
    duration_ms: u128,
    commands: BTreeMap<String, usize>,
}

impl Accumulator {
    fn add(&mut self, record: &UsageRecord) {
        self.stats.calls += 1;
        self.duration_ms += u128::from(record.duration_ms);
        *self.commands.entry(record.command.clone()).or_default() += 1;
        // Failed queries and commands without measured results are not zero hits.
        if record.command == "query" && record.exit_code == 0 {
            if let Some(count) = record.result_count {
                self.stats.query_calls += 1;
                self.stats.zero_result_queries += usize::from(count == 0);
                self.stats.relaxed_queries +=
                    usize::from(count > 0 && record.query_mode.as_deref() == Some("relaxed"));
            }
        }
    }

    fn finish(mut self) -> UsageStats {
        if self.stats.calls > 0 {
            self.stats.average_duration_ms = self.duration_ms as f64 / self.stats.calls as f64;
        }
        if self.stats.query_calls > 0 {
            self.stats.zero_hit_rate =
                self.stats.zero_result_queries as f64 / self.stats.query_calls as f64;
            self.stats.relaxed_hit_rate =
                self.stats.relaxed_queries as f64 / self.stats.query_calls as f64;
        }
        self.stats.most_used_commands = self
            .commands
            .into_iter()
            .map(|(command, calls)| CommandCount { command, calls })
            .collect();
        self.stats.most_used_commands.sort_by(|left, right| {
            right
                .calls
                .cmp(&left.calls)
                .then_with(|| left.command.cmp(&right.command))
        });
        self.stats
    }
}

pub(crate) fn summarize(days: Option<u32>) -> UsageReport {
    let now = now_unix();
    let cutoff = days
        .map(|days| now.saturating_sub(u64::from(days) * 86400))
        .unwrap_or(0);
    let mut summary = Accumulator::default();
    let mut projects = BTreeMap::<String, Accumulator>::new();
    let mut previous = BTreeMap::<String, (String, u64, i32)>::new();
    let mut patterns = UsagePatterns::default();
    let mut ignored_lines = 0;
    let mut read_error = None;
    if let Some(path) = log_path() {
        match fs::File::open(path) {
            Ok(file) => {
                for line in BufReader::new(file).lines() {
                    let line = match line {
                        Ok(line) => line,
                        Err(error) => {
                            read_error = Some(error.to_string());
                            break;
                        }
                    };
                    if line.trim().is_empty() {
                        continue;
                    }
                    let record: UsageRecord = match serde_json::from_str(&line) {
                        Ok(record) => record,
                        Err(_) => {
                            ignored_lines += 1;
                            continue;
                        }
                    };
                    if record.timestamp_unix < cutoff || record.timestamp_unix > now {
                        continue;
                    }
                    if record.command == "check" && record.exit_code == 0 {
                        if let Some((command, timestamp, exit_code)) =
                            previous.get(&record.project_root_hash)
                        {
                            if command == "index"
                                && *exit_code == 0
                                && record
                                    .timestamp_unix
                                    .checked_sub(*timestamp)
                                    .map(|seconds| seconds <= 60)
                                    .unwrap_or(false)
                            {
                                patterns.index_then_check += 1;
                            }
                        }
                    }
                    previous.insert(
                        record.project_root_hash.clone(),
                        (
                            record.command.clone(),
                            record.timestamp_unix,
                            record.exit_code,
                        ),
                    );
                    patterns.version_calls += usize::from(record.command == "--version");
                    projects
                        .entry(record.project_root_hash.clone())
                        .or_default()
                        .add(&record);
                    summary.add(&record);
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => read_error = Some(error.to_string()),
        }
    } else {
        read_error = Some("Neither HARNESSKIT_HOME nor HOME is available".to_string());
    }
    let summary = summary.finish();
    if summary.calls > 0 {
        patterns.version_rate = patterns.version_calls as f64 / summary.calls as f64;
    }
    patterns.frequent_version = patterns.version_calls >= 3 && patterns.version_rate >= 0.2;
    let mut hints = vec!["Rates use completed queries with measured results; failed queries are excluded. Relaxed means at least one displayed result is a partial match. The current doctor invocation is logged after this report.".to_string()];
    if summary.calls == 0 {
        hints.push("No usage records in this time window. Run commands with HARNESSKIT_USAGE_LOG unset to collect minimal local metrics.".to_string());
    }
    if patterns.index_then_check > 0 {
        hints.push("An index was followed by check for the same project within 60 seconds. Run check directly; it refreshes the index automatically.".to_string());
    }
    if patterns.frequent_version {
        hints.push("At least three --version calls account for 20% or more of usage. Skip repeated version preflights during normal work.".to_string());
    }
    if ignored_lines > 0 {
        hints.push(format!(
            "Ignored {} malformed usage log line(s); valid records are still included.",
            ignored_lines
        ));
    }
    if let Some(error) = &read_error {
        hints.push(format!(
            "Usage log could not be read: {}. Check HARNESSKIT_HOME or HOME and file permissions.",
            error
        ));
    }
    UsageReport {
        days,
        summary,
        projects: projects
            .into_iter()
            .map(|(project_root_hash, accumulator)| ProjectUsage {
                project_root_hash,
                summary: accumulator.finish(),
            })
            .collect(),
        patterns,
        ignored_lines,
        read_error,
        hints,
    }
}

pub(crate) fn print_report(report: &UsageReport) {
    let period = report
        .days
        .map(|days| format!("last {} days", days))
        .unwrap_or_else(|| "all time".to_string());
    println!("Usage ({}): {} calls", period, report.summary.calls);
    println!(
        "Queries: {}; zero-hit rate: {:.1}%; relaxed-hit rate: {:.1}%",
        report.summary.query_calls,
        report.summary.zero_hit_rate * 100.0,
        report.summary.relaxed_hit_rate * 100.0
    );
    println!(
        "Average duration: {:.2} ms",
        report.summary.average_duration_ms
    );
    println!(
        "Most used subcommands: {}",
        report
            .summary
            .most_used_commands
            .iter()
            .take(5)
            .map(|item| format!("{} ({})", item.command, item.calls))
            .collect::<Vec<_>>()
            .join(", ")
    );
    for project in &report.projects {
        println!("- Project {}: {} calls; zero-hit rate: {:.1}%; relaxed-hit rate: {:.1}%; average duration: {:.2} ms", project.project_root_hash, project.summary.calls, project.summary.zero_hit_rate * 100.0, project.summary.relaxed_hit_rate * 100.0, project.summary.average_duration_ms);
    }
    println!(
        "Patterns: index -> check within 60s: {}; --version: {} ({:.1}%)",
        report.patterns.index_then_check,
        report.patterns.version_calls,
        report.patterns.version_rate * 100.0
    );
}
