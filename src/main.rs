mod sqlite_cli;

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use sqlite_cli::{QueryResult, SqliteConnection};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

const BUNDLED_SCHEMA_PATH: &str = "schemas/file-first-v0.yaml";
const FACT_SCHEMA_VERSION: &str = "3";
static SNAPSHOT_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Default, Clone)]
struct Schema {
    schema_version: u32,
    managed_root: String,
    entrypoints: EntryPoints,
    core_files: Vec<CoreFileSpec>,
    doc_collections: Vec<DocCollectionSpec>,
    rules: Rules,
    suppressions: Vec<SuppressionSpec>,
}

#[derive(Default, Clone)]
struct EntryPoints {
    agents: Option<String>,
    claude: Option<String>,
    architecture: Option<String>,
    manifest: Option<String>,
    project_map: Option<String>,
}

#[derive(Default, Clone)]
struct CoreFileSpec {
    name: String,
    path: String,
    authority: Option<String>,
    status: Option<String>,
    required: bool,
    template: String,
    root_path: Option<String>,
}

#[derive(Default, Clone)]
struct DocCollectionSpec {
    name: String,
    path: String,
    authority: Option<String>,
    status: Option<String>,
    anchor_candidate: bool,
    template: String,
    allowed_frontmatter: Vec<String>,
}

#[derive(Default, Clone)]
struct Rules {
    path_defines_doc_type: Option<bool>,
    h1_is_title: Option<bool>,
    first_paragraph_is_summary: Option<bool>,
    index_required: Option<bool>,
    prefer_directory_defaults: Option<bool>,
    minimal_frontmatter_only: Option<bool>,
    stale_explicit_anchor: Option<bool>,
    duplicate_detection: Option<bool>,
    contamination_checks: Option<bool>,
    strict_checks: Option<bool>,
}

#[derive(Default, Clone)]
struct SuppressionSpec {
    name: String,
    rule_id: String,
    path: String,
    reason: String,
}

#[derive(Default)]
struct CommandOptions {
    json: bool,
    strict: bool,
    rules: Option<BTreeSet<String>>,
}

#[derive(Default)]
struct InitStats {
    created_files: usize,
    skipped_files: usize,
}

#[derive(Copy, Clone, PartialEq, Eq)]
enum InitMode {
    Local,
    Tracked,
}

enum WriteOutcome {
    Created,
    Skipped,
}

#[derive(Copy, Clone, PartialEq, Eq)]
enum Section {
    None,
    EntryPoints,
    CoreFiles,
    DocCollections,
    Rules,
    Suppressions,
}

struct RenderPaths {
    manifest: String,
    project_map: String,
    architecture: String,
    commands: String,
    product_specs_index: String,
    design_docs_index: String,
    decisions_index: String,
    exec_plans_active_index: String,
    worklog_active_index: String,
    generated_index: String,
    references_index: String,
}

#[derive(Clone, Default)]
struct IndexedDoc {
    path: String,
    title: String,
    summary: String,
    frontmatter_keys: Vec<String>,
    content_text: String,
    role: String,
    status: Option<String>,
    authority: Option<String>,
    collection: Option<String>,
    headings: Vec<String>,
    links: Vec<String>,
    path_mentions: Vec<String>,
    scope_paths: Vec<String>,
    supersedes: Vec<String>,
    incoming_links: usize,
    outgoing_links: usize,
    is_entrypoint: bool,
    is_anchor_candidate: bool,
    is_generated: bool,
    is_reference: bool,
    content_hash: String,
    file_size_bytes: u64,
    mtime_unix: u64,
    mtime_ns: u64,
}

#[derive(Clone)]
struct DocRelation {
    src_path: String,
    dst_path: String,
    relation_type: String,
    reason: String,
    explicit: bool,
    confidence: f32,
    evidence_json: String,
}

#[derive(Default)]
struct IndexArtifact {
    repo_root: String,
    managed_root: String,
    generated_at_unix: u64,
    history_latest_snapshot: Option<String>,
    history_dirty: bool,
    history_tracked_files_count: usize,
    host_git_head: Option<String>,
    host_git_branch: Option<String>,
    host_git_dirty: bool,
    docs: Vec<IndexedDoc>,
    relations: Vec<DocRelation>,
    checks: Vec<IndexCheck>,
}

struct ParsedDoc {
    frontmatter: BTreeMap<String, String>,
    title: Option<String>,
    summary: String,
    body_text: String,
    headings: Vec<String>,
    links: Vec<String>,
    path_mentions: Vec<String>,
    scope_paths: Vec<String>,
    supersedes: Vec<String>,
}

#[derive(Clone)]
struct IndexCheck {
    rule_id: String,
    severity: String,
    subject_path: String,
    message: String,
    evidence_json: String,
}

struct EngineContext {
    target_dir: PathBuf,
    schema: Schema,
}

struct PathDiagnostics {
    target_dir: String,
    current_dir: String,
    pwd: Option<String>,
    state_dir: String,
    facts_path: String,
}

struct CommandCheck {
    present: bool,
    version: Option<String>,
    error: Option<String>,
}

struct DoctorReport {
    cli_version: String,
    os: String,
    arch: String,
    target_dir: String,
    current_dir: String,
    pwd: Option<String>,
    pwd_differs_from_current_dir: bool,
    sqlite3: CommandCheck,
    git: CommandCheck,
    inside_git_repo: Option<bool>,
    git_repo_error: Option<String>,
    state_exists: bool,
    state_writable: bool,
    state_error: Option<String>,
    history_exists: bool,
    schema_exists: bool,
    claude_skill_path: String,
    claude_skill_installed: bool,
    codex_skill_path: String,
    codex_skill_installed: bool,
    path_contains_cli_dir: bool,
    current_exe: Option<String>,
}

#[derive(Clone, Default)]
struct FileStateRecord {
    content_hash: String,
    file_size_bytes: u64,
    mtime_unix: u64,
    mtime_ns: u64,
}

#[derive(Default)]
struct FactStoreDelta {
    added_paths: Vec<String>,
    changed_paths: Vec<String>,
    removed_paths: Vec<String>,
}

#[derive(Clone)]
struct CheckView {
    severity: String,
    rule_id: String,
    subject_path: String,
    message: String,
    evidence_json: String,
    suppressed: bool,
    suppression_reason: Option<String>,
}

#[derive(Clone)]
struct QueryCandidate {
    path: String,
    title: String,
    role: String,
    status: String,
    score: String,
    why: String,
    summary: String,
    neighbors: Vec<String>,
    snippet_heading: String,
    snippet_location: String,
    snippet: String,
}

struct ContextPacket {
    anchor: QueryCandidate,
    incoming: Vec<DocRelation>,
    outgoing: Vec<DocRelation>,
    collection_siblings: Vec<String>,
    same_scope_docs: Vec<String>,
    recommended_reading_order: Vec<String>,
}

#[derive(Clone, Default)]
struct HistoryFileRecord {
    path: String,
    hash: String,
    size: u64,
    mtime_unix: u64,
}

#[derive(Clone, Default)]
struct HistorySnapshot {
    id: String,
    created_at_unix: u64,
    message: String,
    docs_root: String,
    schema_hash: String,
    files: Vec<HistoryFileRecord>,
    host_git_head: Option<String>,
    host_git_branch: Option<String>,
    host_git_dirty: bool,
}

#[derive(Default)]
struct HistoryStatus {
    latest_snapshot: Option<String>,
    tracked_files_count: usize,
    added: Vec<String>,
    changed: Vec<String>,
    removed: Vec<String>,
    schema_changed: bool,
}

#[derive(Default)]
struct HostGitInfo {
    head: Option<String>,
    branch: Option<String>,
    dirty: bool,
}

#[derive(Copy, Clone)]
enum HostGitMode {
    Full,
}

fn main() -> Result<()> {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        usage();
        std::process::exit(2);
    }

    match args[1].as_str() {
        "init" => run_init(&args[2..])?,
        "index" => run_index(&args[2..])?,
        "check" => run_check(&args[2..])?,
        "rank" => run_rank(&args[2..])?,
        "list-docs" => run_list_docs(&args[2..])?,
        "inspect" => run_inspect(&args[2..])?,
        "refs" => run_refs(&args[2..])?,
        "query" => run_query(&args[2..])?,
        "graph" => run_graph(&args[2..])?,
        "context" => run_context(&args[2..])?,
        "history" => run_history(&args[2..])?,
        "doctor" => run_doctor(&args[2..])?,
        "update" => run_update(&args[2..])?,
        "help" | "--help" | "-h" => usage(),
        "--version" | "-V" => println!("harnesskit {}", env!("CARGO_PKG_VERSION")),
        _ => {
            usage();
            std::process::exit(2);
        }
    }

    Ok(())
}

fn usage() {
    eprintln!("harnesskit init [target_dir] [--schema <path>] [--docs-root <path>] [--force] [--local|--tracked] [--preview]");
    eprintln!("harnesskit index [target_dir] [--schema <path>] [--docs-root <path>]");
    eprintln!("harnesskit rank [target_dir] [--schema <path>] [--docs-root <path>]");
    eprintln!("harnesskit list-docs [target_dir] [--schema <path>] [--docs-root <path>]");
    eprintln!("harnesskit inspect <doc_path> [target_dir] [--schema <path>] [--docs-root <path>]");
    eprintln!("harnesskit refs <doc_path> [target_dir] [--schema <path>] [--docs-root <path>]");
    eprintln!(
        "harnesskit query <terms> [target_dir] [--schema <path>] [--docs-root <path>] [--json]"
    );
    eprintln!(
        "harnesskit graph <doc_path> [target_dir] [--schema <path>] [--docs-root <path>] [--json]"
    );
    eprintln!("harnesskit context <terms-or-doc-path> [target_dir] [--schema <path>] [--docs-root <path>] [--json]");
    eprintln!("harnesskit check [target_dir] [--schema <path>] [--docs-root <path>] [--rules <list>] [--strict|--no-strict] [--json]");
    eprintln!("harnesskit history snapshot -m <message> [target_dir] [--schema <path>] [--docs-root <path>]");
    eprintln!("harnesskit history list [target_dir] [--schema <path>] [--docs-root <path>]");
    eprintln!("harnesskit history status [target_dir] [--schema <path>] [--docs-root <path>]");
    eprintln!("harnesskit history diff <snapshot-id|latest> [target_dir] [--schema <path>] [--docs-root <path>]");
    eprintln!(
        "harnesskit history diff <a> <b> [target_dir] [--schema <path>] [--docs-root <path>]"
    );
    eprintln!("harnesskit history restore <snapshot-id|latest> [target_dir] [--apply] [--force] [--schema <path>] [--docs-root <path>]");
    eprintln!("harnesskit doctor [target_dir] [--json]");
    eprintln!("harnesskit update [--version <tag>] [--repo <owner/name>] [--asset-base-url <url>]");
    eprintln!("harnesskit --version");
}

fn run_init(args: &[String]) -> Result<()> {
    let target_dir = PathBuf::from(positional_or_default(args, 0, "."));
    let docs_root_override = option_value(args, "--docs-root");
    let schema_arg =
        option_value(args, "--schema").unwrap_or_else(|| BUNDLED_SCHEMA_PATH.to_string());
    let force = has_flag(args, "--force");
    let preview = has_flag(args, "--preview");
    let local = has_flag(args, "--local");
    let tracked = has_flag(args, "--tracked");
    if local && tracked {
        return Err("harnesskit init accepts only one of --local or --tracked".into());
    }
    let mode = if tracked {
        InitMode::Tracked
    } else {
        InitMode::Local
    };

    let schema_path = resolve_schema_path(&target_dir, &schema_arg)?;
    let raw_schema_text = fs::read_to_string(&schema_path)?;
    let mut schema = parse_schema(&raw_schema_text)?;
    let original_managed_root = schema.managed_root.clone();
    if let Some(ref docs_root) = docs_root_override {
        schema.managed_root = docs_root.clone();
        rewrite_docs_root_bound_paths(&mut schema, &original_managed_root, docs_root);
    }
    let schema_copy_text = render_schema_copy(&schema);

    if preview {
        print_init_preview(&target_dir, &schema, &schema_path, mode, force);
        return Ok(());
    }

    ensure_dir(&target_dir)?;

    let stats = materialize_from_schema(&target_dir, &schema, &schema_copy_text, force)?;
    let exclude_stats = if mode == InitMode::Local {
        ensure_host_git_exclude(&target_dir, &schema)?
    } else {
        None
    };

    println!(
        "Initialized HarnessKit scaffold at {} using schema {} (created {}, skipped {})",
        target_dir.display(),
        schema_path.display(),
        stats.created_files,
        stats.skipped_files
    );
    print_init_summary(&target_dir, &schema, mode, exclude_stats);
    Ok(())
}

fn run_index(args: &[String]) -> Result<()> {
    let context = load_engine_context(args, 0)?;
    refresh_fact_store(&context)?;
    let target_dir = context.target_dir;
    let db = open_fact_store(&target_dir)?;
    let doc_count = scalar_count(&db, "SELECT COUNT(*) FROM docs;")?;
    let relation_count = scalar_count(&db, "SELECT COUNT(*) FROM relations;")?;

    println!(
        "Indexed HarnessKit docs at {} (docs {}, relations {})",
        target_dir.display(),
        doc_count,
        relation_count
    );
    Ok(())
}

fn run_check(args: &[String]) -> Result<()> {
    let context = load_engine_context(args, 0)?;
    let options = command_options(args, Some(&context.schema));
    refresh_fact_store(&context)?;
    let target_dir = context.target_dir;
    let db = open_fact_store(&target_dir)?;
    let docs_count = scalar_count(&db, "SELECT COUNT(*) FROM docs;")?;
    let relation_count = scalar_count(&db, "SELECT COUNT(*) FROM relations;")?;
    let check_rows = db.query(
        "SELECT severity, rule_id, subject_path, message, evidence_json FROM checks ORDER BY
            CASE severity WHEN 'error' THEN 0 WHEN 'warning' THEN 1 ELSE 2 END,
            subject_path ASC,
            rule_id ASC;",
    )?;
    let checks = filter_and_suppress_checks(&check_rows, &context.schema, &options);
    let warning_count = checks
        .iter()
        .filter(|check| !check.suppressed && check.severity == "warning")
        .count();
    let error_count = checks
        .iter()
        .filter(|check| !check.suppressed && check.severity == "error")
        .count();
    let info_count = checks
        .iter()
        .filter(|check| !check.suppressed && check.severity == "info")
        .count();
    let active_count = checks.iter().filter(|check| !check.suppressed).count();

    if options.json {
        println!(
            "{}",
            render_check_json(&target_dir, docs_count, relation_count, &checks)
        );
        if error_count > 0 || (options.strict && warning_count > 0) {
            std::process::exit(1);
        }
        return Ok(());
    }

    println!(
        "HarnessKit checks for {}: docs {}, relations {}, checks {}",
        target_dir.display(),
        docs_count,
        relation_count,
        active_count
    );

    if checks.iter().all(|check| check.suppressed) {
        println!("No checks emitted.");
        return Ok(());
    }

    println!(
        "Severity summary: error {}, warning {}, info {}",
        error_count, warning_count, info_count
    );
    for check in &checks {
        if check.suppressed {
            println!(
                "[suppressed] {} {} - {}",
                check.rule_id,
                check.subject_path,
                check.suppression_reason.as_deref().unwrap_or("suppressed")
            );
            continue;
        }
        println!(
            "[{}] {} {} - {}",
            check.severity, check.rule_id, check.subject_path, check.message
        );
        if !check.evidence_json.is_empty() && check.evidence_json != "{}" {
            println!("  evidence: {}", check.evidence_json);
        }
    }

    if error_count > 0 || (options.strict && warning_count > 0) {
        std::process::exit(1);
    }

    Ok(())
}

fn run_doctor(args: &[String]) -> Result<()> {
    let target_dir = PathBuf::from(positional_or_default(args, 0, "."));
    let json = has_flag(args, "--json");
    let report = build_doctor_report(&target_dir);

    if json {
        println!("{}", render_doctor_json(&report));
    } else {
        print_doctor_report(&report);
    }
    Ok(())
}

fn run_update(args: &[String]) -> Result<()> {
    let version = option_value(args, "--version").unwrap_or_else(|| "latest".to_string());
    let repo = option_value(args, "--repo").unwrap_or_else(|| "qWaitCrypto/HarnessKit".to_string());
    let asset_base_url_arg = option_value(args, "--asset-base-url");
    if !repo.contains('/') {
        return Err("--repo must use owner/name format".into());
    }

    let target = detect_release_target()?;
    let package = format!("harnesskit-{}", target);
    let archive = format!("{}.tar.gz", package);
    let asset_base_url = asset_base_url_arg.unwrap_or_else(|| {
        if version == "latest" {
            format!("https://github.com/{}/releases/latest/download", repo)
        } else {
            format!("https://github.com/{}/releases/download/{}", repo, version)
        }
    });

    let current_exe = env::current_exe()?;
    let tmp_dir = env::temp_dir().join(format!(
        "harnesskit-update-{}-{}",
        std::process::id(),
        now_unix()
    ));
    fs::create_dir_all(&tmp_dir)?;
    let result = update_from_release(
        &asset_base_url,
        &archive,
        &package,
        &tmp_dir,
        &current_exe,
        &version,
        &target,
    );
    let _ = fs::remove_dir_all(&tmp_dir);
    result
}

fn detect_release_target() -> Result<String> {
    let os = env::consts::OS;
    let arch = env::consts::ARCH;
    match (os, arch) {
        ("linux", "x86_64") => Ok("x86_64-unknown-linux-gnu".to_string()),
        ("macos", "x86_64") => Ok("x86_64-apple-darwin".to_string()),
        ("macos", "aarch64") => Ok("aarch64-apple-darwin".to_string()),
        _ => Err(format!(
            "unsupported platform: {} {}. This alpha updater currently supports Linux x86_64 and macOS x86_64/arm64.",
            os, arch
        )
        .into()),
    }
}

fn update_from_release(
    asset_base_url: &str,
    archive: &str,
    package: &str,
    tmp_dir: &Path,
    current_exe: &Path,
    version: &str,
    target: &str,
) -> Result<()> {
    let archive_path = tmp_dir.join(archive);
    let sums_path = tmp_dir.join("SHA256SUMS");
    println!("Downloading HarnessKit {} for {}...", version, target);
    download_to_file(
        &format!("{}/{}", asset_base_url.trim_end_matches('/'), archive),
        &archive_path,
    )?;
    download_to_file(
        &format!("{}/SHA256SUMS", asset_base_url.trim_end_matches('/')),
        &sums_path,
    )?;
    verify_sha256(tmp_dir, "SHA256SUMS")?;
    run_command(
        Command::new("tar")
            .arg("-xzf")
            .arg(&archive_path)
            .arg("-C")
            .arg(tmp_dir),
        "tar extract failed",
    )?;

    let package_dir = tmp_dir.join(package);
    let next_binary = package_dir.join("harnesskit");
    if !next_binary.is_file() {
        return Err(format!("release archive did not contain {}", next_binary.display()).into());
    }

    let replacement = current_exe.with_file_name(format!(
        ".harnesskit-update-{}-{}",
        std::process::id(),
        now_unix()
    ));
    fs::copy(&next_binary, &replacement)?;
    make_executable(&replacement)?;
    fs::rename(&replacement, current_exe).map_err(|err| {
        let _ = fs::remove_file(&replacement);
        format!(
            "failed to replace current binary at {}: {}",
            current_exe.display(),
            err
        )
    })?;
    println!("Updated CLI: {}", current_exe.display());

    sync_existing_installed_skills(&package_dir)?;
    Ok(())
}

fn download_to_file(url: &str, out: &Path) -> Result<()> {
    if command_exists("curl") {
        let status = Command::new("curl")
            .arg("-fsSL")
            .arg("--retry")
            .arg("3")
            .arg("--retry-delay")
            .arg("2")
            .arg("--connect-timeout")
            .arg("20")
            .arg("--max-time")
            .arg("120")
            .arg(url)
            .arg("-o")
            .arg(out)
            .status()?;
        if status.success() {
            return Ok(());
        }
    } else if command_exists("wget") {
        let status = Command::new("wget")
            .arg("-q")
            .arg("--tries=3")
            .arg("--timeout=120")
            .arg(url)
            .arg("-O")
            .arg(out)
            .status()?;
        if status.success() {
            return Ok(());
        }
    } else {
        return Err("missing required command: curl or wget".into());
    }

    Err(format!(
        "download failed: {}. If you are behind a proxy, set http_proxy, https_proxy, or all_proxy and retry.",
        url
    )
    .into())
}

fn verify_sha256(dir: &Path, sums_file: &str) -> Result<()> {
    if command_exists("sha256sum") {
        run_command(
            Command::new("sha256sum")
                .arg("-c")
                .arg(sums_file)
                .current_dir(dir),
            "sha256 verification failed",
        )
    } else if command_exists("shasum") {
        run_command(
            Command::new("shasum")
                .arg("-a")
                .arg("256")
                .arg("-c")
                .arg(sums_file)
                .current_dir(dir),
            "sha256 verification failed",
        )
    } else {
        Err("missing required command: sha256sum or shasum".into())
    }
}

fn run_command(command: &mut Command, message: &str) -> Result<()> {
    let output = command.output()?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let detail = if !stderr.is_empty() {
        stderr
    } else if !stdout.is_empty() {
        stdout
    } else {
        output.status.to_string()
    };
    Err(format!("{}: {}", message, detail).into())
}

fn command_exists(name: &str) -> bool {
    Command::new(name)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
}

fn make_executable(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(path)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions)?;
    }
    Ok(())
}

fn sync_existing_installed_skills(package_dir: &Path) -> Result<()> {
    let source_skill = package_dir
        .join("agent-surfaces")
        .join("skills")
        .join("harnesskit")
        .join("SKILL.md");
    if !source_skill.is_file() {
        return Ok(());
    }

    if let Some(home) = home_dir() {
        let claude_skill = home
            .join(".claude")
            .join("skills")
            .join("harnesskit")
            .join("SKILL.md");
        if claude_skill.exists() {
            copy_skill(&source_skill, &claude_skill)?;
            println!("Updated Claude Code skill: {}", claude_skill.display());
        }

        let codex_home = env::var_os("CODEX_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".codex"));
        let codex_skill = codex_home
            .join("skills")
            .join("harnesskit")
            .join("SKILL.md");
        if codex_skill.exists() {
            copy_skill(&source_skill, &codex_skill)?;
            println!("Updated Codex skill: {}", codex_skill.display());
        }
    }

    Ok(())
}

fn home_dir() -> Option<PathBuf> {
    env::var_os("HOME").map(PathBuf::from)
}

fn copy_skill(source: &Path, target: &Path) -> Result<()> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(source, target)?;
    Ok(())
}

fn run_rank(args: &[String]) -> Result<()> {
    let context = load_engine_context(args, 0)?;
    refresh_fact_store(&context)?;
    let db = open_fact_store(&context.target_dir)?;
    let result = db.query(
        "SELECT d.path, d.title, COALESCE(SUM(CASE
            WHEN r.relation_type = 'doc_indexes_doc' THEN 3
            WHEN r.relation_type = 'doc_links_doc' THEN 1
            WHEN r.relation_type = 'supersedes' THEN 1
            ELSE 0 END), 0) AS score,
            d.role,
            d.summary
         FROM docs d
         LEFT JOIN relations r ON r.dst_path = d.path
         GROUP BY d.path, d.title, d.role, d.summary
         ORDER BY score DESC, d.path ASC
         LIMIT 20;",
    )?;

    print_rank_rows(&result);
    Ok(())
}

fn run_list_docs(args: &[String]) -> Result<()> {
    let context = load_engine_context(args, 0)?;
    refresh_fact_store(&context)?;
    let db = open_fact_store(&context.target_dir)?;
    let result = db.query(
        "SELECT path, title, role, COALESCE(status, ''), COALESCE(authority, ''), incoming_links, outgoing_links, COALESCE(collection_name, '')
         FROM docs
         ORDER BY path ASC;",
    )?;

    print_list_docs_rows(&result);
    Ok(())
}

fn run_inspect(args: &[String]) -> Result<()> {
    if args.is_empty() {
        return Err("inspect requires <doc_path>".into());
    }
    let doc_path = args[0].clone();
    let context = load_engine_context(&args[1..], 0)?;
    refresh_fact_store(&context)?;
    let db = open_fact_store(&context.target_dir)?;

    let escaped = sql_string_literal(&doc_path);
    let doc = db.query(&format!(
        "SELECT path, title, role, COALESCE(status, ''), COALESCE(authority, ''), COALESCE(collection_name, ''), summary, incoming_links, outgoing_links, is_entrypoint, is_anchor_candidate, is_generated, is_reference, file_size_bytes, mtime_unix
         FROM docs WHERE path = {};",
        escaped
    ))?;
    if doc.rows.is_empty() {
        return Err(format!("doc not found in fact store: {}", doc_path).into());
    }

    let outgoing = db.query(&format!(
        "SELECT relation_type, dst_path, reason, confidence
         FROM relations
         WHERE src_path = {}
         ORDER BY relation_type ASC, dst_path ASC;",
        escaped
    ))?;
    let incoming = db.query(&format!(
        "SELECT relation_type, src_path, reason, confidence
         FROM relations
         WHERE dst_path = {}
         ORDER BY relation_type ASC, src_path ASC;",
        escaped
    ))?;
    let checks = db.query(&format!(
        "SELECT severity, rule_id, message, evidence_json
         FROM checks
         WHERE subject_path = {}
         ORDER BY severity DESC, rule_id ASC;",
        escaped
    ))?;

    print_inspect_output(&doc, &outgoing, &incoming, &checks);
    Ok(())
}

fn run_refs(args: &[String]) -> Result<()> {
    if args.is_empty() {
        return Err("refs requires <doc_path>".into());
    }
    let doc_path = args[0].clone();
    let context = load_engine_context(&args[1..], 0)?;
    refresh_fact_store(&context)?;
    let db = open_fact_store(&context.target_dir)?;
    let result = db.query(&format!(
        "SELECT src_path, relation_type, reason, confidence
         FROM relations
         WHERE dst_path = {}
         ORDER BY relation_type ASC, src_path ASC;",
        sql_string_literal(&doc_path)
    ))?;

    print_refs_output(&doc_path, &result);
    Ok(())
}

fn run_query(args: &[String]) -> Result<()> {
    if args.is_empty() {
        return Err("query requires <terms>".into());
    }

    let query_terms = args[0].clone();
    let context = load_engine_context(&args[1..], 0)?;
    let options = command_options(args, Some(&context.schema));
    refresh_fact_store(&context)?;
    let db = open_fact_store(&context.target_dir)?;

    let result = query_candidates(&db, &context.target_dir, &query_terms)?;
    let candidates = query_rows_to_candidates(&result, &context.target_dir, &query_terms);

    if options.json {
        println!("{}", render_query_json(&query_terms, &candidates));
    } else {
        print_query_candidates(&query_terms, &candidates);
    }
    Ok(())
}

fn run_graph(args: &[String]) -> Result<()> {
    if args.is_empty() {
        return Err("graph requires <doc_path>".into());
    }
    let doc_path = args[0].clone();
    let context = load_engine_context(&args[1..], 0)?;
    let options = command_options(args, Some(&context.schema));
    refresh_fact_store(&context)?;
    let db = open_fact_store(&context.target_dir)?;
    let packet = context_packet_for_doc(&db, &context.target_dir, &doc_path)?;

    if options.json {
        println!(
            "{}",
            render_context_packet_json("graph", &doc_path, &packet)
        );
    } else {
        print_context_packet("Graph", &doc_path, &packet);
    }
    Ok(())
}

fn run_context(args: &[String]) -> Result<()> {
    if args.is_empty() {
        return Err("context requires <terms-or-doc-path>".into());
    }
    let input = args[0].clone();
    let context = load_engine_context(&args[1..], 0)?;
    let options = command_options(args, Some(&context.schema));
    refresh_fact_store(&context)?;
    let db = open_fact_store(&context.target_dir)?;
    let anchor_path = if doc_exists(&db, &input)? {
        input.clone()
    } else {
        let rows = query_candidates(&db, &context.target_dir, &input)?;
        let candidates = query_rows_to_candidates(&rows, &context.target_dir, &input);
        candidates
            .first()
            .map(|candidate| candidate.path.clone())
            .ok_or_else(|| format!("no context candidate found for query: {}", input))?
    };
    let packet = context_packet_for_doc(&db, &context.target_dir, &anchor_path)?;

    if options.json {
        println!("{}", render_context_packet_json("context", &input, &packet));
    } else {
        print_context_packet("Context", &input, &packet);
    }
    Ok(())
}

fn run_history(args: &[String]) -> Result<()> {
    if args.is_empty() {
        return Err("history requires a subcommand: snapshot, list, status, diff, restore".into());
    }

    match args[0].as_str() {
        "snapshot" => run_history_snapshot(&args[1..]),
        "list" => run_history_list(&args[1..]),
        "status" => run_history_status(&args[1..]),
        "diff" => run_history_diff(&args[1..]),
        "restore" => run_history_restore(&args[1..]),
        other => Err(format!("unknown history subcommand: {}", other).into()),
    }
}

fn run_history_snapshot(args: &[String]) -> Result<()> {
    let message = option_value(args, "-m")
        .or_else(|| option_value(args, "--message"))
        .unwrap_or_else(|| "snapshot".to_string());
    let context = load_engine_context(args, 0)?;
    let snapshot = create_history_snapshot(&context.target_dir, &context.schema, &message)?;
    println!(
        "Created HarnessKit history snapshot {} (files {}, message: {})",
        snapshot.id,
        snapshot.files.len(),
        snapshot.message
    );
    Ok(())
}

fn run_history_list(args: &[String]) -> Result<()> {
    let context = load_engine_context(args, 0)?;
    let snapshots = list_history_snapshots(&context.target_dir)?;
    if snapshots.is_empty() {
        println!("No HarnessKit history snapshots found.");
        return Ok(());
    }

    println!("HarnessKit history snapshots:");
    for snapshot in snapshots {
        println!(
            "- {} files={} created_at={} message={}",
            snapshot.id,
            snapshot.files.len(),
            snapshot.created_at_unix,
            snapshot.message
        );
    }
    Ok(())
}

fn run_history_status(args: &[String]) -> Result<()> {
    let context = load_engine_context(args, 0)?;
    let status = compute_history_status(&context.target_dir, &context.schema)?;
    print_history_status(&status);
    Ok(())
}

fn run_history_diff(args: &[String]) -> Result<()> {
    if args.is_empty() {
        return Err("history diff requires <snapshot-id|latest> or <a> <b>".into());
    }
    let positionals = positional_args(args);
    if positionals.is_empty() {
        return Err("history diff requires <snapshot-id|latest> or <a> <b>".into());
    }
    let snapshot_args = history_diff_snapshot_args(&positionals);
    let context = load_engine_context(args, snapshot_args.len())?;
    let first = resolve_snapshot_arg(&context.target_dir, &positionals[0])?;
    let second = if snapshot_args.len() >= 2 {
        Some(resolve_snapshot_arg(
            &context.target_dir,
            &snapshot_args[1],
        )?)
    } else {
        None
    };

    let diff = if let Some(second) = second {
        diff_snapshots(&first, &second)
    } else {
        diff_snapshot_to_worktree(&context.target_dir, &context.schema, &first)?
    };
    print_history_diff(&diff);
    Ok(())
}

fn history_diff_snapshot_args(positionals: &[String]) -> Vec<String> {
    if positionals.len() <= 1 {
        return positionals.to_vec();
    }
    let second = Path::new(&positionals[1]);
    if second.exists() && second.is_dir() {
        vec![positionals[0].clone()]
    } else {
        vec![positionals[0].clone(), positionals[1].clone()]
    }
}

fn run_history_restore(args: &[String]) -> Result<()> {
    if args.is_empty() {
        return Err("history restore requires <snapshot-id|latest>".into());
    }
    let apply = has_flag(args, "--apply");
    let force = has_flag(args, "--force");
    let positionals = positional_args(args);
    if positionals.is_empty() {
        return Err("history restore requires <snapshot-id|latest>".into());
    }
    let context = load_engine_context(args, 1)?;
    let snapshot = resolve_snapshot_arg(&context.target_dir, &positionals[0])?;
    let dirty = compute_history_status(&context.target_dir, &context.schema)?;

    if apply && is_history_dirty(&dirty) && !force {
        return Err(
            "current docs differ from latest snapshot; run history snapshot first or use --force"
                .into(),
        );
    }

    let plan = restore_plan(&context.target_dir, &context.schema, &snapshot)?;
    print_restore_plan(&snapshot.id, &plan, apply);
    if apply {
        apply_restore_plan(&context.target_dir, &plan)?;
    }
    Ok(())
}

fn harnesskit_dir(target_dir: &Path) -> PathBuf {
    target_dir.join(".harnesskit")
}

fn history_dir(target_dir: &Path) -> PathBuf {
    harnesskit_dir(target_dir).join("history")
}

fn history_objects_dir(target_dir: &Path) -> PathBuf {
    history_dir(target_dir).join("objects")
}

fn history_snapshots_dir(target_dir: &Path) -> PathBuf {
    history_dir(target_dir).join("snapshots")
}

fn history_refs_dir(target_dir: &Path) -> PathBuf {
    history_dir(target_dir).join("refs")
}

fn ensure_history_dirs(target_dir: &Path) -> Result<()> {
    fs::create_dir_all(history_objects_dir(target_dir))?;
    fs::create_dir_all(history_snapshots_dir(target_dir))?;
    fs::create_dir_all(history_refs_dir(target_dir))?;
    Ok(())
}

fn managed_memory_paths(schema: &Schema) -> Vec<String> {
    let mut paths = Vec::new();
    push_unique_string(
        &mut paths,
        format!("{}/", schema.managed_root.trim_end_matches('/')),
    );
    if let Some(path) = &schema.entrypoints.agents {
        push_unique_string(&mut paths, path.clone());
    }
    if let Some(path) = &schema.entrypoints.claude {
        push_unique_string(&mut paths, path.clone());
    }
    if let Some(path) = &schema.entrypoints.architecture {
        push_unique_string(&mut paths, path.clone());
    }
    push_unique_string(&mut paths, ".harnesskit/".to_string());
    paths
}

fn push_unique_string(values: &mut Vec<String>, value: String) {
    if !value.is_empty() && !values.contains(&value) {
        values.push(value);
    }
}

fn init_mode_label(mode: InitMode) -> &'static str {
    match mode {
        InitMode::Local => "local",
        InitMode::Tracked => "tracked",
    }
}

fn init_plan_paths(schema: &Schema) -> Vec<String> {
    let mut paths = Vec::new();
    if let Some(path) = &schema.entrypoints.agents {
        push_unique_string(&mut paths, path.clone());
    }
    if let Some(path) = &schema.entrypoints.claude {
        push_unique_string(&mut paths, path.clone());
    }
    if let Some(path) = &schema.entrypoints.architecture {
        push_unique_string(&mut paths, path.clone());
    }
    for spec in &schema.core_files {
        if let Some(root_path) = &spec.root_path {
            push_unique_string(&mut paths, root_path.clone());
        } else {
            push_unique_string(
                &mut paths,
                format!(
                    "{}/{}",
                    schema.managed_root.trim_end_matches('/'),
                    spec.path.trim_start_matches('/')
                ),
            );
        }
    }
    for spec in &schema.doc_collections {
        push_unique_string(
            &mut paths,
            format!(
                "{}/{}/index.md",
                schema.managed_root.trim_end_matches('/'),
                spec.path.trim_matches('/')
            ),
        );
    }
    push_unique_string(
        &mut paths,
        format!("{}/templates/", schema.managed_root.trim_end_matches('/')),
    );
    push_unique_string(&mut paths, ".harnesskit/schema.yaml".to_string());
    push_unique_string(&mut paths, ".harnesskit/state/".to_string());
    push_unique_string(&mut paths, ".harnesskit/history/".to_string());
    paths
}

fn print_init_preview(
    target_dir: &Path,
    schema: &Schema,
    schema_path: &Path,
    mode: InitMode,
    force: bool,
) {
    println!("HarnessKit init preview");
    println!("Target: {}", target_dir.display());
    println!("Schema: {}", schema_path.display());
    println!("Mode: {}", init_mode_label(mode));
    println!("Docs root: {}", schema.managed_root);
    println!("Force overwrite: {}", yes_no_bool(force));
    println!();
    println!("Would create or update:");
    for path in init_plan_paths(schema) {
        let abs = target_dir.join(&path);
        let status = if abs.exists() {
            "skip existing"
        } else {
            "create"
        };
        println!("- {} ({})", path, status);
    }
    println!();
    match mode {
        InitMode::Local => {
            println!("Git exclude: would add HarnessKit-managed paths to .git/info/exclude when a git repo is present.");
            println!("Local mode keeps generated docs out of git status. Use --tracked for team-visible repo memory.");
        }
        InitMode::Tracked => {
            println!("Git exclude: would not write HarnessKit-managed paths to .git/info/exclude.");
            println!("Tracked mode leaves generated docs visible to git status so they can be committed.");
        }
    }
    println!();
    println!("Preview only: no files were written.");
}

fn print_init_summary(
    target_dir: &Path,
    schema: &Schema,
    mode: InitMode,
    exclude_stats: Option<(usize, usize)>,
) {
    println!("Mode: {}", init_mode_label(mode));
    println!("Docs root: {}", schema.managed_root);
    println!("Entrypoints:");
    if let Some(path) = &schema.entrypoints.agents {
        println!("- {}", path);
    }
    if let Some(path) = &schema.entrypoints.claude {
        println!("- {}", path);
    }
    if let Some(path) = &schema.entrypoints.architecture {
        println!("- {}", path);
    }
    println!(
        ".harnesskit/state: derived fact/index layer; safe to rebuild with `harnesskit index`."
    );
    println!(".harnesskit/history: local doc checkpoints; do not delete as cache.");

    match mode {
        InitMode::Local => match exclude_stats {
            Some((added, existing)) => {
                println!(
                    "Git visibility: local mode updated .git/info/exclude (added {}, already present {}).",
                    added, existing
                );
                println!("Generated docs are local project memory and will not appear in git status. Use `harnesskit init --tracked` for team-visible docs.");
            }
            None => println!("Git visibility: local mode selected, but no host git repo was found; skipped .git/info/exclude update."),
        },
        InitMode::Tracked => {
            println!("Git visibility: tracked mode selected; .git/info/exclude was not changed.");
            println!("Generated docs will appear in git status and can be committed as team-visible context harness files.");
        }
    }

    println!("Next steps:");
    println!("- Agent-first: ask your agent to read `AGENTS.md` or `CLAUDE.md` and follow the reading path.");
    println!("- Manual: `harnesskit index {}`", target_dir.display());
    println!("- Manual: `harnesskit doctor {}`", target_dir.display());
    println!(
        "- Manual: `harnesskit check {} --json`",
        target_dir.display()
    );
}

fn ensure_host_git_exclude(target_dir: &Path, schema: &Schema) -> Result<Option<(usize, usize)>> {
    let Some(git_dir) = find_host_git_dir(target_dir) else {
        return Ok(None);
    };
    let info_dir = git_dir.join("info");
    let exclude_path = info_dir.join("exclude");
    fs::create_dir_all(&info_dir)?;
    let existing = fs::read_to_string(&exclude_path).unwrap_or_default();
    let mut lines = existing
        .lines()
        .map(|line| line.to_string())
        .collect::<Vec<_>>();
    let mut added = 0usize;
    let mut already = 0usize;
    for path in managed_memory_paths(schema) {
        if lines.iter().any(|line| line.trim() == path) {
            already += 1;
        } else {
            lines.push(path);
            added += 1;
        }
    }
    if added > 0 {
        fs::write(&exclude_path, lines.join("\n") + "\n")?;
    }
    Ok(Some((added, already)))
}

fn find_host_git_dir(target_dir: &Path) -> Option<PathBuf> {
    let mut current = target_dir
        .canonicalize()
        .unwrap_or_else(|_| target_dir.to_path_buf());
    loop {
        let dot_git = current.join(".git");
        if dot_git.is_dir() {
            return Some(dot_git);
        }
        if !current.pop() {
            break;
        }
    }
    None
}

fn host_git_info(target_dir: &Path, mode: HostGitMode) -> HostGitInfo {
    let present = find_host_git_dir(target_dir).is_some();
    if !present {
        return HostGitInfo::default();
    }
    let head = run_git_capture(target_dir, &["rev-parse", "HEAD"]).ok();
    let branch = run_git_capture(target_dir, &["branch", "--show-current"])
        .ok()
        .filter(|value| !value.is_empty());
    let dirty = match mode {
        HostGitMode::Full => run_git_capture(target_dir, &["status", "--porcelain"])
            .map(|value| !value.trim().is_empty())
            .unwrap_or(false),
    };
    HostGitInfo {
        head,
        branch,
        dirty,
    }
}

fn file_modified_parts(metadata: &fs::Metadata) -> Result<(u64, u64)> {
    let modified = metadata.modified()?.duration_since(UNIX_EPOCH)?;
    let nanos = modified.as_nanos().min(u128::from(u64::MAX)) as u64;
    Ok((modified.as_secs(), nanos))
}

fn run_git_capture(target_dir: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(target_dir)
        .output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if stderr.is_empty() {
            "git command failed".to_string()
        } else {
            stderr
        }
        .into());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn create_history_snapshot(
    target_dir: &Path,
    schema: &Schema,
    message: &str,
) -> Result<HistorySnapshot> {
    ensure_history_dirs(target_dir)?;
    let created_at_unix = now_unix();
    let id = unique_snapshot_id(created_at_unix);
    let schema_hash = schema_file_hash(target_dir);
    let files = collect_history_file_records(target_dir, schema)?;
    for file in &files {
        store_history_object(target_dir, &file.path, &file.hash)?;
    }
    let git = host_git_info(target_dir, HostGitMode::Full);
    let snapshot = HistorySnapshot {
        id,
        created_at_unix,
        message: message.to_string(),
        docs_root: schema.managed_root.clone(),
        schema_hash,
        files,
        host_git_head: git.head,
        host_git_branch: git.branch,
        host_git_dirty: git.dirty,
    };
    write_history_snapshot(target_dir, &snapshot)?;
    fs::write(history_refs_dir(target_dir).join("latest"), &snapshot.id)?;
    Ok(snapshot)
}

fn unique_snapshot_id(created_at_unix: u64) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.subsec_nanos())
        .unwrap_or(0);
    let counter = SNAPSHOT_COUNTER.fetch_add(1, Ordering::Relaxed) % 1000;
    format!("{}-{:09}-{:03}", created_at_unix, nanos, counter)
}

fn schema_file_hash(target_dir: &Path) -> String {
    fs::read(target_dir.join(".harnesskit").join("schema.yaml"))
        .map(|bytes| history_content_hash(&bytes))
        .unwrap_or_default()
}

fn collect_history_file_records(
    target_dir: &Path,
    schema: &Schema,
) -> Result<Vec<HistoryFileRecord>> {
    let mut paths = collect_managed_doc_paths(target_dir, schema)?;
    paths.push(".harnesskit/schema.yaml".to_string());
    paths.sort();
    paths.dedup();

    let mut records = Vec::new();
    for path in paths {
        let abs = target_dir.join(&path);
        if !abs.is_file() {
            continue;
        }
        let bytes = fs::read(&abs)?;
        let metadata = fs::metadata(&abs)?;
        let (mtime_unix, _) = file_modified_parts(&metadata)?;
        records.push(HistoryFileRecord {
            path,
            hash: history_content_hash(&bytes),
            size: metadata.len(),
            mtime_unix,
        });
    }
    Ok(records)
}

fn store_history_object(target_dir: &Path, rel_path: &str, hash: &str) -> Result<()> {
    let object_path = history_object_path(target_dir, hash);
    if object_path.exists() {
        return Ok(());
    }
    if let Some(parent) = object_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let content = fs::read(target_dir.join(rel_path))?;
    fs::write(&object_path, content).map_err(|err| {
        format!(
            "failed to store history object for {} at {}: {}",
            rel_path,
            object_path.display(),
            err
        )
    })?;
    Ok(())
}

fn history_object_path(target_dir: &Path, hash: &str) -> PathBuf {
    let prefix_len = hash.len().min(2);
    let (prefix, suffix) = hash.split_at(prefix_len);
    history_objects_dir(target_dir).join(prefix).join(suffix)
}

fn write_history_snapshot(target_dir: &Path, snapshot: &HistorySnapshot) -> Result<()> {
    let path = history_snapshots_dir(target_dir).join(format!("{}.json", snapshot.id));
    fs::write(path, render_history_snapshot_json(snapshot))?;
    Ok(())
}

fn render_history_snapshot_json(snapshot: &HistorySnapshot) -> String {
    let mut out = String::new();
    out.push_str("{\n");
    out.push_str(&format!("  \"id\": {},\n", json_string(&snapshot.id)));
    out.push_str(&format!(
        "  \"created_at_unix\": {},\n",
        snapshot.created_at_unix
    ));
    out.push_str(&format!(
        "  \"message\": {},\n",
        json_string(&snapshot.message)
    ));
    out.push_str(&format!(
        "  \"docs_root\": {},\n",
        json_string(&snapshot.docs_root)
    ));
    out.push_str(&format!(
        "  \"schema_hash\": {},\n",
        json_string(&snapshot.schema_hash)
    ));
    out.push_str(&format!(
        "  \"host_git_head\": {},\n",
        json_opt_string(snapshot.host_git_head.as_deref())
    ));
    out.push_str(&format!(
        "  \"host_git_branch\": {},\n",
        json_opt_string(snapshot.host_git_branch.as_deref())
    ));
    out.push_str(&format!(
        "  \"host_git_dirty\": {},\n",
        json_bool(snapshot.host_git_dirty)
    ));
    out.push_str("  \"files\": [\n");
    for (idx, file) in snapshot.files.iter().enumerate() {
        out.push_str("    {\n");
        out.push_str(&format!("      \"path\": {},\n", json_string(&file.path)));
        out.push_str(&format!("      \"hash\": {},\n", json_string(&file.hash)));
        out.push_str(&format!("      \"size\": {},\n", file.size));
        out.push_str(&format!("      \"mtime_unix\": {}\n", file.mtime_unix));
        out.push_str("    }");
        if idx + 1 != snapshot.files.len() {
            out.push(',');
        }
        out.push('\n');
    }
    out.push_str("  ]\n}\n");
    out
}

#[derive(Default)]
struct JsonCursor {
    pos: usize,
}

fn sanitize_fts_query(query: &str) -> String {
    let tokens = query
        .split(|ch: char| !ch.is_alphanumeric())
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .collect::<Vec<_>>();
    if tokens.is_empty() {
        String::new()
    } else {
        tokens.join(" ")
    }
}

fn query_candidates(
    db: &SqliteConnection,
    target_dir: &Path,
    query_terms: &str,
) -> Result<QueryResult> {
    let escaped_terms = sql_string_literal(query_terms);
    let sanitized_fts_terms = sanitize_fts_query(query_terms);
    let escaped_fts_terms = sql_string_literal(&sanitized_fts_terms);
    let fts_match_clause = if sanitized_fts_terms.is_empty() {
        "0".to_string()
    } else {
        format!("docs_fts MATCH {}", escaped_fts_terms)
    };
    db.query(&format!(
        "WITH lexical AS (
            SELECT d.path,
                   d.title,
                   d.role,
                   COALESCE(d.status, '') AS status,
                   d.summary,
                   d.incoming_links,
                   CASE
                       WHEN lower(d.title) LIKE '%' || lower({0}) || '%' THEN 4.0
                       WHEN lower(d.path) LIKE '%' || lower({0}) || '%' THEN 3.0
                       WHEN lower(d.headings_text) LIKE '%' || lower({0}) || '%' THEN 2.0
                       WHEN lower(d.summary) LIKE '%' || lower({0}) || '%' THEN 1.5
                       ELSE 0.0
                   END AS lexical_score,
                   CASE
                       WHEN lower(d.title) LIKE '%' || lower({0}) || '%' THEN 'title matched query'
                       WHEN lower(d.path) LIKE '%' || lower({0}) || '%' THEN 'path matched query'
                       WHEN lower(d.headings_text) LIKE '%' || lower({0}) || '%' THEN 'heading matched query'
                       WHEN lower(d.summary) LIKE '%' || lower({0}) || '%' THEN 'summary matched query'
                       ELSE ''
                   END AS why
            FROM docs d
            WHERE lower(d.path) LIKE '%' || lower({0}) || '%'
               OR lower(d.title) LIKE '%' || lower({0}) || '%'
               OR lower(d.headings_text) LIKE '%' || lower({0}) || '%'
               OR lower(d.summary) LIKE '%' || lower({0}) || '%'
        ),
        fts AS (
            SELECT d.path,
                   d.title,
                   d.role,
                   COALESCE(d.status, '') AS status,
                   d.summary,
                   d.incoming_links,
                   2.5 AS lexical_score,
                   'full-text matched query' AS why
            FROM docs_fts f
            JOIN docs d ON d.path = f.path
            WHERE {1}
        ),
        base AS (
            SELECT * FROM lexical
            UNION ALL
            SELECT * FROM fts
        ),
        merged AS (
            SELECT path,
                   max(title) AS title,
                   max(role) AS role,
                   max(status) AS status,
                   max(summary) AS summary,
                   max(incoming_links) AS incoming_links,
                   max(lexical_score) AS lexical_score,
                   group_concat(DISTINCT why) AS why
            FROM base
            GROUP BY path
        ),
        ranked AS (
            SELECT path,
                   title,
                   role,
                   status,
                   summary,
                   incoming_links,
                   lexical_score,
                   why,
                   (lexical_score + (incoming_links * 0.05)) AS score
            FROM merged
            ORDER BY score DESC, path ASC
            LIMIT 12
        ),
        expanded AS (
            SELECT rnk.path AS anchor_path,
                   r.dst_path AS neighbor_path
            FROM ranked rnk
            JOIN relations r ON r.src_path = rnk.path
            WHERE r.dst_path LIKE '%.md'
            UNION
            SELECT rnk.path AS anchor_path,
                   r.src_path AS neighbor_path
            FROM ranked rnk
            JOIN relations r ON r.dst_path = rnk.path
            WHERE r.src_path LIKE '%.md'
            UNION
            SELECT rnk.path AS anchor_path,
                   sibling.dst_path AS neighbor_path
            FROM ranked rnk
            JOIN relations parent ON parent.dst_path = rnk.path AND parent.relation_type = 'doc_indexes_doc'
            JOIN relations sibling ON sibling.src_path = parent.src_path AND sibling.relation_type = 'doc_indexes_doc'
            WHERE sibling.dst_path LIKE '%.md' AND sibling.dst_path != rnk.path
            UNION
            SELECT rnk.path AS anchor_path,
                   scoped.doc_path AS neighbor_path
            FROM ranked rnk
            JOIN scope_paths base_scope ON base_scope.doc_path = rnk.path
            JOIN scope_paths scoped ON scoped.scope_path = base_scope.scope_path AND scoped.doc_path != rnk.path
        ),
        neighbor_summary AS (
            SELECT anchor_path,
                   group_concat(neighbor_path, ' | ') AS neighbors
            FROM (
                SELECT anchor_path, neighbor_path
                FROM (
                    SELECT anchor_path,
                           neighbor_path,
                           row_number() OVER (PARTITION BY anchor_path ORDER BY neighbor_path) AS rn
                    FROM expanded
                )
                WHERE rn <= 8
                ORDER BY anchor_path, neighbor_path
            )
            GROUP BY anchor_path
        )
        SELECT rnk.path,
               rnk.title,
               rnk.role,
               rnk.status,
               printf('%.2f', rnk.score) AS score,
               rnk.why,
               rnk.summary,
               COALESCE(n.neighbors, '')
        FROM ranked rnk
        LEFT JOIN neighbor_summary n ON n.anchor_path = rnk.path
        ORDER BY rnk.score DESC, rnk.path ASC;",
        escaped_terms,
        fts_match_clause
    ))
    .map(|mut result| {
        for row in &mut result.rows {
            let path = cell(row, 0).to_string();
            let (heading, location, snippet) = snippet_for_doc(target_dir, &path, query_terms);
            row.push(Some(heading));
            row.push(Some(location));
            row.push(Some(snippet));
        }
        result
    })
}

fn query_rows_to_candidates(
    result: &QueryResult,
    target_dir: &Path,
    query_terms: &str,
) -> Vec<QueryCandidate> {
    result
        .rows
        .iter()
        .map(|row| {
            let path = cell(row, 0).to_string();
            let mut fallback = None;
            let snippet_heading = cell(row, 8).to_string();
            let snippet_location = cell(row, 9).to_string();
            let snippet = cell(row, 10).to_string();
            let (snippet_heading, snippet_location, snippet) = if snippet_heading.is_empty()
                || snippet_location.is_empty()
                || snippet.is_empty()
            {
                let computed =
                    fallback.get_or_insert_with(|| snippet_for_doc(target_dir, &path, query_terms));
                (
                    if snippet_heading.is_empty() {
                        computed.0.clone()
                    } else {
                        snippet_heading
                    },
                    if snippet_location.is_empty() {
                        computed.1.clone()
                    } else {
                        snippet_location
                    },
                    if snippet.is_empty() {
                        computed.2.clone()
                    } else {
                        snippet
                    },
                )
            } else {
                (snippet_heading, snippet_location, snippet)
            };
            QueryCandidate {
                path,
                title: cell(row, 1).to_string(),
                role: cell(row, 2).to_string(),
                status: cell(row, 3).to_string(),
                score: cell(row, 4).to_string(),
                why: cell(row, 5).to_string(),
                summary: cell(row, 6).to_string(),
                neighbors: split_pipe_list(cell(row, 7)),
                snippet_heading,
                snippet_location,
                snippet,
            }
        })
        .collect()
}

fn snippet_for_doc(
    target_dir: &Path,
    doc_path: &str,
    query_terms: &str,
) -> (String, String, String) {
    let text = fs::read_to_string(target_dir.join(doc_path)).unwrap_or_default();
    if text.is_empty() {
        return (String::new(), String::new(), String::new());
    }
    let terms = sanitize_fts_query(query_terms)
        .split_whitespace()
        .map(|term| term.to_ascii_lowercase())
        .collect::<Vec<_>>();
    let mut current_heading = String::new();
    let mut fallback = String::new();
    let mut fallback_line = 0usize;
    let mut current_heading_line = 0usize;
    for (index, line) in text.lines().enumerate() {
        let line_number = index + 1;
        if let Some((_level, heading)) = parse_heading(line) {
            current_heading = heading;
            current_heading_line = line_number;
            continue;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("---") {
            continue;
        }
        if fallback.is_empty() && !trimmed.starts_with('#') {
            fallback = trimmed.to_string();
            fallback_line = line_number;
        }
        let lower = trimmed.to_ascii_lowercase();
        if terms.iter().any(|term| lower.contains(term)) {
            return (
                current_heading,
                format!("line {}", line_number),
                truncate_snippet(trimmed, 220),
            );
        }
    }
    let location = if fallback_line > 0 {
        format!("line {}", fallback_line)
    } else if current_heading_line > 0 {
        format!("line {}", current_heading_line)
    } else {
        String::new()
    };
    (current_heading, location, truncate_snippet(&fallback, 220))
}

fn truncate_snippet(value: &str, max_chars: usize) -> String {
    let mut out = String::new();
    for ch in value.chars().take(max_chars) {
        out.push(ch);
    }
    if value.chars().count() > max_chars {
        out.push_str("...");
    }
    out
}

fn doc_exists(db: &SqliteConnection, doc_path: &str) -> Result<bool> {
    Ok(scalar_count(
        db,
        &format!(
            "SELECT COUNT(*) FROM docs WHERE path = {};",
            sql_string_literal(doc_path)
        ),
    )? > 0)
}

fn context_packet_for_doc(
    db: &SqliteConnection,
    target_dir: &Path,
    doc_path: &str,
) -> Result<ContextPacket> {
    let doc = db.query(&format!(
        "SELECT path, title, role, COALESCE(status, ''), summary, incoming_links
         FROM docs WHERE path = {};",
        sql_string_literal(doc_path)
    ))?;
    let Some(row) = doc.rows.first() else {
        return Err(format!("doc not found in fact store: {}", doc_path).into());
    };
    let snippet = snippet_for_doc(target_dir, doc_path, doc_path);
    let anchor = QueryCandidate {
        path: cell(row, 0).to_string(),
        title: cell(row, 1).to_string(),
        role: cell(row, 2).to_string(),
        status: cell(row, 3).to_string(),
        score: format!("{:.2}", cell(row, 5).parse::<f32>().unwrap_or(0.0) * 0.05),
        why: "context anchor".to_string(),
        summary: cell(row, 4).to_string(),
        neighbors: Vec::new(),
        snippet_heading: snippet.0,
        snippet_location: snippet.1,
        snippet: snippet.2,
    };

    let outgoing = relation_rows(db, "src_path", doc_path)?;
    let incoming = relation_rows(db, "dst_path", doc_path)?;
    let collection_siblings = collection_siblings(db, doc_path)?;
    let same_scope_docs = same_scope_docs(db, doc_path)?;
    let recommended_reading_order = recommended_reading_order(
        &anchor,
        &incoming,
        &outgoing,
        &collection_siblings,
        &same_scope_docs,
    );

    Ok(ContextPacket {
        anchor,
        incoming,
        outgoing,
        collection_siblings,
        same_scope_docs,
        recommended_reading_order,
    })
}

fn relation_rows(db: &SqliteConnection, side: &str, doc_path: &str) -> Result<Vec<DocRelation>> {
    let sql = format!(
        "SELECT src_path, dst_path, relation_type, reason, explicit, confidence, evidence_json
         FROM relations
         WHERE {side} = {}
         ORDER BY relation_type ASC, src_path ASC, dst_path ASC;",
        sql_string_literal(doc_path),
        side = side
    );
    let rows = db.query(&sql)?;
    Ok(rows
        .rows
        .iter()
        .map(|row| DocRelation {
            src_path: cell(row, 0).to_string(),
            dst_path: cell(row, 1).to_string(),
            relation_type: cell(row, 2).to_string(),
            reason: cell(row, 3).to_string(),
            explicit: cell(row, 4) == "1",
            confidence: cell(row, 5).parse::<f32>().unwrap_or(0.0),
            evidence_json: cell(row, 6).to_string(),
        })
        .collect())
}

fn collection_siblings(db: &SqliteConnection, doc_path: &str) -> Result<Vec<String>> {
    let rows = db.query(&format!(
        "SELECT sibling.path
         FROM docs target
         JOIN docs sibling ON sibling.collection_name = target.collection_name
         WHERE target.path = {}
           AND sibling.path != target.path
           AND sibling.path NOT LIKE '%/index.md'
         ORDER BY sibling.path ASC
         LIMIT 12;",
        sql_string_literal(doc_path)
    ))?;
    Ok(rows
        .rows
        .iter()
        .map(|row| cell(row, 0).to_string())
        .collect())
}

fn same_scope_docs(db: &SqliteConnection, doc_path: &str) -> Result<Vec<String>> {
    let rows = db.query(&format!(
        "SELECT DISTINCT scoped.doc_path
         FROM scope_paths base
         JOIN scope_paths scoped ON scoped.scope_path = base.scope_path
         WHERE base.doc_path = {}
           AND scoped.doc_path != base.doc_path
         ORDER BY scoped.doc_path ASC
         LIMIT 12;",
        sql_string_literal(doc_path)
    ))?;
    Ok(rows
        .rows
        .iter()
        .map(|row| cell(row, 0).to_string())
        .collect())
}

fn recommended_reading_order(
    anchor: &QueryCandidate,
    incoming: &[DocRelation],
    outgoing: &[DocRelation],
    collection_siblings: &[String],
    same_scope_docs: &[String],
) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut order = Vec::new();
    push_unique_path(&mut order, &mut seen, &anchor.path);
    for relation in incoming
        .iter()
        .filter(|relation| relation.relation_type == "doc_indexes_doc")
    {
        push_unique_path(&mut order, &mut seen, &relation.src_path);
    }
    for relation in outgoing
        .iter()
        .filter(|relation| relation.dst_path.ends_with(".md"))
    {
        push_unique_path(&mut order, &mut seen, &relation.dst_path);
    }
    for path in same_scope_docs {
        push_unique_path(&mut order, &mut seen, path);
    }
    for path in collection_siblings {
        push_unique_path(&mut order, &mut seen, path);
    }
    order.truncate(12);
    order
}

fn push_unique_path(out: &mut Vec<String>, seen: &mut BTreeSet<String>, path: &str) {
    if !path.is_empty() && seen.insert(path.to_string()) {
        out.push(path.to_string());
    }
}

fn positional_or_default(args: &[String], index: usize, default: &str) -> String {
    positional_args(args)
        .get(index)
        .cloned()
        .unwrap_or_else(|| default.to_string())
}

fn positional_args(args: &[String]) -> Vec<String> {
    let mut values = Vec::new();
    let mut skip_next = false;
    for arg in args {
        if skip_next {
            skip_next = false;
            continue;
        }
        match arg.as_str() {
            "--schema" | "--docs-root" | "--rules" | "--message" | "-m" | "--version"
            | "--repo" | "--asset-base-url" => {
                skip_next = true;
            }
            "--force"
            | "--json"
            | "--strict"
            | "--no-strict"
            | "--apply"
            | "--cli-only"
            | "--no-claude"
            | "--no-codex"
            | "--with-codex-plugin"
            | "--local"
            | "--tracked"
            | "--preview" => {}
            _ if arg.starts_with("--") => {}
            _ if arg.starts_with('-') => {}
            _ => values.push(arg.clone()),
        }
    }
    values
}

fn has_flag(args: &[String], flag: &str) -> bool {
    args.iter().any(|arg| arg == flag)
}

fn command_options(args: &[String], schema: Option<&Schema>) -> CommandOptions {
    let schema_strict = schema
        .and_then(|schema| schema.rules.strict_checks)
        .unwrap_or(false);
    CommandOptions {
        json: has_flag(args, "--json"),
        strict: (has_flag(args, "--strict") || schema_strict) && !has_flag(args, "--no-strict"),
        rules: option_value(args, "--rules").map(|value| {
            value
                .split(',')
                .map(|item| item.trim())
                .filter(|item| !item.is_empty())
                .map(|item| item.to_string())
                .collect::<BTreeSet<_>>()
        }),
    }
}

fn option_value(args: &[String], flag: &str) -> Option<String> {
    let values = args
        .windows(2)
        .filter(|pair| pair[0] == flag)
        .map(|pair| pair[1].clone())
        .collect::<Vec<_>>();
    if values.len() > 1 {
        eprintln!("warning: duplicate {}; using last value", flag);
    }
    values.last().cloned()
}

fn ensure_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    Ok(())
}

fn ensure_writable_dir(path: &Path, purpose: &str) -> Result<()> {
    fs::create_dir_all(path).map_err(|err| {
        format!(
            "cannot create {} directory at {}: {}.",
            purpose,
            path.display(),
            err
        )
    })?;

    let probe = path.join(format!(
        ".harnesskit-write-test-{}-{}",
        std::process::id(),
        now_unix()
    ));
    match fs::write(&probe, b"ok") {
        Ok(()) => {
            let _ = fs::remove_file(&probe);
            Ok(())
        }
        Err(err) => Err(format!(
            "cannot write {} directory at {}: {}. Check directory permissions or run HarnessKit from a writable project path.",
            purpose,
            path.display(),
            err
        )
        .into()),
    }
}

fn ensure_writable_fact_store_dir(
    target_dir: &Path,
    state_dir: &Path,
    purpose: &str,
) -> Result<()> {
    ensure_writable_dir(state_dir, purpose).map_err(|err| {
        format!(
            "{}\n\n{}",
            err,
            fact_store_path_hint(target_dir, Some(state_dir), None)
        )
        .into()
    })
}

fn fact_store_path_hint(
    target_dir: &Path,
    state_dir: Option<&Path>,
    facts_path: Option<&Path>,
) -> String {
    let diagnostics = path_diagnostics(target_dir, state_dir, facts_path);
    let mut out = String::new();
    out.push_str("HarnessKit path diagnostics:\n");
    out.push_str(&format!("- target_dir: {}\n", diagnostics.target_dir));
    out.push_str(&format!("- current_dir: {}\n", diagnostics.current_dir));
    out.push_str(&format!(
        "- PWD: {}\n",
        diagnostics.pwd.as_deref().unwrap_or("(not set)")
    ));
    out.push_str(&format!("- state_dir: {}\n", diagnostics.state_dir));
    out.push_str(&format!("- facts_path: {}\n", diagnostics.facts_path));
    out.push_str("Hint: run `harnesskit doctor --json` to verify sqlite3, git, PATH, and HarnessKit state directories. If diagnostics do not point at the intended writable project root, pass the project root explicitly, for example `harnesskit check /path/to/repo --json` or `harnesskit index /path/to/repo`.\n");
    out.push_str("Do not delete `.harnesskit/history` unless you intentionally want to discard local doc checkpoints.");
    out
}

fn path_diagnostics(
    target_dir: &Path,
    state_dir: Option<&Path>,
    facts_path: Option<&Path>,
) -> PathDiagnostics {
    let state = state_dir
        .map(Path::to_path_buf)
        .unwrap_or_else(|| target_dir.join(".harnesskit").join("state"));
    let facts = facts_path
        .map(Path::to_path_buf)
        .unwrap_or_else(|| state.join("facts.sqlite"));
    PathDiagnostics {
        target_dir: absolute_display_path(target_dir),
        current_dir: env::current_dir()
            .map(|path| absolute_display_path(&path))
            .unwrap_or_else(|err| format!("(unavailable: {})", err)),
        pwd: env::var("PWD").ok(),
        state_dir: absolute_display_path(&state),
        facts_path: absolute_display_path(&facts),
    }
}

fn build_doctor_report(target_dir: &Path) -> DoctorReport {
    let current_dir = env::current_dir().ok();
    let current_dir_display = current_dir
        .as_ref()
        .map(|path| absolute_display_path(path))
        .unwrap_or_else(|| "(unavailable)".to_string());
    let pwd = env::var("PWD").ok();
    let pwd_differs_from_current_dir = pwd
        .as_ref()
        .map(|pwd| pwd != &current_dir_display)
        .unwrap_or(false);
    let sqlite3 = command_check("sqlite3", &["--version"]);
    let git = command_check("git", &["--version"]);
    let (inside_git_repo, git_repo_error) = git_repo_check(target_dir);
    let state_dir = target_dir.join(".harnesskit").join("state");
    let history_dir = target_dir.join(".harnesskit").join("history");
    let schema_path = target_dir.join(".harnesskit").join("schema.yaml");
    let (state_writable, state_error) = state_writable_check(target_dir, &state_dir);
    let home = env::var("HOME").unwrap_or_default();
    let claude_skill = PathBuf::from(&home)
        .join(".claude")
        .join("skills")
        .join("harnesskit")
        .join("SKILL.md");
    let codex_home = env::var("CODEX_HOME").unwrap_or_else(|_| {
        PathBuf::from(&home)
            .join(".codex")
            .to_string_lossy()
            .to_string()
    });
    let codex_skill = PathBuf::from(codex_home)
        .join("skills")
        .join("harnesskit")
        .join("SKILL.md");
    let current_exe = env::current_exe().ok();
    let path_contains_cli_dir = current_exe
        .as_ref()
        .and_then(|path| path.parent())
        .map(path_contains_dir)
        .unwrap_or(false);

    DoctorReport {
        cli_version: env!("CARGO_PKG_VERSION").to_string(),
        os: env::consts::OS.to_string(),
        arch: env::consts::ARCH.to_string(),
        target_dir: absolute_display_path(target_dir),
        current_dir: current_dir_display,
        pwd,
        pwd_differs_from_current_dir,
        sqlite3,
        git,
        inside_git_repo,
        git_repo_error,
        state_exists: state_dir.exists(),
        state_writable,
        state_error,
        history_exists: history_dir.exists(),
        schema_exists: schema_path.exists(),
        claude_skill_path: absolute_display_path(&claude_skill),
        claude_skill_installed: claude_skill.exists(),
        codex_skill_path: absolute_display_path(&codex_skill),
        codex_skill_installed: codex_skill.exists(),
        path_contains_cli_dir,
        current_exe: current_exe.as_ref().map(|path| absolute_display_path(path)),
    }
}

fn command_check(name: &str, args: &[&str]) -> CommandCheck {
    match Command::new(name).args(args).output() {
        Ok(output) if output.status.success() => {
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            CommandCheck {
                present: true,
                version: Some(if stdout.is_empty() { stderr } else { stdout }),
                error: None,
            }
        }
        Ok(output) => CommandCheck {
            present: true,
            version: None,
            error: Some(render_command_output_error(name, &output)),
        },
        Err(err) => CommandCheck {
            present: false,
            version: None,
            error: Some(err.to_string()),
        },
    }
}

fn render_command_output_error(name: &str, output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !stderr.is_empty() {
        format!("{} exited with {}: {}", name, output.status, stderr)
    } else if !stdout.is_empty() {
        format!("{} exited with {}: {}", name, output.status, stdout)
    } else {
        format!("{} exited with {}", name, output.status)
    }
}

fn git_repo_check(target_dir: &Path) -> (Option<bool>, Option<String>) {
    match Command::new("git")
        .arg("rev-parse")
        .arg("--is-inside-work-tree")
        .current_dir(target_dir)
        .output()
    {
        Ok(output) if output.status.success() => {
            let value = String::from_utf8_lossy(&output.stdout).trim() == "true";
            (Some(value), None)
        }
        Ok(output) => (
            Some(false),
            Some(render_command_output_error("git", &output)),
        ),
        Err(err) => (None, Some(err.to_string())),
    }
}

fn state_writable_check(target_dir: &Path, state_dir: &Path) -> (bool, Option<String>) {
    let harnesskit_dir = target_dir.join(".harnesskit");
    let probe_dir = if state_dir.exists() {
        state_dir.to_path_buf()
    } else if harnesskit_dir.exists() {
        harnesskit_dir
    } else if target_dir.exists() {
        target_dir.to_path_buf()
    } else {
        match nearest_existing_parent(target_dir) {
            Some(path) => path,
            None => {
                return (
                    false,
                    Some("target dir and parents do not exist".to_string()),
                )
            }
        }
    };
    let probe = probe_dir.join(format!(
        ".harnesskit-doctor-write-test-{}-{}",
        std::process::id(),
        now_unix()
    ));
    match fs::write(&probe, b"ok") {
        Ok(()) => {
            let _ = fs::remove_file(&probe);
            (true, None)
        }
        Err(err) => (
            false,
            Some(format!(
                "cannot write probe at {}: {}",
                probe.display(),
                err
            )),
        ),
    }
}

fn nearest_existing_parent(path: &Path) -> Option<PathBuf> {
    let mut current = path;
    loop {
        if current.exists() {
            return Some(current.to_path_buf());
        }
        current = current.parent()?;
    }
}

fn path_contains_dir(dir: &Path) -> bool {
    let Some(path_value) = env::var_os("PATH") else {
        return false;
    };
    env::split_paths(&path_value).any(|entry| entry == dir)
}

fn print_doctor_report(report: &DoctorReport) {
    println!("HarnessKit doctor");
    println!("CLI version: {}", report.cli_version);
    println!("Platform: {} {}", report.os, report.arch);
    println!("Target dir: {}", report.target_dir);
    println!("Current dir: {}", report.current_dir);
    println!("PWD: {}", report.pwd.as_deref().unwrap_or("(not set)"));
    println!(
        "PWD differs from current_dir: {}",
        yes_no_bool(report.pwd_differs_from_current_dir)
    );
    println!("sqlite3: {}", command_check_label(&report.sqlite3));
    println!("git: {}", command_check_label(&report.git));
    println!(
        "Inside git repo: {}",
        report.inside_git_repo.map(yes_no_bool).unwrap_or("unknown")
    );
    if let Some(error) = &report.git_repo_error {
        println!("Git repo check: {}", error);
    }
    println!(
        ".harnesskit/state exists: {}",
        yes_no_bool(report.state_exists)
    );
    println!(
        ".harnesskit/state writable or creatable: {}",
        yes_no_bool(report.state_writable)
    );
    if let Some(error) = &report.state_error {
        println!("State write check: {}", error);
        println!("Hint: if diagnostics do not point at the intended writable project root, pass the project root explicitly, for example `harnesskit doctor /path/to/repo --json` or `harnesskit index /path/to/repo`.");
    }
    println!(
        ".harnesskit/history exists: {}",
        yes_no_bool(report.history_exists)
    );
    println!(
        ".harnesskit/schema.yaml exists: {}",
        yes_no_bool(report.schema_exists)
    );
    println!(
        "Claude skill: {} ({})",
        installed_label(report.claude_skill_installed),
        report.claude_skill_path
    );
    println!(
        "Codex skill: {} ({})",
        installed_label(report.codex_skill_installed),
        report.codex_skill_path
    );
    println!(
        "Current executable: {}",
        report.current_exe.as_deref().unwrap_or("(unknown)")
    );
    println!(
        "Current executable directory on PATH: {}",
        yes_no_bool(report.path_contains_cli_dir)
    );
    if !report.sqlite3.present {
        println!("Hint: HarnessKit alpha requires sqlite3 CLI on PATH for fact-store-backed commands such as index, query, check, context, graph, inspect, refs, rank, and list-docs.");
    }
    println!("Note: `.harnesskit/state` is derived and rebuildable; `.harnesskit/history` stores local doc checkpoints.");
}

fn command_check_label(check: &CommandCheck) -> String {
    if check.present {
        match (&check.version, &check.error) {
            (Some(version), _) if !version.is_empty() => format!("ok ({})", version),
            (_, Some(error)) => format!("present but failed ({})", error),
            _ => "ok".to_string(),
        }
    } else {
        format!(
            "missing ({})",
            check.error.as_deref().unwrap_or("not found")
        )
    }
}

fn installed_label(installed: bool) -> &'static str {
    if installed {
        "installed"
    } else {
        "missing"
    }
}

fn render_doctor_json(report: &DoctorReport) -> String {
    let mut out = String::new();
    out.push_str("{\n");
    out.push_str(&format!(
        "  \"cli_version\": {},\n",
        json_string(&report.cli_version)
    ));
    out.push_str(&format!("  \"os\": {},\n", json_string(&report.os)));
    out.push_str(&format!("  \"arch\": {},\n", json_string(&report.arch)));
    out.push_str(&format!(
        "  \"target_dir\": {},\n",
        json_string(&report.target_dir)
    ));
    out.push_str(&format!(
        "  \"current_dir\": {},\n",
        json_string(&report.current_dir)
    ));
    out.push_str(&format!(
        "  \"pwd\": {},\n",
        json_opt_string(report.pwd.as_deref())
    ));
    out.push_str(&format!(
        "  \"pwd_differs_from_current_dir\": {},\n",
        json_bool(report.pwd_differs_from_current_dir)
    ));
    out.push_str(&format!(
        "  \"sqlite3\": {},\n",
        render_command_check_json(&report.sqlite3)
    ));
    out.push_str(&format!(
        "  \"git\": {},\n",
        render_command_check_json(&report.git)
    ));
    out.push_str(&format!(
        "  \"inside_git_repo\": {},\n",
        report.inside_git_repo.map(json_bool).unwrap_or("null")
    ));
    out.push_str(&format!(
        "  \"git_repo_error\": {},\n",
        json_opt_string(report.git_repo_error.as_deref())
    ));
    out.push_str(&format!(
        "  \"state_exists\": {},\n",
        json_bool(report.state_exists)
    ));
    out.push_str(&format!(
        "  \"state_writable\": {},\n",
        json_bool(report.state_writable)
    ));
    out.push_str(&format!(
        "  \"state_error\": {},\n",
        json_opt_string(report.state_error.as_deref())
    ));
    out.push_str(&format!(
        "  \"history_exists\": {},\n",
        json_bool(report.history_exists)
    ));
    out.push_str(&format!(
        "  \"schema_exists\": {},\n",
        json_bool(report.schema_exists)
    ));
    out.push_str(&format!(
        "  \"claude_skill_path\": {},\n",
        json_string(&report.claude_skill_path)
    ));
    out.push_str(&format!(
        "  \"claude_skill_installed\": {},\n",
        json_bool(report.claude_skill_installed)
    ));
    out.push_str(&format!(
        "  \"codex_skill_path\": {},\n",
        json_string(&report.codex_skill_path)
    ));
    out.push_str(&format!(
        "  \"codex_skill_installed\": {},\n",
        json_bool(report.codex_skill_installed)
    ));
    out.push_str(&format!(
        "  \"current_exe\": {},\n",
        json_opt_string(report.current_exe.as_deref())
    ));
    out.push_str(&format!(
        "  \"path_contains_cli_dir\": {},\n",
        json_bool(report.path_contains_cli_dir)
    ));
    out.push_str("  \"notes\": [\n");
    out.push_str(
        "    \"HarnessKit alpha requires sqlite3 CLI on PATH for fact-store-backed commands.\",\n",
    );
    out.push_str("    \".harnesskit/state is derived and rebuildable; .harnesskit/history stores local doc checkpoints.\",\n");
    out.push_str("    \"If diagnostics do not point at the intended writable project root, pass the project root explicitly.\"\n");
    out.push_str("  ]\n");
    out.push('}');
    out
}

fn render_command_check_json(check: &CommandCheck) -> String {
    format!(
        "{{\"present\":{},\"version\":{},\"error\":{}}}",
        json_bool(check.present),
        json_opt_string(check.version.as_deref()),
        json_opt_string(check.error.as_deref())
    )
}

fn load_engine_context(args: &[String], positional_index: usize) -> Result<EngineContext> {
    let target_dir = PathBuf::from(positional_or_default(args, positional_index, "."));
    let docs_root_override = option_value(args, "--docs-root");
    let schema_arg =
        option_value(args, "--schema").unwrap_or_else(|| default_context_schema_arg(&target_dir));

    let schema_path = resolve_schema_path(&target_dir, &schema_arg)?;
    let raw_schema_text = fs::read_to_string(&schema_path)?;
    let mut schema = parse_schema(&raw_schema_text)?;
    let original_managed_root = schema.managed_root.clone();
    if let Some(ref docs_root) = docs_root_override {
        schema.managed_root = docs_root.clone();
        rewrite_docs_root_bound_paths(&mut schema, &original_managed_root, docs_root);
    }

    Ok(EngineContext { target_dir, schema })
}

fn default_context_schema_arg(target_dir: &Path) -> String {
    let project_schema = target_dir.join(".harnesskit").join("schema.yaml");
    if project_schema.exists() {
        absolute_display_path(&project_schema)
    } else {
        BUNDLED_SCHEMA_PATH.to_string()
    }
}

fn resolve_schema_path(target_dir: &Path, schema_path: &str) -> Result<PathBuf> {
    let requested = PathBuf::from(schema_path);
    let candidates = if requested.is_absolute() {
        vec![requested]
    } else {
        vec![
            env::current_dir()?.join(schema_path),
            target_dir.join(schema_path),
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(schema_path),
        ]
    };

    for candidate in candidates {
        if candidate.exists() {
            return Ok(candidate);
        }
    }

    Err(format!("schema not found: {}", schema_path).into())
}

fn parse_schema(text: &str) -> Result<Schema> {
    let mut schema = Schema {
        schema_version: 0,
        managed_root: "docs".to_string(),
        ..Schema::default()
    };

    let mut section = Section::None;
    let mut current_core: Option<CoreFileSpec> = None;
    let mut current_collection: Option<DocCollectionSpec> = None;
    let mut current_suppression: Option<SuppressionSpec> = None;

    for raw_line in text.lines() {
        if raw_line.contains('\t') {
            return Err("invalid schema: tab indentation is not supported; use spaces".into());
        }
        let line_without_comment = raw_line.split('#').next().unwrap_or("").trim_end();
        if line_without_comment.trim().is_empty() {
            continue;
        }

        let indent = line_without_comment
            .chars()
            .take_while(|c| *c == ' ')
            .count();
        let trimmed = line_without_comment[indent..].trim();

        match indent {
            0 => {
                flush_core(&mut schema, &mut current_core);
                flush_collection(&mut schema, &mut current_collection);
                flush_suppression(&mut schema, &mut current_suppression);
                if trimmed.ends_with(':') {
                    section = match trimmed.trim_end_matches(':') {
                        "entrypoints" => Section::EntryPoints,
                        "core_files" => Section::CoreFiles,
                        "doc_collections" => Section::DocCollections,
                        "rules" => Section::Rules,
                        "suppressions" => Section::Suppressions,
                        other => {
                            return Err(
                                format!("invalid schema: unknown section `{}`", other).into()
                            )
                        }
                    };
                } else if let Some((key, value)) = parse_key_value(trimmed) {
                    match key.as_str() {
                        "schema_version" => schema.schema_version = parse_u32(&value)?,
                        "managed_root" => schema.managed_root = value,
                        other => {
                            return Err(format!(
                                "invalid schema: unknown top-level key `{}`",
                                other
                            )
                            .into())
                        }
                    }
                } else {
                    return Err(format!("invalid schema: malformed line `{}`", trimmed).into());
                }
            }
            2 => match section {
                Section::EntryPoints => {
                    if let Some((key, value)) = parse_key_value(trimmed) {
                        match key.as_str() {
                            "agents" => schema.entrypoints.agents = Some(value),
                            "claude" => schema.entrypoints.claude = Some(value),
                            "architecture" => schema.entrypoints.architecture = Some(value),
                            "manifest" => schema.entrypoints.manifest = Some(value),
                            "project_map" => schema.entrypoints.project_map = Some(value),
                            other => {
                                return Err(format!(
                                    "invalid schema: unknown entrypoint `{}`",
                                    other
                                )
                                .into())
                            }
                        }
                    } else {
                        return Err(
                            format!("invalid schema: malformed entrypoint `{}`", trimmed).into(),
                        );
                    }
                }
                Section::CoreFiles => {
                    flush_core(&mut schema, &mut current_core);
                    current_core = Some(CoreFileSpec {
                        name: parse_item_header(trimmed, "core file")?,
                        ..CoreFileSpec::default()
                    });
                }
                Section::DocCollections => {
                    flush_collection(&mut schema, &mut current_collection);
                    current_collection = Some(DocCollectionSpec {
                        name: parse_item_header(trimmed, "doc collection")?,
                        ..DocCollectionSpec::default()
                    });
                }
                Section::Suppressions => {
                    flush_suppression(&mut schema, &mut current_suppression);
                    current_suppression = Some(SuppressionSpec {
                        name: parse_item_header(trimmed, "suppression")?,
                        ..SuppressionSpec::default()
                    });
                }
                Section::Rules => {
                    if let Some((key, value)) = parse_key_value(trimmed) {
                        let parsed = parse_bool(&value)?;
                        match key.as_str() {
                            "path_defines_doc_type" => {
                                schema.rules.path_defines_doc_type = Some(parsed)
                            }
                            "h1_is_title" => schema.rules.h1_is_title = Some(parsed),
                            "first_paragraph_is_summary" => {
                                schema.rules.first_paragraph_is_summary = Some(parsed)
                            }
                            "index_required" => schema.rules.index_required = Some(parsed),
                            "prefer_directory_defaults" => {
                                schema.rules.prefer_directory_defaults = Some(parsed)
                            }
                            "minimal_frontmatter_only" => {
                                schema.rules.minimal_frontmatter_only = Some(parsed)
                            }
                            "stale_explicit_anchor" => {
                                schema.rules.stale_explicit_anchor = Some(parsed)
                            }
                            "duplicate_detection" => {
                                schema.rules.duplicate_detection = Some(parsed)
                            }
                            "contamination_checks" => {
                                schema.rules.contamination_checks = Some(parsed)
                            }
                            "strict_checks" => schema.rules.strict_checks = Some(parsed),
                            other => {
                                return Err(
                                    format!("invalid schema: unknown rule `{}`", other).into()
                                )
                            }
                        }
                    } else {
                        return Err(format!("invalid schema: malformed rule `{}`", trimmed).into());
                    }
                }
                Section::None => {}
            },
            4 => match section {
                Section::CoreFiles => {
                    if let Some((key, value)) = parse_key_value(trimmed) {
                        let spec = current_core.as_mut().ok_or_else(|| {
                            "invalid schema: core file property without item".to_string()
                        })?;
                        match key.as_str() {
                            "path" => spec.path = value,
                            "authority" => spec.authority = Some(value),
                            "status" => spec.status = Some(value),
                            "required" => spec.required = parse_bool(&value)?,
                            "template" => spec.template = value,
                            "root_path" => spec.root_path = Some(value),
                            other => {
                                return Err(format!(
                                    "invalid schema: unknown core file property `{}`",
                                    other
                                )
                                .into())
                            }
                        }
                    } else {
                        return Err(format!(
                            "invalid schema: malformed core file property `{}`",
                            trimmed
                        )
                        .into());
                    }
                }
                Section::DocCollections => {
                    if let Some((key, value)) = parse_key_value(trimmed) {
                        let spec = current_collection.as_mut().ok_or_else(|| {
                            "invalid schema: collection property without item".to_string()
                        })?;
                        match key.as_str() {
                            "path" => spec.path = value,
                            "authority" => spec.authority = Some(value),
                            "status" => spec.status = Some(value),
                            "anchor_candidate" => spec.anchor_candidate = parse_bool(&value)?,
                            "template" => spec.template = value,
                            "allowed_frontmatter" => {
                                spec.allowed_frontmatter = parse_inline_list(&value)
                            }
                            other => {
                                return Err(format!(
                                    "invalid schema: unknown doc collection property `{}`",
                                    other
                                )
                                .into())
                            }
                        }
                    } else {
                        return Err(format!(
                            "invalid schema: malformed doc collection property `{}`",
                            trimmed
                        )
                        .into());
                    }
                }
                Section::Suppressions => {
                    if let Some((key, value)) = parse_key_value(trimmed) {
                        let spec = current_suppression.as_mut().ok_or_else(|| {
                            "invalid schema: suppression property without item".to_string()
                        })?;
                        match key.as_str() {
                            "rule_id" => spec.rule_id = value,
                            "path" => spec.path = value,
                            "reason" => spec.reason = value,
                            other => {
                                return Err(format!(
                                    "invalid schema: unknown suppression property `{}`",
                                    other
                                )
                                .into())
                            }
                        }
                    } else {
                        return Err(format!(
                            "invalid schema: malformed suppression property `{}`",
                            trimmed
                        )
                        .into());
                    }
                }
                Section::Rules | Section::EntryPoints | Section::None => {
                    return Err(
                        format!("invalid schema: unexpected indentation for `{}`", trimmed).into(),
                    )
                }
            },
            _ => {
                return Err(
                    format!("invalid schema: unsupported indentation for `{}`", trimmed).into(),
                )
            }
        }
    }

    flush_core(&mut schema, &mut current_core);
    flush_collection(&mut schema, &mut current_collection);
    flush_suppression(&mut schema, &mut current_suppression);

    validate_schema(&schema)?;
    Ok(schema)
}

fn flush_core(schema: &mut Schema, current: &mut Option<CoreFileSpec>) {
    if let Some(spec) = current.take() {
        schema.core_files.push(spec);
    }
}

fn flush_collection(schema: &mut Schema, current: &mut Option<DocCollectionSpec>) {
    if let Some(spec) = current.take() {
        schema.doc_collections.push(spec);
    }
}

fn flush_suppression(schema: &mut Schema, current: &mut Option<SuppressionSpec>) {
    if let Some(spec) = current.take() {
        schema.suppressions.push(spec);
    }
}

fn parse_key_value(line: &str) -> Option<(String, String)> {
    let (key, value) = line.split_once(':')?;
    Some((key.trim().to_string(), value.trim().to_string()))
}

fn parse_item_header(line: &str, kind: &str) -> Result<String> {
    if !line.ends_with(':') {
        return Err(format!("invalid schema: expected {} header, got `{}`", kind, line).into());
    }

    let name = line.trim_end_matches(':').trim();
    if name.is_empty() {
        return Err(format!("invalid schema: empty {} name", kind).into());
    }

    Ok(name.to_string())
}

fn parse_u32(value: &str) -> Result<u32> {
    Ok(value.parse::<u32>()?)
}

fn parse_bool(value: &str) -> Result<bool> {
    match value {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(format!("invalid schema: expected boolean, got `{}`", value).into()),
    }
}

fn parse_inline_list(value: &str) -> Vec<String> {
    let inner = value.trim().trim_start_matches('[').trim_end_matches(']');
    let mut items = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    for ch in inner.chars() {
        match ch {
            '"' | '\'' if quote == Some(ch) => quote = None,
            '"' | '\'' if quote.is_none() => quote = Some(ch),
            ',' if quote.is_none() => {
                push_inline_list_item(&mut items, &current);
                current.clear();
            }
            _ => current.push(ch),
        }
    }
    push_inline_list_item(&mut items, &current);
    items
}

fn push_inline_list_item(items: &mut Vec<String>, value: &str) {
    let trimmed = value.trim().trim_matches('"').trim_matches('\'');
    if !trimmed.is_empty() {
        items.push(trimmed.to_string());
    }
}

fn rewrite_docs_root_bound_paths(schema: &mut Schema, old_root: &str, new_root: &str) {
    schema.entrypoints.manifest = schema
        .entrypoints
        .manifest
        .take()
        .map(|path| rewrite_rooted_path(path, old_root, new_root));
    schema.entrypoints.project_map = schema
        .entrypoints
        .project_map
        .take()
        .map(|path| rewrite_rooted_path(path, old_root, new_root));
}

fn rewrite_rooted_path(path: String, old_root: &str, new_root: &str) -> String {
    let old_prefix = format!("{}/", old_root.trim_end_matches('/'));
    if let Some(suffix) = path.strip_prefix(&old_prefix) {
        format!("{}/{}", new_root.trim_end_matches('/'), suffix)
    } else if path == old_root {
        new_root.to_string()
    } else {
        path
    }
}

fn validate_schema(schema: &Schema) -> Result<()> {
    if schema.managed_root.trim().is_empty() {
        return Err("invalid schema: managed_root is empty".into());
    }
    if schema.schema_version != 0 {
        return Err(format!(
            "invalid schema: unsupported schema_version `{}`; this harnesskit binary supports schema_version 0, please upgrade harnesskit for newer schemas",
            schema.schema_version
        )
        .into());
    }
    if schema.entrypoints.manifest.is_none() {
        return Err("invalid schema: entrypoints.manifest is required".into());
    }
    if schema.entrypoints.project_map.is_none() {
        return Err("invalid schema: entrypoints.project_map is required".into());
    }

    let mut core_names = BTreeSet::new();
    for spec in &schema.core_files {
        if spec.name.trim().is_empty() {
            return Err("invalid schema: core file name is empty".into());
        }
        if !core_names.insert(spec.name.clone()) {
            return Err(format!("invalid schema: duplicate core file `{}`", spec.name).into());
        }
        if spec.template.trim().is_empty() {
            return Err(
                format!("invalid schema: core file `{}` missing template", spec.name).into(),
            );
        }
        if spec.path.trim().is_empty() && spec.root_path.as_deref().unwrap_or("").trim().is_empty()
        {
            return Err(format!(
                "invalid schema: core file `{}` missing path/root_path",
                spec.name
            )
            .into());
        }
    }

    let mut collection_names = BTreeSet::new();
    let mut collection_paths = BTreeSet::new();
    for spec in &schema.doc_collections {
        if spec.name.trim().is_empty() {
            return Err("invalid schema: doc collection name is empty".into());
        }
        if !collection_names.insert(spec.name.clone()) {
            return Err(format!("invalid schema: duplicate doc collection `{}`", spec.name).into());
        }
        if spec.path.trim().is_empty() {
            return Err(format!(
                "invalid schema: doc collection `{}` missing path",
                spec.name
            )
            .into());
        }
        if !collection_paths.insert(spec.path.clone()) {
            return Err(format!(
                "invalid schema: duplicate doc collection path `{}`",
                spec.path
            )
            .into());
        }
        if spec.template.trim().is_empty() {
            return Err(format!(
                "invalid schema: doc collection `{}` missing template",
                spec.name
            )
            .into());
        }
    }

    let mut suppression_names = BTreeSet::new();
    for spec in &schema.suppressions {
        if spec.name.trim().is_empty() {
            return Err("invalid schema: suppression name is empty".into());
        }
        if !suppression_names.insert(spec.name.clone()) {
            return Err(format!("invalid schema: duplicate suppression `{}`", spec.name).into());
        }
        if spec.rule_id.trim().is_empty() {
            return Err(format!(
                "invalid schema: suppression `{}` missing rule_id",
                spec.name
            )
            .into());
        }
        if spec.path.trim().is_empty() {
            return Err(format!("invalid schema: suppression `{}` missing path", spec.name).into());
        }
        if spec.reason.trim().is_empty() {
            return Err(
                format!("invalid schema: suppression `{}` missing reason", spec.name).into(),
            );
        }
    }

    Ok(())
}

fn render_schema_copy(schema: &Schema) -> String {
    let mut lines = vec![
        format!("schema_version: {}", schema.schema_version),
        format!("managed_root: {}", schema.managed_root),
        String::new(),
        "entrypoints:".to_string(),
    ];

    push_optional_line(
        &mut lines,
        2,
        "agents",
        schema.entrypoints.agents.as_deref(),
    );
    push_optional_line(
        &mut lines,
        2,
        "claude",
        schema.entrypoints.claude.as_deref(),
    );
    push_optional_line(
        &mut lines,
        2,
        "architecture",
        schema.entrypoints.architecture.as_deref(),
    );
    push_optional_line(
        &mut lines,
        2,
        "manifest",
        schema.entrypoints.manifest.as_deref(),
    );
    push_optional_line(
        &mut lines,
        2,
        "project_map",
        schema.entrypoints.project_map.as_deref(),
    );

    lines.push(String::new());
    lines.push("core_files:".to_string());
    for spec in &schema.core_files {
        lines.push(format!("  {}:", spec.name));
        push_optional_line(&mut lines, 4, "path", non_empty(Some(spec.path.as_str())));
        push_optional_line(&mut lines, 4, "root_path", spec.root_path.as_deref());
        push_optional_line(&mut lines, 4, "authority", spec.authority.as_deref());
        push_optional_line(&mut lines, 4, "status", spec.status.as_deref());
        lines.push(format!("    required: {}", spec.required));
        lines.push(format!("    template: {}", spec.template));
    }

    lines.push(String::new());
    lines.push("doc_collections:".to_string());
    for spec in &schema.doc_collections {
        lines.push(format!("  {}:", spec.name));
        lines.push(format!("    path: {}", spec.path));
        push_optional_line(&mut lines, 4, "authority", spec.authority.as_deref());
        push_optional_line(&mut lines, 4, "status", spec.status.as_deref());
        lines.push(format!("    anchor_candidate: {}", spec.anchor_candidate));
        lines.push(format!("    template: {}", spec.template));
        lines.push(format!(
            "    allowed_frontmatter: [{}]",
            spec.allowed_frontmatter.join(", ")
        ));
    }

    lines.push(String::new());
    lines.push("rules:".to_string());
    push_optional_bool_line(
        &mut lines,
        2,
        "path_defines_doc_type",
        schema.rules.path_defines_doc_type,
    );
    push_optional_bool_line(&mut lines, 2, "h1_is_title", schema.rules.h1_is_title);
    push_optional_bool_line(
        &mut lines,
        2,
        "first_paragraph_is_summary",
        schema.rules.first_paragraph_is_summary,
    );
    push_optional_bool_line(&mut lines, 2, "index_required", schema.rules.index_required);
    push_optional_bool_line(
        &mut lines,
        2,
        "prefer_directory_defaults",
        schema.rules.prefer_directory_defaults,
    );
    push_optional_bool_line(
        &mut lines,
        2,
        "minimal_frontmatter_only",
        schema.rules.minimal_frontmatter_only,
    );
    push_optional_bool_line(
        &mut lines,
        2,
        "stale_explicit_anchor",
        schema.rules.stale_explicit_anchor,
    );
    push_optional_bool_line(
        &mut lines,
        2,
        "duplicate_detection",
        schema.rules.duplicate_detection,
    );
    push_optional_bool_line(
        &mut lines,
        2,
        "contamination_checks",
        schema.rules.contamination_checks,
    );
    push_optional_bool_line(&mut lines, 2, "strict_checks", schema.rules.strict_checks);

    if !schema.suppressions.is_empty() {
        lines.push(String::new());
        lines.push("suppressions:".to_string());
        for spec in &schema.suppressions {
            lines.push(format!("  {}:", spec.name));
            lines.push(format!("    rule_id: {}", spec.rule_id));
            lines.push(format!("    path: {}", spec.path));
            lines.push(format!("    reason: {}", spec.reason));
        }
    }

    lines.join("\n") + "\n"
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.filter(|item| !item.trim().is_empty())
}

fn push_optional_line(lines: &mut Vec<String>, indent: usize, key: &str, value: Option<&str>) {
    if let Some(value) = non_empty(value) {
        lines.push(format!("{}{}: {}", " ".repeat(indent), key, value));
    }
}

fn push_optional_bool_line(lines: &mut Vec<String>, indent: usize, key: &str, value: Option<bool>) {
    if let Some(value) = value {
        lines.push(format!("{}{}: {}", " ".repeat(indent), key, value));
    }
}

fn materialize_from_schema(
    target_dir: &Path,
    schema: &Schema,
    schema_copy_text: &str,
    force: bool,
) -> Result<InitStats> {
    let mut stats = InitStats::default();
    let docs_root_path = target_dir.join(&schema.managed_root);
    let harnesskit_state = target_dir.join(".harnesskit").join("state");
    let harnesskit_history = target_dir.join(".harnesskit").join("history");
    let schema_dst = target_dir.join(".harnesskit").join("schema.yaml");
    let render_paths = build_render_paths(schema);

    fs::create_dir_all(&harnesskit_state)?;
    fs::create_dir_all(harnesskit_history.join("objects"))?;
    fs::create_dir_all(harnesskit_history.join("snapshots"))?;
    fs::create_dir_all(harnesskit_history.join("refs"))?;
    fs::create_dir_all(docs_root_path.join("templates"))?;

    for collection in &schema.doc_collections {
        fs::create_dir_all(docs_root_path.join(&collection.path))?;
    }

    if let Some(path) = &schema.entrypoints.agents {
        let content = render_entrypoint_template(
            "templates/entrypoints/AGENTS.md.tpl",
            schema,
            &render_paths,
        )?;
        record_write(
            write_if_allowed(&target_dir.join(path), &content, force)?,
            &mut stats,
        );
    }

    if let Some(path) = &schema.entrypoints.claude {
        let content = render_entrypoint_template(
            "templates/entrypoints/CLAUDE.md.tpl",
            schema,
            &render_paths,
        )?;
        record_write(
            write_if_allowed(&target_dir.join(path), &content, force)?,
            &mut stats,
        );
    }

    for spec in &schema.core_files {
        let template_path = core_template_path(&spec.template)
            .ok_or_else(|| format!("unknown core template: {} ({})", spec.template, spec.name))?;
        let content = render_core_template(template_path, schema, &render_paths)?;
        let target_path = if let Some(root_path) = &spec.root_path {
            target_dir.join(root_path)
        } else {
            docs_root_path.join(&spec.path)
        };
        record_write(write_if_allowed(&target_path, &content, force)?, &mut stats);
    }

    for spec in &schema.doc_collections {
        let content = render_collection_index_template(schema, spec, &render_paths)?;
        record_write(
            write_if_allowed(
                &docs_root_path.join(&spec.path).join("index.md"),
                &content,
                force,
            )?,
            &mut stats,
        );
    }

    let mut seen_templates = BTreeSet::new();
    for spec in &schema.doc_collections {
        if seen_templates.insert(spec.template.clone()) {
            let template_path = doc_template_path(&spec.template).ok_or_else(|| {
                format!("unknown doc template: {} ({})", spec.template, spec.name)
            })?;
            let filename = format!("{}.md", spec.template);
            let content = render_template(template_path, &schema.managed_root)?;
            record_write(
                write_if_allowed(
                    &docs_root_path.join("templates").join(filename),
                    &content,
                    force,
                )?,
                &mut stats,
            );
        }
    }

    record_write(
        write_if_allowed(&schema_dst, schema_copy_text, force)?,
        &mut stats,
    );

    Ok(stats)
}

fn build_index_artifact(target_dir: &Path, schema: &Schema) -> Result<IndexArtifact> {
    let repo_root = target_dir
        .canonicalize()
        .unwrap_or_else(|_| target_dir.to_path_buf());
    let mut doc_paths = collect_managed_doc_paths(&repo_root, schema)?;
    doc_paths.sort();

    let mut parsed_docs = Vec::new();
    let mut docs = Vec::new();
    let mut path_set = BTreeSet::new();
    let collection_by_path = collection_lookup(schema);

    for rel_path in &doc_paths {
        path_set.insert(rel_path.clone());
        let abs = repo_root.join(rel_path);
        let text = read_text_lossy(&abs)?;
        let parsed = parse_doc_text(rel_path, &text);
        let metadata = fs::metadata(&abs)?;
        let (mtime_unix, mtime_ns) = file_modified_parts(&metadata)?;
        let collection = collection_name_for_path(schema, rel_path);
        let authority = parsed.frontmatter.get("authority").cloned().or_else(|| {
            collection.as_ref().and_then(|name| {
                collection_by_path
                    .get(name)
                    .and_then(|spec| spec.authority.clone())
            })
        });
        let status = parsed.frontmatter.get("status").cloned().or_else(|| {
            collection.as_ref().and_then(|name| {
                collection_by_path
                    .get(name)
                    .and_then(|spec| spec.status.clone())
            })
        });
        let role = infer_index_role(schema, rel_path, collection.as_deref());
        let is_generated = authority.as_deref() == Some("generated");
        let is_reference = authority.as_deref() == Some("reference");

        docs.push(IndexedDoc {
            path: rel_path.clone(),
            title: parsed.title.clone().unwrap_or_else(|| rel_path.clone()),
            summary: parsed.summary.clone(),
            frontmatter_keys: parsed.frontmatter.keys().cloned().collect(),
            content_text: parsed.body_text.clone(),
            role,
            status,
            authority,
            collection,
            headings: parsed.headings.clone(),
            links: parsed.links.clone(),
            path_mentions: parsed.path_mentions.clone(),
            scope_paths: parsed.scope_paths.clone(),
            supersedes: parsed.supersedes.clone(),
            incoming_links: 0,
            outgoing_links: 0,
            is_entrypoint: is_entrypoint_path(schema, rel_path),
            is_anchor_candidate: is_anchor_candidate(schema, rel_path),
            is_generated,
            is_reference,
            content_hash: simple_content_hash(&text),
            file_size_bytes: metadata.len(),
            mtime_unix,
            mtime_ns,
        });
        parsed_docs.push(parsed);
    }

    let mut relations = build_doc_relations(schema, &docs, &path_set);
    annotate_link_counts(&mut docs, &relations);
    let checks = build_index_checks(schema, &repo_root, &docs, &relations);
    let history_status = compute_history_status(&repo_root, schema).unwrap_or_default();
    let host_git = host_git_info(&repo_root, HostGitMode::Full);
    relations.sort_by(|a, b| {
        a.src_path
            .cmp(&b.src_path)
            .then_with(|| a.dst_path.cmp(&b.dst_path))
            .then_with(|| a.relation_type.cmp(&b.relation_type))
    });

    Ok(IndexArtifact {
        repo_root: absolute_display_path(&repo_root),
        managed_root: schema.managed_root.clone(),
        generated_at_unix: now_unix(),
        history_latest_snapshot: history_status.latest_snapshot.clone(),
        history_dirty: is_history_dirty(&history_status),
        history_tracked_files_count: history_status.tracked_files_count,
        host_git_head: host_git.head,
        host_git_branch: host_git.branch,
        host_git_dirty: host_git.dirty,
        docs,
        relations,
        checks,
    })
}

fn write_index_artifact(target_dir: &Path, artifact: &IndexArtifact) -> Result<()> {
    let state_dir = target_dir.join(".harnesskit").join("state");
    ensure_writable_fact_store_dir(target_dir, &state_dir, "HarnessKit state")?;
    fs::write(
        state_dir.join("doc-index.json"),
        render_index_json(artifact),
    )
    .map_err(|err| {
        format!(
            "cannot write HarnessKit index artifact: {}\n\n{}",
            err,
            fact_store_path_hint(target_dir, Some(&state_dir), None)
        )
    })?;
    fs::write(
        state_dir.join("doc-index-summary.md"),
        render_index_summary(artifact),
    )
    .map_err(|err| {
        format!(
            "cannot write HarnessKit index summary: {}\n\n{}",
            err,
            fact_store_path_hint(target_dir, Some(&state_dir), None)
        )
    })?;
    Ok(())
}

fn write_fact_store(target_dir: &Path, artifact: &IndexArtifact) -> Result<()> {
    let state_dir = target_dir.join(".harnesskit").join("state");
    ensure_writable_fact_store_dir(target_dir, &state_dir, "HarnessKit fact store")?;
    let db_path = state_dir.join("facts.sqlite");
    let tmp_path = state_dir.join(format!(
        "facts.sqlite.rebuild-{}-{}",
        std::process::id(),
        now_unix()
    ));
    let _ = fs::remove_file(&tmp_path);
    let db = SqliteConnection::open(&tmp_path).map_err(|err| {
        format!(
            "cannot create HarnessKit fact store at {}: {}\n\n{}",
            tmp_path.display(),
            err,
            fact_store_path_hint(target_dir, Some(&state_dir), Some(&db_path))
        )
    })?;
    initialize_fact_store(&db)?;
    replace_fact_store_contents(&db, artifact)?;
    db.exec("PRAGMA wal_checkpoint(TRUNCATE);")?;
    drop(db);
    replace_fact_store_file(&db_path, &tmp_path)?;
    Ok(())
}

fn replace_fact_store_file(db_path: &Path, tmp_path: &Path) -> Result<()> {
    for suffix in ["-wal", "-shm"] {
        let _ = fs::remove_file(PathBuf::from(format!("{}{}", tmp_path.display(), suffix)));
        let _ = fs::remove_file(PathBuf::from(format!("{}{}", db_path.display(), suffix)));
    }
    fs::rename(tmp_path, db_path).map_err(|err| {
        let target_dir = db_path
            .parent()
            .and_then(|state| state.parent())
            .and_then(|harnesskit| harnesskit.parent())
            .unwrap_or_else(|| Path::new("."));
        format!(
            "cannot replace HarnessKit fact store at {}: {}\n\n{}",
            db_path.display(),
            err,
            fact_store_path_hint(target_dir, db_path.parent(), Some(db_path))
        )
    })?;
    Ok(())
}

fn open_fact_store(target_dir: &Path) -> Result<SqliteConnection> {
    let state_dir = target_dir.join(".harnesskit").join("state");
    ensure_writable_fact_store_dir(target_dir, &state_dir, "HarnessKit fact store")?;
    let db_path = state_dir.join("facts.sqlite");
    if !db_path.exists() {
        return Err(format!(
            "fact store not found at {}. Run `harnesskit index` first.\n\n{}",
            db_path.display(),
            fact_store_path_hint(target_dir, Some(&state_dir), Some(&db_path))
        )
        .into());
    }
    SqliteConnection::open(&db_path).map_err(|err| {
        let err_text = err.to_string();
        if err_text.contains("sqlite3 CLI not found") {
            return format!(
                "cannot open HarnessKit fact store at {} because sqlite3 is unavailable: {}\n\n{}",
                db_path.display(),
                trim_trailing_period(&err_text),
                fact_store_path_hint(target_dir, Some(&state_dir), Some(&db_path))
            )
            .into();
        }
        format!(
            "cannot open HarnessKit fact store at {}: {}. Check that the project path and `.harnesskit/state` are writable.\n\n{}",
            db_path.display(),
            err_text,
            fact_store_path_hint(target_dir, Some(&state_dir), Some(&db_path))
        )
        .into()
    })
}

fn trim_trailing_period(value: &str) -> &str {
    value.strip_suffix('.').unwrap_or(value)
}

fn scalar_count(db: &SqliteConnection, sql: &str) -> Result<usize> {
    let result = db.query(sql)?;
    Ok(result
        .rows
        .first()
        .map(|row| cell(row, 0).parse::<usize>().unwrap_or(0))
        .unwrap_or(0))
}

fn refresh_fact_store(context: &EngineContext) -> Result<()> {
    let db_path = context
        .target_dir
        .join(".harnesskit")
        .join("state")
        .join("facts.sqlite");
    if !db_path.exists() {
        rebuild_fact_store_full(context)?;
        return Ok(());
    }

    let db = open_fact_store(&context.target_dir)?;
    initialize_fact_store(&db)?;
    if !fact_store_is_compatible(&db)? {
        rebuild_fact_store_full(context)?;
        return Ok(());
    }

    let delta = compute_fact_store_delta(context)?;
    if delta.added_paths.is_empty()
        && delta.changed_paths.is_empty()
        && delta.removed_paths.is_empty()
    {
        if history_or_host_git_state_changed(context, &db)? {
            refresh_fact_store_metadata(context, &db, "metadata")?;
        }
        return Ok(());
    }

    apply_fact_store_delta(context, &delta)?;
    Ok(())
}

fn history_or_host_git_state_changed(
    context: &EngineContext,
    db: &SqliteConnection,
) -> Result<bool> {
    let history_status =
        compute_history_status(&context.target_dir, &context.schema).unwrap_or_default();
    let host_git = host_git_info(&context.target_dir, HostGitMode::Full);
    let current = [
        (
            "history_latest_snapshot",
            history_status.latest_snapshot.clone().unwrap_or_default(),
        ),
        (
            "history_dirty",
            if is_history_dirty(&history_status) {
                "true"
            } else {
                "false"
            }
            .to_string(),
        ),
        (
            "history_tracked_files_count",
            history_status.tracked_files_count.to_string(),
        ),
        ("host_git_head", host_git.head.unwrap_or_default()),
        ("host_git_branch", host_git.branch.unwrap_or_default()),
        (
            "host_git_dirty",
            if host_git.dirty { "true" } else { "false" }.to_string(),
        ),
    ];
    for (key, value) in current {
        if meta_value(db, key)?.unwrap_or_default() != value {
            return Ok(true);
        }
    }
    Ok(false)
}

fn refresh_fact_store_metadata(
    context: &EngineContext,
    db: &SqliteConnection,
    mode: &str,
) -> Result<()> {
    let artifact = read_index_artifact_from_fact_store(db)?;
    let repo_root = context
        .target_dir
        .canonicalize()
        .unwrap_or_else(|_| context.target_dir.clone());
    let history_status = compute_history_status(&repo_root, &context.schema).unwrap_or_default();
    let host_git = host_git_info(&repo_root, HostGitMode::Full);
    let checks = build_index_checks(
        &context.schema,
        &repo_root,
        &artifact.docs,
        &artifact.relations,
    );
    replace_checks_and_meta(
        db,
        &absolute_display_path(&repo_root),
        &context.schema.managed_root,
        now_unix(),
        mode,
        &FactStoreDelta::default(),
        &history_status,
        &host_git,
        &checks,
    )
}

fn rebuild_fact_store_full(context: &EngineContext) -> Result<()> {
    let artifact = build_index_artifact(&context.target_dir, &context.schema)?;
    write_index_artifact(&context.target_dir, &artifact)?;
    write_fact_store(&context.target_dir, &artifact)?;
    Ok(())
}

fn fact_store_is_compatible(db: &SqliteConnection) -> Result<bool> {
    let version = meta_value(db, "fact_schema_version")?;
    if version.as_deref() != Some(FACT_SCHEMA_VERSION) {
        return Ok(false);
    }

    let required_tables = [
        "docs",
        "file_states",
        "headings",
        "raw_links",
        "path_mentions",
        "relations",
        "checks",
        "scope_paths",
        "supersedes",
    ];
    for table in required_tables {
        let exists = db.query(&format!(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = {};",
            sql_string_literal(table)
        ))?;
        if exists
            .rows
            .first()
            .map(|row| cell(row, 0) != "1")
            .unwrap_or(true)
        {
            return Ok(false);
        }
    }

    let file_state_columns = db.query("PRAGMA table_info(file_states);")?;
    let has_mtime_ns = file_state_columns
        .rows
        .iter()
        .any(|row| cell(row, 1) == "mtime_ns");
    if !has_mtime_ns {
        return Ok(false);
    }

    Ok(true)
}

fn meta_value(db: &SqliteConnection, key: &str) -> Result<Option<String>> {
    let result = db.query(&format!(
        "SELECT value FROM meta WHERE key = {};",
        sql_string_literal(key)
    ))?;
    Ok(result.rows.first().map(|row| cell(row, 0).to_string()))
}

fn compute_fact_store_delta(context: &EngineContext) -> Result<FactStoreDelta> {
    let db = open_fact_store(&context.target_dir)?;
    let rows = db.query("SELECT path, content_hash, file_size_bytes, mtime_unix, mtime_ns FROM file_states ORDER BY path ASC;")?;
    let previous_states = parse_file_state_rows(&rows);
    let current_states =
        collect_current_file_states(&context.target_dir, &context.schema, &previous_states)?;

    let mut added_paths = Vec::new();
    let mut changed_paths = Vec::new();
    let mut removed_paths = Vec::new();

    for (path, current) in &current_states {
        match previous_states.get(path) {
            None => added_paths.push(path.clone()),
            Some(previous) => {
                if previous.content_hash != current.content_hash
                    || previous.file_size_bytes != current.file_size_bytes
                    || previous.mtime_unix != current.mtime_unix
                    || previous.mtime_ns != current.mtime_ns
                {
                    changed_paths.push(path.clone());
                }
            }
        }
    }

    for path in previous_states.keys() {
        if !current_states.contains_key(path) {
            removed_paths.push(path.clone());
        }
    }

    added_paths.sort();
    changed_paths.sort();
    removed_paths.sort();

    Ok(FactStoreDelta {
        added_paths,
        changed_paths,
        removed_paths,
    })
}

fn parse_file_state_rows(result: &QueryResult) -> BTreeMap<String, FileStateRecord> {
    let mut states = BTreeMap::new();
    for row in &result.rows {
        let path = cell(row, 0).to_string();
        if path.is_empty() {
            continue;
        }
        states.insert(
            path.clone(),
            FileStateRecord {
                content_hash: cell(row, 1).to_string(),
                file_size_bytes: cell(row, 2).parse::<u64>().unwrap_or(0),
                mtime_unix: cell(row, 3).parse::<u64>().unwrap_or(0),
                mtime_ns: cell(row, 4).parse::<u64>().unwrap_or(0),
            },
        );
    }
    states
}

fn collect_current_file_states(
    target_dir: &Path,
    schema: &Schema,
    previous_states: &BTreeMap<String, FileStateRecord>,
) -> Result<BTreeMap<String, FileStateRecord>> {
    let mut states = BTreeMap::new();
    for rel_path in collect_managed_doc_paths(target_dir, schema)? {
        let abs = target_dir.join(&rel_path);
        let metadata = fs::metadata(&abs)?;
        let (mtime_unix, mtime_ns) = file_modified_parts(&metadata)?;
        if let Some(previous) = previous_states.get(&rel_path) {
            if previous.file_size_bytes == metadata.len() && previous.mtime_ns == mtime_ns {
                states.insert(rel_path.clone(), previous.clone());
                continue;
            }
        }
        let text = read_text_lossy(&abs)?;
        states.insert(
            rel_path.clone(),
            FileStateRecord {
                content_hash: simple_content_hash(&text),
                file_size_bytes: metadata.len(),
                mtime_unix,
                mtime_ns,
            },
        );
    }
    Ok(states)
}

fn apply_fact_store_delta(context: &EngineContext, delta: &FactStoreDelta) -> Result<()> {
    let db = open_fact_store(&context.target_dir)?;
    let mut artifact = read_index_artifact_from_fact_store(&db)?;
    let previous_docs = artifact.docs.clone();
    let changed_set = delta
        .added_paths
        .iter()
        .chain(delta.changed_paths.iter())
        .cloned()
        .collect::<BTreeSet<_>>();
    let repo_root = context
        .target_dir
        .canonicalize()
        .unwrap_or_else(|_| context.target_dir.clone());
    let collection_by_path = collection_lookup(&context.schema);
    let changed_docs = changed_set
        .iter()
        .map(|path| {
            load_indexed_doc(
                &context.target_dir,
                &context.schema,
                &collection_by_path,
                path,
            )
        })
        .collect::<Result<Vec<_>>>()?;

    apply_doc_fact_delta(
        &db,
        &delta
            .added_paths
            .iter()
            .chain(delta.changed_paths.iter())
            .chain(delta.removed_paths.iter())
            .cloned()
            .collect::<Vec<_>>(),
        &changed_docs,
    )?;

    let generated_at_unix = now_unix();
    apply_doc_delta_to_artifact_docs(&mut artifact.docs, &changed_docs, &delta.removed_paths);
    let path_set = artifact
        .docs
        .iter()
        .map(|doc| doc.path.clone())
        .collect::<BTreeSet<_>>();
    let impacted_sources = collect_impacted_relation_sources(
        &db,
        &context.schema,
        delta,
        &changed_docs,
        &previous_docs,
        &path_set,
    )?;
    let impacted_docs = artifact
        .docs
        .iter()
        .filter(|doc| impacted_sources.contains(&doc.path))
        .cloned()
        .collect::<Vec<_>>();
    let rebuilt_relations = build_doc_relations(&context.schema, &impacted_docs, &path_set);
    replace_relations_for_sources(&db, &impacted_sources, &rebuilt_relations)?;

    artifact = read_index_artifact_from_fact_store(&db)?;
    artifact.repo_root = absolute_display_path(&repo_root);
    artifact.managed_root = context.schema.managed_root.clone();
    artifact.generated_at_unix = generated_at_unix;
    let history_status = compute_history_status(&repo_root, &context.schema).unwrap_or_default();
    let host_git = host_git_info(&repo_root, HostGitMode::Full);
    artifact.history_latest_snapshot = history_status.latest_snapshot.clone();
    artifact.history_dirty = is_history_dirty(&history_status);
    artifact.history_tracked_files_count = history_status.tracked_files_count;
    artifact.host_git_head = host_git.head.clone();
    artifact.host_git_branch = host_git.branch.clone();
    artifact.host_git_dirty = host_git.dirty;
    let mut relations = artifact.relations.clone();
    relations.sort_by(|a, b| {
        a.src_path
            .cmp(&b.src_path)
            .then_with(|| a.dst_path.cmp(&b.dst_path))
            .then_with(|| a.relation_type.cmp(&b.relation_type))
    });
    annotate_link_counts(&mut artifact.docs, &relations);
    update_doc_link_counts_in_db(&db, &artifact.docs)?;
    let checks = build_index_checks(&context.schema, &repo_root, &artifact.docs, &relations);
    replace_checks_and_meta(
        &db,
        &artifact.repo_root,
        &artifact.managed_root,
        generated_at_unix,
        "delta",
        delta,
        &history_status,
        &host_git,
        &checks,
    )?;
    artifact.relations = relations;
    artifact.checks = checks;
    write_index_artifact(&context.target_dir, &artifact)?;
    Ok(())
}

fn apply_doc_delta_to_artifact_docs(
    docs: &mut Vec<IndexedDoc>,
    changed_docs: &[IndexedDoc],
    removed_paths: &[String],
) {
    let removed = removed_paths.iter().cloned().collect::<BTreeSet<_>>();
    let mut changed_by_path = changed_docs
        .iter()
        .map(|doc| (doc.path.clone(), doc.clone()))
        .collect::<BTreeMap<_, _>>();
    let mut next = Vec::with_capacity(docs.len() + changed_docs.len());
    for doc in docs.drain(..) {
        if removed.contains(&doc.path) {
            continue;
        }
        if let Some(changed) = changed_by_path.remove(&doc.path) {
            next.push(changed);
        } else {
            next.push(doc);
        }
    }
    next.extend(changed_by_path.into_values());
    next.sort_by(|a, b| a.path.cmp(&b.path));
    *docs = next;
}

fn initialize_fact_store(db: &SqliteConnection) -> Result<()> {
    db.exec(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         CREATE TABLE IF NOT EXISTS meta (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS docs (
            path TEXT PRIMARY KEY,
            title TEXT NOT NULL,
            role TEXT NOT NULL,
            status TEXT,
            authority TEXT,
            collection_name TEXT,
            summary TEXT NOT NULL,
            frontmatter_keys_text TEXT NOT NULL DEFAULT '',
            headings_text TEXT NOT NULL,
            content_hash TEXT NOT NULL,
            file_size_bytes INTEGER NOT NULL,
            incoming_links INTEGER NOT NULL,
            outgoing_links INTEGER NOT NULL,
            is_entrypoint INTEGER NOT NULL,
            is_anchor_candidate INTEGER NOT NULL,
            is_generated INTEGER NOT NULL,
            is_reference INTEGER NOT NULL,
            mtime_unix INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS file_states (
            path TEXT PRIMARY KEY,
            content_hash TEXT NOT NULL,
            file_size_bytes INTEGER NOT NULL,
            mtime_unix INTEGER NOT NULL,
            mtime_ns INTEGER NOT NULL DEFAULT 0
         );
         CREATE TABLE IF NOT EXISTS headings (
            doc_path TEXT NOT NULL,
            heading_index INTEGER NOT NULL,
            heading_text TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS raw_links (
            src_doc TEXT NOT NULL,
            link_index INTEGER NOT NULL,
            normalized_target TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS path_mentions (
            doc_path TEXT NOT NULL,
            mention_index INTEGER NOT NULL,
            mentioned_path TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS relations (
            src_path TEXT NOT NULL,
            dst_path TEXT NOT NULL,
            relation_type TEXT NOT NULL,
            reason TEXT NOT NULL,
            explicit INTEGER NOT NULL,
            confidence REAL NOT NULL,
            evidence_json TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS checks (
            rule_id TEXT NOT NULL,
            severity TEXT NOT NULL,
            subject_path TEXT NOT NULL,
            message TEXT NOT NULL,
            evidence_json TEXT NOT NULL DEFAULT '{}'
         );
         CREATE TABLE IF NOT EXISTS scope_paths (
            doc_path TEXT NOT NULL,
            scope_path TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS supersedes (
            doc_path TEXT NOT NULL,
            superseded_path TEXT NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_docs_role ON docs(role);
         CREATE INDEX IF NOT EXISTS idx_docs_collection ON docs(collection_name);
         CREATE INDEX IF NOT EXISTS idx_docs_status ON docs(status);
         CREATE INDEX IF NOT EXISTS idx_docs_mtime ON docs(mtime_unix);
         CREATE INDEX IF NOT EXISTS idx_relations_src ON relations(src_path, relation_type);
         CREATE INDEX IF NOT EXISTS idx_relations_dst ON relations(dst_path, relation_type);
         CREATE INDEX IF NOT EXISTS idx_checks_subject ON checks(subject_path, severity);
         CREATE VIRTUAL TABLE IF NOT EXISTS docs_fts USING fts5(
            path UNINDEXED,
            title,
            headings_text,
            summary,
            body_tokens
         );",
    )?;
    Ok(())
}

fn load_indexed_doc(
    target_dir: &Path,
    schema: &Schema,
    collection_by_path: &BTreeMap<String, &DocCollectionSpec>,
    rel_path: &str,
) -> Result<IndexedDoc> {
    let abs = target_dir.join(rel_path);
    let text = read_text_lossy(&abs)?;
    let parsed = parse_doc_text(rel_path, &text);
    let metadata = fs::metadata(&abs)?;
    let (mtime_unix, mtime_ns) = file_modified_parts(&metadata)?;
    let collection = collection_name_for_path(schema, rel_path);
    let authority = parsed.frontmatter.get("authority").cloned().or_else(|| {
        collection.as_ref().and_then(|name| {
            collection_by_path
                .get(name)
                .and_then(|spec| spec.authority.clone())
        })
    });
    let status = parsed.frontmatter.get("status").cloned().or_else(|| {
        collection.as_ref().and_then(|name| {
            collection_by_path
                .get(name)
                .and_then(|spec| spec.status.clone())
        })
    });
    let role = infer_index_role(schema, rel_path, collection.as_deref());
    let is_generated = authority.as_deref() == Some("generated");
    let is_reference = authority.as_deref() == Some("reference");

    Ok(IndexedDoc {
        path: rel_path.to_string(),
        title: parsed.title.clone().unwrap_or_else(|| rel_path.to_string()),
        summary: parsed.summary.clone(),
        frontmatter_keys: parsed.frontmatter.keys().cloned().collect(),
        content_text: parsed.body_text.clone(),
        role,
        status,
        authority,
        collection,
        headings: parsed.headings.clone(),
        links: parsed.links.clone(),
        path_mentions: parsed.path_mentions.clone(),
        scope_paths: parsed.scope_paths.clone(),
        supersedes: parsed.supersedes.clone(),
        incoming_links: 0,
        outgoing_links: 0,
        is_entrypoint: is_entrypoint_path(schema, rel_path),
        is_anchor_candidate: is_anchor_candidate(schema, rel_path),
        is_generated,
        is_reference,
        content_hash: simple_content_hash(&text),
        file_size_bytes: metadata.len(),
        mtime_unix,
        mtime_ns,
    })
}

fn apply_doc_fact_delta(
    db: &SqliteConnection,
    touched_paths: &[String],
    changed_docs: &[IndexedDoc],
) -> Result<()> {
    let mut sql = String::from("BEGIN IMMEDIATE;");
    for path in touched_paths {
        let escaped = sql_string_literal(path);
        sql.push_str(&format!(
            "DELETE FROM docs WHERE path = {p};
             DELETE FROM file_states WHERE path = {p};
             DELETE FROM headings WHERE doc_path = {p};
             DELETE FROM raw_links WHERE src_doc = {p};
             DELETE FROM path_mentions WHERE doc_path = {p};
             DELETE FROM scope_paths WHERE doc_path = {p};
             DELETE FROM supersedes WHERE doc_path = {p};
             DELETE FROM docs_fts WHERE path = {p};",
            p = escaped
        ));
    }

    for doc in changed_docs {
        sql.push_str(&render_doc_fact_sql(doc));
    }

    sql.push_str("COMMIT;");
    db.exec(&sql)?;
    Ok(())
}

fn collect_impacted_relation_sources(
    db: &SqliteConnection,
    schema: &Schema,
    delta: &FactStoreDelta,
    changed_docs: &[IndexedDoc],
    previous_docs: &[IndexedDoc],
    path_set: &BTreeSet<String>,
) -> Result<BTreeSet<String>> {
    let mut impacted = BTreeSet::new();
    let touched_paths = delta
        .added_paths
        .iter()
        .chain(delta.changed_paths.iter())
        .chain(delta.removed_paths.iter())
        .cloned()
        .collect::<BTreeSet<_>>();
    let removed_paths = delta.removed_paths.iter().cloned().collect::<BTreeSet<_>>();

    for doc in changed_docs {
        impacted.insert(doc.path.clone());
        if let Some(index_path) = collection_index_for_doc(schema, doc) {
            impacted.insert(index_path);
        }
    }

    for doc in previous_docs {
        if removed_paths.contains(&doc.path) {
            if let Some(index_path) = collection_index_for_doc(schema, doc) {
                impacted.insert(index_path);
            }
            continue;
        }
        let depends_on_touched = doc.links.iter().any(|link| touched_paths.contains(link))
            || doc
                .path_mentions
                .iter()
                .any(|path| touched_paths.contains(path))
            || doc
                .scope_paths
                .iter()
                .any(|path| touched_paths.contains(path))
            || doc
                .supersedes
                .iter()
                .any(|path| touched_paths.contains(path));
        if depends_on_touched {
            impacted.insert(doc.path.clone());
        }
    }

    for removed in &removed_paths {
        let rows = db.query(&format!(
            "SELECT DISTINCT src_path FROM relations WHERE dst_path = {} OR src_path = {};",
            sql_string_literal(removed),
            sql_string_literal(removed)
        ))?;
        for row in &rows.rows {
            let path = cell(row, 0).to_string();
            if !path.is_empty() && path_set.contains(&path) {
                impacted.insert(path);
            }
        }
    }

    let manifest_path = schema.entrypoints.manifest.as_deref().unwrap_or("");
    if !manifest_path.is_empty() {
        impacted.insert(manifest_path.to_string());
    }
    if let Some(path) = &schema.entrypoints.agents {
        impacted.insert(path.clone());
    }
    if let Some(path) = &schema.entrypoints.claude {
        impacted.insert(path.clone());
    }
    if let Some(path) = &schema.entrypoints.architecture {
        impacted.insert(path.clone());
    }

    Ok(impacted)
}

fn collection_index_for_doc(schema: &Schema, doc: &IndexedDoc) -> Option<String> {
    let collection = doc.collection.as_deref()?;
    let spec = schema
        .doc_collections
        .iter()
        .find(|item| item.name == collection)?;
    Some(format!("{}/{}/index.md", schema.managed_root, spec.path))
}

fn replace_relations_for_sources(
    db: &SqliteConnection,
    source_paths: &BTreeSet<String>,
    relations: &[DocRelation],
) -> Result<()> {
    let mut statements = Vec::new();
    let mut prelude = String::new();
    for path in source_paths {
        prelude.push_str(&format!(
            "DELETE FROM relations WHERE src_path = {path};
             DELETE FROM relations WHERE dst_path = {path} AND relation_type = 'code_mentions_doc';",
            path = sql_string_literal(path)
        ));
    }
    statements.push(prelude);

    for relation in relations {
        let mut statement = String::new();
        append_relation_insert_sql(&mut statement, relation);
        statements.push(statement);
    }

    exec_sql_batches(db, &statements, 500)
}

fn update_doc_link_counts_in_db(db: &SqliteConnection, docs: &[IndexedDoc]) -> Result<()> {
    let mut statements = Vec::new();
    for doc in docs {
        statements.push(format!(
            "UPDATE docs
             SET incoming_links = {incoming_links},
                 outgoing_links = {outgoing_links}
             WHERE path = {path};",
            incoming_links = doc.incoming_links,
            outgoing_links = doc.outgoing_links,
            path = sql_string_literal(&doc.path)
        ));
    }
    exec_sql_batches(db, &statements, 500)
}

#[allow(clippy::too_many_arguments)]
fn replace_checks_and_meta(
    db: &SqliteConnection,
    repo_root: &str,
    managed_root: &str,
    generated_at_unix: u64,
    mode: &str,
    delta: &FactStoreDelta,
    history_status: &HistoryStatus,
    host_git: &HostGitInfo,
    checks: &[IndexCheck],
) -> Result<()> {
    let mut sql = String::from(
        "BEGIN IMMEDIATE;
         DELETE FROM checks;
         DELETE FROM meta;",
    );

    sql.push_str(&format!(
        "INSERT INTO meta(key, value) VALUES
            ('repo_root', {repo_root}),
            ('managed_root', {managed_root}),
            ('generated_at_unix', {generated_at_unix}),
            ('fact_schema_version', {fact_schema_version}),
            ('last_refresh_mode', {mode}),
            ('last_delta_added', {added}),
            ('last_delta_changed', {changed}),
            ('last_delta_removed', {removed}),
            ('history_latest_snapshot', {history_latest_snapshot}),
            ('history_dirty', {history_dirty}),
            ('history_tracked_files_count', {history_tracked_files_count}),
            ('host_git_head', {host_git_head}),
            ('host_git_branch', {host_git_branch}),
            ('host_git_dirty', {host_git_dirty});",
        repo_root = sql_string_literal(repo_root),
        managed_root = sql_string_literal(managed_root),
        generated_at_unix = sql_string_literal(&generated_at_unix.to_string()),
        fact_schema_version = sql_string_literal(FACT_SCHEMA_VERSION),
        mode = sql_string_literal(mode),
        added = sql_string_literal(&delta.added_paths.len().to_string()),
        changed = sql_string_literal(&delta.changed_paths.len().to_string()),
        removed = sql_string_literal(&delta.removed_paths.len().to_string()),
        history_latest_snapshot =
            sql_string_literal(history_status.latest_snapshot.as_deref().unwrap_or("")),
        history_dirty = sql_string_literal(if is_history_dirty(history_status) {
            "true"
        } else {
            "false"
        }),
        history_tracked_files_count =
            sql_string_literal(&history_status.tracked_files_count.to_string()),
        host_git_head = sql_string_literal(host_git.head.as_deref().unwrap_or("")),
        host_git_branch = sql_string_literal(host_git.branch.as_deref().unwrap_or("")),
        host_git_dirty = sql_string_literal(if host_git.dirty { "true" } else { "false" })
    ));

    for check in checks {
        append_check_insert_sql(&mut sql, check);
    }

    sql.push_str("COMMIT;");
    db.exec(&sql)?;
    Ok(())
}

fn render_doc_secondary_fact_sql(doc: &IndexedDoc) -> String {
    let mut sql = String::new();
    sql.push_str(&format!(
        "INSERT INTO file_states(path, content_hash, file_size_bytes, mtime_unix, mtime_ns) VALUES ({path}, {content_hash}, {file_size_bytes}, {mtime_unix}, {mtime_ns});",
        path = sql_string_literal(&doc.path),
        content_hash = sql_string_literal(&doc.content_hash),
        file_size_bytes = doc.file_size_bytes,
        mtime_unix = doc.mtime_unix,
        mtime_ns = doc.mtime_ns
    ));

    sql.push_str(&format!(
        "INSERT INTO docs_fts(path, title, headings_text, summary, body_tokens) VALUES ({path}, {title}, {headings_text}, {summary}, {body_tokens});",
        path = sql_string_literal(&doc.path),
        title = sql_string_literal(&doc.title),
        headings_text = sql_string_literal(&doc.headings.join(" | ")),
        summary = sql_string_literal(&doc.summary),
        body_tokens = sql_string_literal(&doc.content_text)
    ));

    for (index, heading) in doc.headings.iter().enumerate() {
        sql.push_str(&format!(
            "INSERT INTO headings(doc_path, heading_index, heading_text) VALUES ({doc_path}, {heading_index}, {heading_text});",
            doc_path = sql_string_literal(&doc.path),
            heading_index = index,
            heading_text = sql_string_literal(heading)
        ));
    }

    for (index, link) in doc.links.iter().enumerate() {
        sql.push_str(&format!(
            "INSERT INTO raw_links(src_doc, link_index, normalized_target) VALUES ({src_doc}, {link_index}, {normalized_target});",
            src_doc = sql_string_literal(&doc.path),
            link_index = index,
            normalized_target = sql_string_literal(link)
        ));
    }

    for (index, mentioned_path) in doc.path_mentions.iter().enumerate() {
        sql.push_str(&format!(
            "INSERT INTO path_mentions(doc_path, mention_index, mentioned_path) VALUES ({doc_path}, {mention_index}, {mentioned_path});",
            doc_path = sql_string_literal(&doc.path),
            mention_index = index,
            mentioned_path = sql_string_literal(mentioned_path)
        ));
    }

    for scope_path in &doc.scope_paths {
        sql.push_str(&format!(
            "INSERT INTO scope_paths(doc_path, scope_path) VALUES ({doc_path}, {scope_path});",
            doc_path = sql_string_literal(&doc.path),
            scope_path = sql_string_literal(scope_path)
        ));
    }

    for superseded in &doc.supersedes {
        sql.push_str(&format!(
            "INSERT INTO supersedes(doc_path, superseded_path) VALUES ({doc_path}, {superseded_path});",
            doc_path = sql_string_literal(&doc.path),
            superseded_path = sql_string_literal(superseded)
        ));
    }

    sql
}

fn render_doc_fact_sql(doc: &IndexedDoc) -> String {
    let mut sql = String::new();
    sql.push_str(&format!(
        "INSERT INTO docs(
            path, title, role, status, authority, collection_name, summary, frontmatter_keys_text, headings_text, content_hash, file_size_bytes,
            incoming_links, outgoing_links, is_entrypoint, is_anchor_candidate, is_generated, is_reference, mtime_unix
         ) VALUES (
            {path}, {title}, {role}, {status}, {authority}, {collection_name}, {summary}, {frontmatter_keys_text}, {headings_text}, {content_hash}, {file_size_bytes},
            0, 0, {is_entrypoint}, {is_anchor_candidate}, {is_generated}, {is_reference}, {mtime_unix}
         );",
        path = sql_string_literal(&doc.path),
        title = sql_string_literal(&doc.title),
        role = sql_string_literal(&doc.role),
        status = sql_opt_literal(doc.status.as_deref()),
        authority = sql_opt_literal(doc.authority.as_deref()),
        collection_name = sql_opt_literal(doc.collection.as_deref()),
        summary = sql_string_literal(&doc.summary),
        frontmatter_keys_text = sql_string_literal(&doc.frontmatter_keys.join(" | ")),
        headings_text = sql_string_literal(&doc.headings.join(" | ")),
        content_hash = sql_string_literal(&doc.content_hash),
        file_size_bytes = doc.file_size_bytes,
        is_entrypoint = sql_bool(doc.is_entrypoint),
        is_anchor_candidate = sql_bool(doc.is_anchor_candidate),
        is_generated = sql_bool(doc.is_generated),
        is_reference = sql_bool(doc.is_reference),
        mtime_unix = doc.mtime_unix
    ));
    sql.push_str(&render_doc_secondary_fact_sql(doc));
    sql
}

fn render_doc_full_sql(doc: &IndexedDoc) -> String {
    let mut sql = String::new();
    sql.push_str(&format!(
        "INSERT INTO docs(
            path, title, role, status, authority, collection_name, summary, frontmatter_keys_text, headings_text, content_hash, file_size_bytes,
            incoming_links, outgoing_links, is_entrypoint, is_anchor_candidate, is_generated, is_reference, mtime_unix
         ) VALUES (
            {path}, {title}, {role}, {status}, {authority}, {collection_name}, {summary}, {frontmatter_keys_text}, {headings_text}, {content_hash}, {file_size_bytes},
            {incoming_links}, {outgoing_links}, {is_entrypoint}, {is_anchor_candidate}, {is_generated}, {is_reference}, {mtime_unix}
         );",
        path = sql_string_literal(&doc.path),
        title = sql_string_literal(&doc.title),
        role = sql_string_literal(&doc.role),
        status = sql_opt_literal(doc.status.as_deref()),
        authority = sql_opt_literal(doc.authority.as_deref()),
        collection_name = sql_opt_literal(doc.collection.as_deref()),
        summary = sql_string_literal(&doc.summary),
        frontmatter_keys_text = sql_string_literal(&doc.frontmatter_keys.join(" | ")),
        headings_text = sql_string_literal(&doc.headings.join(" | ")),
        content_hash = sql_string_literal(&doc.content_hash),
        file_size_bytes = doc.file_size_bytes,
        incoming_links = doc.incoming_links,
        outgoing_links = doc.outgoing_links,
        is_entrypoint = sql_bool(doc.is_entrypoint),
        is_anchor_candidate = sql_bool(doc.is_anchor_candidate),
        is_generated = sql_bool(doc.is_generated),
        is_reference = sql_bool(doc.is_reference),
        mtime_unix = doc.mtime_unix
    ));

    sql.push_str(&render_doc_secondary_fact_sql(doc));
    sql
}

fn append_relation_insert_sql(sql: &mut String, relation: &DocRelation) {
    sql.push_str(&format!(
        "INSERT INTO relations(src_path, dst_path, relation_type, reason, explicit, confidence, evidence_json) VALUES ({src_path}, {dst_path}, {relation_type}, {reason}, {explicit}, {confidence}, {evidence_json});",
        src_path = sql_string_literal(&relation.src_path),
        dst_path = sql_string_literal(&relation.dst_path),
        relation_type = sql_string_literal(&relation.relation_type),
        reason = sql_string_literal(&relation.reason),
        explicit = sql_bool(relation.explicit),
        confidence = relation.confidence,
        evidence_json = sql_string_literal(&relation.evidence_json)
    ));
}

fn append_check_insert_sql(sql: &mut String, check: &IndexCheck) {
    sql.push_str(&format!(
        "INSERT INTO checks(rule_id, severity, subject_path, message, evidence_json) VALUES ({rule_id}, {severity}, {subject_path}, {message}, {evidence_json});",
        rule_id = sql_string_literal(&check.rule_id),
        severity = sql_string_literal(&check.severity),
        subject_path = sql_string_literal(&check.subject_path),
        message = sql_string_literal(&check.message),
        evidence_json = sql_string_literal(&check.evidence_json)
    ));
}

fn exec_sql_batches(db: &SqliteConnection, statements: &[String], batch_size: usize) -> Result<()> {
    if statements.is_empty() {
        return Ok(());
    }
    let chunk_size = batch_size.max(1);
    for chunk in statements.chunks(chunk_size) {
        let mut sql = String::from("BEGIN IMMEDIATE;");
        for statement in chunk {
            sql.push_str(statement);
        }
        sql.push_str("COMMIT;");
        db.exec(&sql)?;
    }
    Ok(())
}

fn replace_fact_store_contents(db: &SqliteConnection, artifact: &IndexArtifact) -> Result<()> {
    let mut sql = String::from(
        "BEGIN IMMEDIATE;
         DELETE FROM meta;
         DELETE FROM docs;
         DELETE FROM file_states;
         DELETE FROM headings;
         DELETE FROM raw_links;
         DELETE FROM path_mentions;
         DELETE FROM relations;
         DELETE FROM checks;
         DELETE FROM scope_paths;
         DELETE FROM supersedes;
         DELETE FROM docs_fts;",
    );

    sql.push_str(&format!(
        "INSERT INTO meta(key, value) VALUES
            ('repo_root', {repo_root}),
            ('managed_root', {managed_root}),
            ('generated_at_unix', {generated_at}),
            ('fact_schema_version', {fact_schema_version}),
            ('last_refresh_mode', 'full'),
            ('last_delta_added', '0'),
            ('last_delta_changed', '0'),
            ('last_delta_removed', '0'),
            ('history_latest_snapshot', {history_latest_snapshot}),
            ('history_dirty', {history_dirty}),
            ('history_tracked_files_count', {history_tracked_files_count}),
            ('host_git_head', {host_git_head}),
            ('host_git_branch', {host_git_branch}),
            ('host_git_dirty', {host_git_dirty});",
        repo_root = sql_string_literal(&artifact.repo_root),
        managed_root = sql_string_literal(&artifact.managed_root),
        generated_at = sql_string_literal(&artifact.generated_at_unix.to_string()),
        fact_schema_version = sql_string_literal(FACT_SCHEMA_VERSION),
        history_latest_snapshot =
            sql_string_literal(artifact.history_latest_snapshot.as_deref().unwrap_or("")),
        history_dirty = sql_string_literal(if artifact.history_dirty {
            "true"
        } else {
            "false"
        }),
        history_tracked_files_count =
            sql_string_literal(&artifact.history_tracked_files_count.to_string()),
        host_git_head = sql_string_literal(artifact.host_git_head.as_deref().unwrap_or("")),
        host_git_branch = sql_string_literal(artifact.host_git_branch.as_deref().unwrap_or("")),
        host_git_dirty = sql_string_literal(if artifact.host_git_dirty {
            "true"
        } else {
            "false"
        })
    ));

    for doc in &artifact.docs {
        sql.push_str(&render_doc_full_sql(doc));
    }

    for relation in &artifact.relations {
        append_relation_insert_sql(&mut sql, relation);
    }

    for check in &artifact.checks {
        append_check_insert_sql(&mut sql, check);
    }

    sql.push_str("COMMIT;");
    db.exec(&sql)
}

fn read_index_artifact_from_fact_store(db: &SqliteConnection) -> Result<IndexArtifact> {
    let meta = db.query("SELECT key, value FROM meta ORDER BY key ASC;")?;
    let docs_rows = db.query(
        "SELECT path, title, role, COALESCE(status, ''), COALESCE(authority, ''), COALESCE(collection_name, ''),
                summary, frontmatter_keys_text, headings_text, content_hash, file_size_bytes, incoming_links, outgoing_links,
                is_entrypoint, is_anchor_candidate, is_generated, is_reference, mtime_unix
         FROM docs
         ORDER BY path ASC;",
    )?;
    let headings_rows = db.query(
        "SELECT doc_path, heading_text FROM headings ORDER BY doc_path ASC, heading_index ASC;",
    )?;
    let links_rows = db.query(
        "SELECT src_doc, normalized_target FROM raw_links ORDER BY src_doc ASC, link_index ASC;",
    )?;
    let path_mentions_rows = db.query(
        "SELECT doc_path, mentioned_path FROM path_mentions ORDER BY doc_path ASC, mention_index ASC;",
    )?;
    let file_state_rows = db.query("SELECT path, mtime_ns FROM file_states ORDER BY path ASC;")?;
    let scope_rows = db.query(
        "SELECT doc_path, scope_path FROM scope_paths ORDER BY doc_path ASC, scope_path ASC;",
    )?;
    let supersedes_rows = db.query(
        "SELECT doc_path, superseded_path FROM supersedes ORDER BY doc_path ASC, superseded_path ASC;",
    )?;
    let relations_rows = db.query(
        "SELECT src_path, dst_path, relation_type, reason, explicit, confidence, evidence_json
         FROM relations
         ORDER BY src_path ASC, dst_path ASC, relation_type ASC;",
    )?;
    let checks_rows = db.query(
        "SELECT rule_id, severity, subject_path, message, evidence_json FROM checks ORDER BY subject_path ASC, rule_id ASC;",
    )?;

    let mut meta_map = BTreeMap::new();
    for row in &meta.rows {
        meta_map.insert(cell(row, 0).to_string(), cell(row, 1).to_string());
    }

    let headings_by_doc = group_second_column(&headings_rows);
    let links_by_doc = group_second_column(&links_rows);
    let path_mentions_by_doc = group_second_column(&path_mentions_rows);
    let scope_by_doc = group_second_column(&scope_rows);
    let supersedes_by_doc = group_second_column(&supersedes_rows);
    let mut mtime_ns_by_path = BTreeMap::new();
    for row in &file_state_rows.rows {
        mtime_ns_by_path.insert(
            cell(row, 0).to_string(),
            cell(row, 1).parse::<u64>().unwrap_or(0),
        );
    }

    let docs = docs_rows
        .rows
        .iter()
        .map(|row| IndexedDoc {
            path: cell(row, 0).to_string(),
            title: cell(row, 1).to_string(),
            summary: cell(row, 6).to_string(),
            frontmatter_keys: split_pipe_list(cell(row, 7)),
            content_text: String::new(),
            role: cell(row, 2).to_string(),
            status: empty_as_none(cell(row, 3)),
            authority: empty_as_none(cell(row, 4)),
            collection: empty_as_none(cell(row, 5)),
            headings: headings_by_doc
                .get(cell(row, 0))
                .cloned()
                .unwrap_or_default(),
            links: links_by_doc.get(cell(row, 0)).cloned().unwrap_or_default(),
            path_mentions: path_mentions_by_doc
                .get(cell(row, 0))
                .cloned()
                .unwrap_or_default(),
            scope_paths: scope_by_doc.get(cell(row, 0)).cloned().unwrap_or_default(),
            supersedes: supersedes_by_doc
                .get(cell(row, 0))
                .cloned()
                .unwrap_or_default(),
            incoming_links: cell(row, 11).parse::<usize>().unwrap_or(0),
            outgoing_links: cell(row, 12).parse::<usize>().unwrap_or(0),
            is_entrypoint: cell(row, 13) == "1",
            is_anchor_candidate: cell(row, 14) == "1",
            is_generated: cell(row, 15) == "1",
            is_reference: cell(row, 16) == "1",
            content_hash: cell(row, 9).to_string(),
            file_size_bytes: cell(row, 10).parse::<u64>().unwrap_or(0),
            mtime_unix: cell(row, 17).parse::<u64>().unwrap_or(0),
            mtime_ns: mtime_ns_by_path
                .get(cell(row, 0))
                .cloned()
                .unwrap_or_else(|| {
                    cell(row, 17)
                        .parse::<u64>()
                        .unwrap_or(0)
                        .saturating_mul(1_000_000_000)
                }),
        })
        .collect::<Vec<_>>();

    let relations = relations_rows
        .rows
        .iter()
        .map(|row| DocRelation {
            src_path: cell(row, 0).to_string(),
            dst_path: cell(row, 1).to_string(),
            relation_type: cell(row, 2).to_string(),
            reason: cell(row, 3).to_string(),
            explicit: cell(row, 4) == "1",
            confidence: cell(row, 5).parse::<f32>().unwrap_or(0.0),
            evidence_json: cell(row, 6).to_string(),
        })
        .collect::<Vec<_>>();

    let checks = checks_rows
        .rows
        .iter()
        .map(|row| IndexCheck {
            rule_id: cell(row, 0).to_string(),
            severity: cell(row, 1).to_string(),
            subject_path: cell(row, 2).to_string(),
            message: cell(row, 3).to_string(),
            evidence_json: cell(row, 4).to_string(),
        })
        .collect::<Vec<_>>();

    Ok(IndexArtifact {
        repo_root: meta_map.get("repo_root").cloned().unwrap_or_default(),
        managed_root: meta_map.get("managed_root").cloned().unwrap_or_default(),
        generated_at_unix: meta_map
            .get("generated_at_unix")
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(0),
        history_latest_snapshot: meta_map
            .get("history_latest_snapshot")
            .cloned()
            .filter(|value| !value.is_empty()),
        history_dirty: meta_map
            .get("history_dirty")
            .map(|value| value == "true")
            .unwrap_or(false),
        history_tracked_files_count: meta_map
            .get("history_tracked_files_count")
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(0),
        host_git_head: meta_map
            .get("host_git_head")
            .cloned()
            .filter(|value| !value.is_empty()),
        host_git_branch: meta_map
            .get("host_git_branch")
            .cloned()
            .filter(|value| !value.is_empty()),
        host_git_dirty: meta_map
            .get("host_git_dirty")
            .map(|value| value == "true")
            .unwrap_or(false),
        docs,
        relations,
        checks,
    })
}

fn group_second_column(result: &QueryResult) -> BTreeMap<String, Vec<String>> {
    let mut grouped = BTreeMap::<String, Vec<String>>::new();
    for row in &result.rows {
        let key = cell(row, 0).to_string();
        let value = cell(row, 1).to_string();
        if !key.is_empty() && !value.is_empty() {
            grouped.entry(key).or_default().push(value);
        }
    }
    grouped
}

fn split_pipe_list(value: &str) -> Vec<String> {
    value
        .split('|')
        .map(|item| item.trim())
        .filter(|item| !item.is_empty())
        .map(|item| item.to_string())
        .collect()
}

fn empty_as_none(value: &str) -> Option<String> {
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

fn collect_managed_doc_paths(repo_root: &Path, schema: &Schema) -> Result<Vec<String>> {
    let mut paths = Vec::new();

    for path in [
        schema.entrypoints.agents.as_ref(),
        schema.entrypoints.claude.as_ref(),
        schema.entrypoints.architecture.as_ref(),
    ]
    .into_iter()
    .flatten()
    {
        if repo_root.join(path).is_file() {
            paths.push(path.clone());
        }
    }

    for spec in &schema.core_files {
        let rel_path = if let Some(root_path) = &spec.root_path {
            root_path.clone()
        } else {
            format!("{}/{}", schema.managed_root, spec.path)
        };
        if repo_root.join(&rel_path).is_file() {
            paths.push(rel_path);
        }
    }

    for spec in &schema.doc_collections {
        let dir = repo_root.join(&schema.managed_root).join(&spec.path);
        if !dir.exists() {
            continue;
        }
        collect_markdown_files(repo_root, &dir, &mut paths)?;
    }

    paths.sort();
    paths.dedup();
    Ok(paths)
}

fn collect_markdown_files(repo_root: &Path, dir: &Path, out: &mut Vec<String>) -> Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_markdown_files(repo_root, &path, out)?;
            continue;
        }
        if path.extension().and_then(|ext| ext.to_str()) == Some("md") {
            let rel = path
                .strip_prefix(repo_root)
                .map(normalize_path)
                .unwrap_or_else(|_| normalize_path(&path));
            out.push(rel);
        }
    }
    Ok(())
}

fn read_text_lossy(path: &Path) -> Result<String> {
    let bytes = fs::read(path)?;
    Ok(String::from_utf8_lossy(&bytes).to_string())
}

fn parse_doc_text(path: &str, text: &str) -> ParsedDoc {
    let normalized_text;
    let text = if text.contains("\r\n") {
        normalized_text = text.replace("\r\n", "\n");
        normalized_text.as_str()
    } else {
        text
    };
    let (frontmatter, body) = split_frontmatter(text);
    let frontmatter_map = frontmatter.map(parse_frontmatter).unwrap_or_default();
    let scope_paths = frontmatter.map(parse_scope_paths).unwrap_or_else(Vec::new);
    let supersedes = frontmatter.map(parse_supersedes).unwrap_or_else(Vec::new);

    let mut title = None;
    let mut headings = Vec::new();
    let mut links = Vec::new();
    let path_mentions = extract_inline_code_path_mentions(body);
    let mut in_fence = false;
    for line in body.lines() {
        let is_fence_line = is_backtick_fence_line(line);
        let line_in_fence = in_fence;
        if is_fence_line {
            in_fence = !in_fence;
        }
        if !line_in_fence && !is_fence_line {
            if let Some((level, heading)) = parse_heading(line) {
                if level == 1 && title.is_none() {
                    title = Some(heading.clone());
                }
                headings.push(heading);
            }
            for (_text, target) in extract_markdown_links(line) {
                if let Some(normalized) = normalize_link(path, &target) {
                    links.push(normalized);
                }
            }
        }
    }

    ParsedDoc {
        frontmatter: frontmatter_map,
        title,
        summary: extract_summary(body),
        body_text: body.to_string(),
        headings,
        links,
        path_mentions,
        scope_paths,
        supersedes,
    }
}

fn split_frontmatter(text: &str) -> (Option<&str>, &str) {
    if !text.starts_with("---\n") {
        return (None, text);
    }
    if let Some(end) = text[4..].find("\n---") {
        let fm_end = 4 + end;
        let after_marker = fm_end + 4;
        if after_marker == text.len() {
            (Some(&text[4..fm_end]), "")
        } else if text[after_marker..].starts_with('\n') {
            (Some(&text[4..fm_end]), &text[after_marker + 1..])
        } else {
            (None, text)
        }
    } else {
        (None, text)
    }
}

fn parse_frontmatter(raw: &str) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with("- ") {
            continue;
        }
        if let Some((key, value)) = trimmed.split_once(':') {
            let value = value.trim().trim_matches('"').trim_matches('\'');
            if !value.is_empty() {
                map.insert(key.trim().to_string(), value.to_string());
            }
        }
    }
    map
}

fn parse_scope_paths(raw: &str) -> Vec<String> {
    parse_frontmatter_list(raw, &["scope_paths", "source_paths"])
}

fn parse_supersedes(raw: &str) -> Vec<String> {
    let mut values = parse_frontmatter_list(raw, &["supersedes"]);
    for value in &mut values {
        let normalized = normalize_path(Path::new(value));
        *value = normalized;
    }
    values.sort();
    values.dedup();
    values
}

fn parse_frontmatter_list(raw: &str, keys: &[&str]) -> Vec<String> {
    let mut values = Vec::new();
    let mut active_key: Option<String> = None;

    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = trimmed.split_once(':') {
            let key = key.trim();
            if keys.contains(&key) {
                active_key = Some(key.to_string());
                let value = value.trim();
                if value.starts_with('[') && value.ends_with(']') {
                    for item in value.trim_matches(&['[', ']'][..]).split(',') {
                        push_list_value(&mut values, item);
                    }
                } else if !value.is_empty() {
                    push_list_value(&mut values, value);
                }
                continue;
            }
            active_key = None;
        } else if trimmed.starts_with("- ") && active_key.is_some() {
            push_list_value(&mut values, trimmed.trim_start_matches("- "));
        }
    }

    values.sort();
    values.dedup();
    values
}

fn push_list_value(values: &mut Vec<String>, raw: &str) {
    let value = raw
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
        .trim_end_matches('/')
        .to_string();
    if !value.is_empty() {
        values.push(value);
    }
}

fn parse_heading(line: &str) -> Option<(u32, String)> {
    let trimmed = line.trim_start();
    let hashes = trimmed.chars().take_while(|c| *c == '#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let rest = trimmed[hashes..].trim();
    if rest.is_empty() {
        None
    } else {
        Some((hashes as u32, rest.trim_matches('#').trim().to_string()))
    }
}

fn leading_backtick_run(line: &str) -> Option<usize> {
    let trimmed = line.trim_start();
    let run = trimmed.chars().take_while(|ch| *ch == '`').count();
    if run > 0 {
        Some(run)
    } else {
        None
    }
}

fn is_backtick_fence_line(line: &str) -> bool {
    leading_backtick_run(line)
        .map(|run| run >= 3)
        .unwrap_or(false)
}

fn extract_markdown_links(line: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'[' {
            if let Some(close) = line[i + 1..].find(']') {
                let close_idx = i + 1 + close;
                if line[close_idx..].starts_with("](") {
                    if let Some(end) = line[close_idx + 2..].find(')') {
                        let text = &line[i + 1..close_idx];
                        let target = &line[close_idx + 2..close_idx + 2 + end];
                        out.push((text.to_string(), target.to_string()));
                        i = close_idx + 3 + end;
                        continue;
                    }
                }
            }
        }
        i += 1;
    }
    out
}

fn normalize_link(src_doc: &str, raw: &str) -> Option<String> {
    if raw.starts_with("http://")
        || raw.starts_with("https://")
        || raw.starts_with("mailto:")
        || raw.starts_with('#')
    {
        return None;
    }
    let target = raw.split('#').next().unwrap_or(raw).trim();
    if target.is_empty() {
        return None;
    }
    let joined = if target.starts_with('/') {
        PathBuf::from(target.trim_start_matches('/'))
    } else {
        Path::new(src_doc)
            .parent()
            .unwrap_or_else(|| Path::new(""))
            .join(target)
    };
    Some(normalize_path(&joined))
}

fn normalize_path(path: &Path) -> String {
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                parts.pop();
            }
            Component::Normal(text) => parts.push(text.to_string_lossy().to_string()),
            Component::RootDir | Component::Prefix(_) => {}
        }
    }
    parts.join("/")
}

fn absolute_display_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn simple_content_hash(text: &str) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in text.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{:016x}", hash)
}

fn history_content_hash(bytes: &[u8]) -> String {
    let mut left: u64 = 0xcbf29ce484222325;
    let mut right: u64 = 0x84222325cbf29ce4;
    for byte in bytes {
        left ^= *byte as u64;
        left = left.wrapping_mul(0x100000001b3);
        right ^= (*byte as u64).wrapping_add(0x9e3779b97f4a7c15);
        right = right.rotate_left(5).wrapping_mul(0x100000001b3);
    }
    format!("{:016x}{:016x}", left, right)
}

fn extract_summary(body: &str) -> String {
    let mut paragraph = Vec::new();
    let mut in_fence = false;
    for line in body.lines() {
        let line_in_fence = in_fence;
        let is_fence_line = is_backtick_fence_line(line);
        if is_fence_line {
            in_fence = !in_fence;
        }
        if line_in_fence || is_fence_line {
            if !paragraph.is_empty() {
                break;
            }
            continue;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if !paragraph.is_empty() {
                break;
            }
            continue;
        }
        if trimmed.starts_with('#') || trimmed.starts_with("- ") {
            if !paragraph.is_empty() {
                break;
            }
            continue;
        }
        paragraph.push(trimmed);
    }
    paragraph.join(" ")
}

fn collection_lookup(schema: &Schema) -> BTreeMap<String, &DocCollectionSpec> {
    let mut map = BTreeMap::new();
    for spec in &schema.doc_collections {
        map.insert(spec.name.clone(), spec);
    }
    map
}

fn collection_name_for_path(schema: &Schema, path: &str) -> Option<String> {
    for spec in &schema.doc_collections {
        let prefix = format!(
            "{}/{}/",
            schema.managed_root,
            spec.path.trim_end_matches('/')
        );
        if path.starts_with(&prefix) {
            return Some(spec.name.clone());
        }
    }
    None
}

fn infer_index_role(schema: &Schema, path: &str, collection: Option<&str>) -> String {
    if is_entrypoint_path(schema, path) {
        "entrypoint".to_string()
    } else if configured_architecture_path(schema).as_deref() == Some(path) {
        "architecture".to_string()
    } else if path == schema.entrypoints.manifest.as_deref().unwrap_or("") {
        "manifest".to_string()
    } else if path.ends_with("/index.md") {
        "collection-index".to_string()
    } else {
        collection.unwrap_or("doc").to_string()
    }
}

fn is_entrypoint_path(schema: &Schema, path: &str) -> bool {
    schema.entrypoints.agents.as_deref() == Some(path)
        || schema.entrypoints.claude.as_deref() == Some(path)
        || schema.entrypoints.architecture.as_deref() == Some(path)
}

fn is_anchor_candidate(schema: &Schema, path: &str) -> bool {
    if is_entrypoint_path(schema, path) {
        return true;
    }
    if path == schema.entrypoints.manifest.as_deref().unwrap_or("") {
        return true;
    }
    if path == schema.entrypoints.project_map.as_deref().unwrap_or("") {
        return true;
    }
    if schema
        .core_files
        .iter()
        .any(|spec| core_file_rel_path(schema, spec) == path)
    {
        return true;
    }
    collection_name_for_path(schema, path)
        .and_then(|name| schema.doc_collections.iter().find(|spec| spec.name == name))
        .map(|spec| spec.anchor_candidate)
        .unwrap_or(false)
}

fn core_file_rel_path(schema: &Schema, spec: &CoreFileSpec) -> String {
    spec.root_path
        .clone()
        .unwrap_or_else(|| format!("{}/{}", schema.managed_root, spec.path))
}

fn build_doc_relations(
    schema: &Schema,
    docs: &[IndexedDoc],
    path_set: &BTreeSet<String>,
) -> Vec<DocRelation> {
    let manifest_path = schema
        .entrypoints
        .manifest
        .clone()
        .unwrap_or_else(|| format!("{}/index.md", schema.managed_root));
    let mut relations = Vec::new();
    let mut seen = BTreeSet::new();

    for doc in docs {
        if let Some(collection) = &doc.collection {
            if let Some(spec) = schema
                .doc_collections
                .iter()
                .find(|item| item.name == *collection)
            {
                let collection_index = format!("{}/{}/index.md", schema.managed_root, spec.path);
                if doc.path != collection_index {
                    push_relation(
                        &mut relations,
                        &mut seen,
                        collection_index,
                        doc.path.clone(),
                        "doc_indexes_doc",
                        "collection-membership",
                        true,
                    );
                }
            }
        }

        for link in &doc.links {
            if path_set.contains(link) {
                let relation_type = if doc.path == manifest_path
                    || is_entrypoint_path(schema, &doc.path)
                    || doc.path.ends_with("/index.md")
                {
                    "doc_indexes_doc"
                } else {
                    "doc_links_doc"
                };
                push_relation(
                    &mut relations,
                    &mut seen,
                    doc.path.clone(),
                    link.clone(),
                    relation_type,
                    "markdown-link",
                    true,
                );
            }
        }

        for mentioned in &doc.path_mentions {
            if path_set.contains(mentioned) {
                let relation_type = if is_navigation_doc(doc) {
                    "doc_indexes_doc"
                } else {
                    "doc_mentions_code_path"
                };
                push_relation(
                    &mut relations,
                    &mut seen,
                    doc.path.clone(),
                    mentioned.clone(),
                    relation_type,
                    "inline-code-path",
                    true,
                );
            }
        }

        for scope_path in &doc.scope_paths {
            push_relation(
                &mut relations,
                &mut seen,
                doc.path.clone(),
                scope_path.clone(),
                "doc_scopes_code",
                "frontmatter.scope_paths",
                true,
            );
            push_relation(
                &mut relations,
                &mut seen,
                scope_path.clone(),
                doc.path.clone(),
                "code_mentions_doc",
                "frontmatter.scope_paths.reverse",
                false,
            );
        }

        for superseded in &doc.supersedes {
            push_relation(
                &mut relations,
                &mut seen,
                doc.path.clone(),
                superseded.clone(),
                "supersedes",
                "frontmatter.supersedes",
                true,
            );
        }
    }

    relations
}

fn is_navigation_doc(doc: &IndexedDoc) -> bool {
    doc.is_entrypoint
        || matches!(
            doc.role.as_str(),
            "manifest" | "collection-index" | "architecture"
        )
}

fn extract_inline_code_path_mentions(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut mentions = Vec::new();
    let mut seen = BTreeSet::new();
    let mut current = String::new();
    let mut in_inline_code = false;
    let mut in_fence = false;
    let mut index = 0;

    while index < chars.len() {
        if chars[index] == '`' {
            let mut run = 1;
            while index + run < chars.len() && chars[index + run] == '`' {
                run += 1;
            }

            if run >= 3 && is_line_leading_backtick_run(&chars, index) {
                if !in_inline_code {
                    in_fence = !in_fence;
                } else {
                    for _ in 0..run {
                        current.push('`');
                    }
                }
                index += run;
                continue;
            }

            if run == 1 && !in_fence {
                if in_inline_code {
                    for candidate in extract_path_candidates(&current) {
                        if seen.insert(candidate.clone()) {
                            mentions.push(candidate);
                        }
                    }
                    current.clear();
                    in_inline_code = false;
                } else {
                    in_inline_code = true;
                }
                index += 1;
                continue;
            }

            if in_inline_code {
                for _ in 0..run {
                    current.push('`');
                }
            }
            index += run;
            continue;
        }

        if in_inline_code {
            current.push(chars[index]);
        }
        index += 1;
    }

    mentions
}

fn is_line_leading_backtick_run(chars: &[char], index: usize) -> bool {
    let line_start = chars[..index]
        .iter()
        .rposition(|ch| *ch == '\n')
        .map(|position| position + 1)
        .unwrap_or(0);
    chars[line_start..index].iter().all(|ch| ch.is_whitespace())
}

fn extract_path_candidates(text: &str) -> Vec<String> {
    let prefixes = [
        "docs/",
        "src/",
        "tests/",
        "test/",
        "crates/",
        "packages/",
        "AGENTS.md",
        "CLAUDE.md",
        "ARCHITECTURE.md",
        "README.md",
    ];
    let mut paths = Vec::new();
    let mut seen = BTreeSet::new();
    for token in text.split(|c: char| {
        c.is_whitespace() || matches!(c, '`' | '"' | '\'' | '(' | ')' | '[' | ']' | ',' | ';')
    }) {
        let normalized = token
            .trim()
            .trim_matches(|c: char| matches!(c, '.' | ':' | ',' | ')' | '(' | '"' | '\'' | '`'))
            .trim_end_matches('/')
            .trim_start_matches("./")
            .to_string();
        if normalized.len() < 3 {
            continue;
        }
        if prefixes.iter().any(|prefix| normalized.starts_with(prefix))
            && seen.insert(normalized.clone())
        {
            paths.push(normalized);
        }
    }
    paths
}

fn push_relation(
    relations: &mut Vec<DocRelation>,
    seen: &mut BTreeSet<(String, String, String)>,
    src_path: String,
    dst_path: String,
    relation_type: &str,
    reason: &str,
    explicit: bool,
) {
    let key = (
        src_path.clone(),
        dst_path.clone(),
        relation_type.to_string(),
    );
    if seen.insert(key) {
        relations.push(DocRelation {
            src_path,
            dst_path,
            relation_type: relation_type.to_string(),
            reason: reason.to_string(),
            explicit,
            confidence: relation_confidence(relation_type, explicit),
            evidence_json: relation_evidence_json(reason),
        });
    }
}

fn relation_confidence(relation_type: &str, explicit: bool) -> f32 {
    if explicit {
        match relation_type {
            "doc_scopes_code" | "supersedes" => 1.0,
            "doc_indexes_doc" => 0.95,
            "doc_links_doc" => 0.9,
            "doc_mentions_code_path" => 0.75,
            "code_mentions_doc" => 0.55,
            _ => 0.8,
        }
    } else {
        0.5
    }
}

fn relation_evidence_json(reason: &str) -> String {
    format!("{{\"reason\":{}}}", json_string(reason))
}

fn annotate_link_counts(docs: &mut [IndexedDoc], relations: &[DocRelation]) {
    let mut incoming = BTreeMap::<String, usize>::new();
    let mut outgoing = BTreeMap::<String, usize>::new();

    for relation in relations {
        if !counts_for_doc_navigation(relation) {
            continue;
        }
        if relation.dst_path.ends_with(".md") {
            *incoming.entry(relation.dst_path.clone()).or_insert(0) += 1;
        }
        if relation.src_path.ends_with(".md") {
            *outgoing.entry(relation.src_path.clone()).or_insert(0) += 1;
        }
    }

    for doc in docs {
        doc.incoming_links = incoming.get(&doc.path).copied().unwrap_or(0);
        doc.outgoing_links = outgoing.get(&doc.path).copied().unwrap_or(0);
    }
}

fn counts_for_doc_navigation(relation: &DocRelation) -> bool {
    matches!(
        relation.relation_type.as_str(),
        "doc_indexes_doc" | "doc_links_doc" | "supersedes"
    )
}

fn build_index_checks(
    schema: &Schema,
    repo_root: &Path,
    docs: &[IndexedDoc],
    relations: &[DocRelation],
) -> Vec<IndexCheck> {
    let mut checks = Vec::new();
    let doc_map: BTreeMap<String, &IndexedDoc> =
        docs.iter().map(|doc| (doc.path.clone(), doc)).collect();
    let manifest_path = schema
        .entrypoints
        .manifest
        .clone()
        .unwrap_or_else(|| format!("{}/index.md", schema.managed_root));

    for (entrypoint_name, entrypoint_path) in [
        ("agents", schema.entrypoints.agents.as_deref()),
        ("claude", schema.entrypoints.claude.as_deref()),
        ("architecture", schema.entrypoints.architecture.as_deref()),
    ] {
        if let Some(path) = entrypoint_path {
            if !doc_map.contains_key(path) {
                push_check(
                    &mut checks,
                    "missing-entrypoint-doc",
                    "warning",
                    path,
                    &format!(
                        "configured {} entrypoint is missing: {}",
                        entrypoint_name, path
                    ),
                    &[("entrypoint", entrypoint_name), ("path", path)],
                );
            }
        }
    }

    for spec in &schema.core_files {
        let rel_path = if let Some(root_path) = &spec.root_path {
            root_path.clone()
        } else {
            format!("{}/{}", schema.managed_root, spec.path)
        };
        if spec.required && !doc_map.contains_key(&rel_path) {
            push_check(
                &mut checks,
                "missing-required-doc",
                "warning",
                &rel_path,
                &format!("required doc is missing: {}", rel_path),
                &[("path", &rel_path), ("schema_item", &spec.name)],
            );
        }
    }

    if schema.rules.index_required.unwrap_or(false) {
        for spec in &schema.doc_collections {
            let collection_index_path = format!("{}/{}/index.md", schema.managed_root, spec.path);
            if !doc_map.contains_key(&collection_index_path) {
                push_check(
                    &mut checks,
                    "missing-collection-index",
                    "warning",
                    &collection_index_path,
                    &format!("collection index is missing: {}", collection_index_path),
                    &[("collection", &spec.name), ("path", &collection_index_path)],
                );
            }
        }
    }

    for path in [
        schema.entrypoints.agents.as_deref(),
        schema.entrypoints.claude.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        if doc_map.contains_key(path) {
            let points_to_manifest = relations
                .iter()
                .any(|relation| relation.src_path == path && relation.dst_path == manifest_path);
            if !points_to_manifest {
                push_check(
                    &mut checks,
                    "entrypoint-manifest-link",
                    "warning",
                    path,
                    &format!("{} does not point to {}", path, manifest_path),
                    &[("entrypoint", path), ("manifest", &manifest_path)],
                );
            }
        }
    }

    if schema.rules.index_required.unwrap_or(false) {
        add_manifest_constraint_checks(schema, docs, relations, &doc_map, &mut checks);
    }

    for doc in docs
        .iter()
        .filter(|doc| doc.collection.is_some() && !doc.path.ends_with("/index.md"))
    {
        if doc.incoming_links == 0 {
            push_check(
                &mut checks,
                "orphan-managed-doc",
                "info",
                &doc.path,
                &format!("{} has no incoming managed-doc navigation edges", doc.path),
                &[("path", &doc.path)],
            );
        }
    }

    if schema.rules.stale_explicit_anchor.unwrap_or(false) {
        add_stale_anchor_checks(repo_root, docs, &mut checks);
    }
    if schema.rules.duplicate_detection.unwrap_or(false) {
        add_duplicate_checks(docs, &mut checks);
    }
    if schema.rules.contamination_checks.unwrap_or(false) {
        add_contamination_checks(docs, relations, &mut checks);
    }
    if schema.rules.minimal_frontmatter_only.unwrap_or(false) {
        add_frontmatter_constraint_checks(schema, docs, &mut checks);
    }
    add_history_checks(schema, repo_root, &mut checks);

    checks
}

fn push_check(
    checks: &mut Vec<IndexCheck>,
    rule_id: &str,
    severity: &str,
    subject_path: &str,
    message: &str,
    evidence: &[(&str, &str)],
) {
    checks.push(IndexCheck {
        rule_id: rule_id.to_string(),
        severity: severity.to_string(),
        subject_path: subject_path.to_string(),
        message: message.to_string(),
        evidence_json: evidence_object_json(evidence),
    });
}

fn evidence_object_json(items: &[(&str, &str)]) -> String {
    let entries = items
        .iter()
        .map(|(key, value)| format!("{}:{}", json_string(key), json_string(value)))
        .collect::<Vec<_>>()
        .join(",");
    format!("{{{}}}", entries)
}

fn add_manifest_constraint_checks(
    schema: &Schema,
    docs: &[IndexedDoc],
    relations: &[DocRelation],
    doc_map: &BTreeMap<String, &IndexedDoc>,
    checks: &mut Vec<IndexCheck>,
) {
    let manifest_path = schema.entrypoints.manifest.as_deref().unwrap_or("");
    if manifest_path.is_empty() || !doc_map.contains_key(manifest_path) {
        return;
    }

    let manifest_links = relations
        .iter()
        .filter(|relation| relation.src_path == manifest_path && relation.dst_path.ends_with(".md"))
        .map(|relation| relation.dst_path.clone())
        .collect::<BTreeSet<_>>();

    for doc in docs {
        if doc.path == manifest_path
            || doc.is_entrypoint
            || doc
                .path
                .starts_with(&format!("{}/templates/", schema.managed_root))
        {
            continue;
        }
        let is_core = schema.core_files.iter().any(|spec| {
            let expected = spec
                .root_path
                .clone()
                .unwrap_or_else(|| format!("{}/{}", schema.managed_root, spec.path));
            expected == doc.path
        });
        let should_be_manifested = is_core || doc.path.ends_with("/index.md");
        if should_be_manifested && !manifest_links.contains(&doc.path) {
            push_check(
                checks,
                "manifest-missing-doc",
                "warning",
                &doc.path,
                &format!("{} is not linked from {}", doc.path, manifest_path),
                &[("manifest", manifest_path), ("missing_doc", &doc.path)],
            );
        }
    }

    for doc in docs.iter().filter(|doc| is_navigation_doc(doc)) {
        for link in &doc.links {
            if link.ends_with(".md") && !doc_map.contains_key(link) {
                push_check(
                    checks,
                    "unresolved-managed-link",
                    "warning",
                    &doc.path,
                    &format!("{} links to missing managed doc {}", doc.path, link),
                    &[("source", &doc.path), ("target", link)],
                );
            }
            if link.starts_with(&format!("{}/", schema.managed_root)) && !link.ends_with(".md") {
                push_check(
                    checks,
                    "wrong-docs-navigation-target",
                    "info",
                    &doc.path,
                    &format!("{} navigates to non-document target {}", doc.path, link),
                    &[("source", &doc.path), ("target", link)],
                );
            }
        }
    }
}

fn add_stale_anchor_checks(repo_root: &Path, docs: &[IndexedDoc], checks: &mut Vec<IndexCheck>) {
    for doc in docs {
        for scope_path in &doc.scope_paths {
            let scoped_mtime = newest_existing_path_mtime(&repo_root.join(scope_path));
            if let Some(scoped_mtime) = scoped_mtime {
                if scoped_mtime > doc.mtime_unix {
                    push_check(
                        checks,
                        "stale-explicit-anchor",
                        "warning",
                        &doc.path,
                        &format!(
                            "{} may be stale: scoped path {} is newer than the doc",
                            doc.path, scope_path
                        ),
                        &[
                            ("doc", &doc.path),
                            ("scope_path", scope_path),
                            ("doc_mtime_unix", &doc.mtime_unix.to_string()),
                            ("scope_mtime_unix", &scoped_mtime.to_string()),
                        ],
                    );
                }
            } else {
                push_check(
                    checks,
                    "missing-scope-path",
                    "info",
                    &doc.path,
                    &format!("{} scopes missing path {}", doc.path, scope_path),
                    &[("doc", &doc.path), ("scope_path", scope_path)],
                );
            }
        }
    }
}

fn newest_existing_path_mtime(path: &Path) -> Option<u64> {
    if path.is_file() {
        return file_mtime(path);
    }
    if path.is_dir() {
        return newest_dir_mtime(path);
    }
    None
}

fn newest_dir_mtime(path: &Path) -> Option<u64> {
    let mut newest = file_mtime(path);
    let entries = fs::read_dir(path).ok()?;
    for entry in entries.flatten() {
        let child = entry.path();
        let child_mtime = if child.is_dir() {
            newest_dir_mtime(&child)
        } else {
            file_mtime(&child)
        };
        if let Some(value) = child_mtime {
            newest = Some(newest.map(|current| current.max(value)).unwrap_or(value));
        }
    }
    newest
}

fn file_mtime(path: &Path) -> Option<u64> {
    fs::metadata(path)
        .ok()
        .and_then(|meta| meta.modified().ok())
        .and_then(|mtime| mtime.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs())
}

fn add_duplicate_checks(docs: &[IndexedDoc], checks: &mut Vec<IndexCheck>) {
    let mut by_hash = BTreeMap::<String, Vec<&IndexedDoc>>::new();
    let mut by_fingerprint = BTreeMap::<String, Vec<&IndexedDoc>>::new();

    for doc in docs {
        by_hash
            .entry(doc.content_hash.clone())
            .or_default()
            .push(doc);
        let fingerprint = doc_structure_fingerprint(doc);
        if !fingerprint.is_empty() {
            by_fingerprint.entry(fingerprint).or_default().push(doc);
        }
    }

    for group in by_hash.values().filter(|group| group.len() > 1) {
        let paths = group
            .iter()
            .map(|doc| doc.path.clone())
            .collect::<Vec<_>>()
            .join(" | ");
        for doc in group {
            push_check(
                checks,
                "duplicate-content",
                "warning",
                &doc.path,
                &format!("{} has identical content with {}", doc.path, paths),
                &[("paths", &paths), ("content_hash", &doc.content_hash)],
            );
        }
    }

    for group in by_fingerprint.values().filter(|group| group.len() > 1) {
        let paths = group
            .iter()
            .map(|doc| doc.path.clone())
            .collect::<Vec<_>>()
            .join(" | ");
        for doc in group {
            push_check(
                checks,
                "overlap-heading-fingerprint",
                "info",
                &doc.path,
                &format!(
                    "{} has a similar title/headings structure with {}",
                    doc.path, paths
                ),
                &[
                    ("paths", &paths),
                    ("fingerprint", &doc_structure_fingerprint(doc)),
                ],
            );
        }
    }
}

fn doc_structure_fingerprint(doc: &IndexedDoc) -> String {
    let title = normalize_fingerprint_text(&doc.title);
    let headings = doc
        .headings
        .iter()
        .take(6)
        .map(|heading| normalize_fingerprint_text(heading))
        .filter(|heading| !heading.is_empty())
        .collect::<Vec<_>>()
        .join("/");
    if title.is_empty() || headings.is_empty() {
        String::new()
    } else {
        format!("{}::{}", title, headings)
    }
}

fn normalize_fingerprint_text(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn add_contamination_checks(
    docs: &[IndexedDoc],
    relations: &[DocRelation],
    checks: &mut Vec<IndexCheck>,
) {
    let doc_map: BTreeMap<String, &IndexedDoc> =
        docs.iter().map(|doc| (doc.path.clone(), doc)).collect();
    for relation in relations {
        let Some(src) = doc_map.get(&relation.src_path) else {
            continue;
        };
        let Some(dst) = doc_map.get(&relation.dst_path) else {
            continue;
        };
        if src.authority.as_deref() == Some("canonical") && (dst.is_generated || dst.is_reference) {
            let rule = if dst.is_generated {
                "canonical-links-generated"
            } else {
                "canonical-links-reference"
            };
            push_check(
                checks,
                rule,
                "warning",
                &src.path,
                &format!(
                    "canonical doc {} links to non-source doc {}",
                    src.path, dst.path
                ),
                &[
                    ("source", &src.path),
                    ("target", &dst.path),
                    ("relation_type", &relation.relation_type),
                ],
            );
        }
    }
}

fn add_frontmatter_constraint_checks(
    schema: &Schema,
    docs: &[IndexedDoc],
    checks: &mut Vec<IndexCheck>,
) {
    let collection_by_name = collection_lookup(schema);
    for doc in docs {
        let Some(collection_name) = &doc.collection else {
            continue;
        };
        let Some(spec) = collection_by_name.get(collection_name) else {
            continue;
        };
        let allowed = spec
            .allowed_frontmatter
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        for key in &doc.frontmatter_keys {
            if !allowed.contains(key) {
                push_check(
                    checks,
                    "frontmatter-field-not-allowed",
                    "info",
                    &doc.path,
                    &format!(
                        "{} uses frontmatter field `{}` outside this collection's allowlist",
                        doc.path, key
                    ),
                    &[
                        ("doc", &doc.path),
                        ("field", key),
                        ("collection", collection_name),
                    ],
                );
            }
        }
    }
}

fn add_history_checks(schema: &Schema, repo_root: &Path, checks: &mut Vec<IndexCheck>) {
    let expected_excludes = managed_memory_paths(schema);
    match find_host_git_dir(repo_root) {
        Some(git_dir) => {
            let exclude_path = git_dir.join("info").join("exclude");
            let exclude_text = fs::read_to_string(&exclude_path).unwrap_or_default();
            if exclude_text.trim().is_empty() {
                push_check(
                    checks,
                    "host-git-exclude-missing",
                    "warning",
                    ".git/info/exclude",
                    "host git local exclude does not contain HarnessKit isolation rules",
                    &[("path", ".git/info/exclude")],
                );
            }
            for path in expected_excludes {
                if !exclude_text.lines().any(|line| line.trim() == path) {
                    push_check(
                        checks,
                        "managed-file-not-excluded",
                        "warning",
                        &path,
                        &format!("{} is not listed in host git local exclude", path),
                        &[("path", &path)],
                    );
                }
            }
        }
        None => {
            push_check(
                checks,
                "host-git-exclude-missing",
                "info",
                ".git/info/exclude",
                "host git repo not found; HarnessKit cannot install local exclude rules",
                &[("path", ".git/info/exclude")],
            );
        }
    }

    if !history_dir(repo_root).exists() || latest_history_snapshot_id(repo_root).is_none() {
        push_check(
            checks,
            "history-missing",
            "warning",
            ".harnesskit/history",
            "no HarnessKit history snapshot exists",
            &[("path", ".harnesskit/history")],
        );
        return;
    }

    if let Ok(status) = compute_history_status(repo_root, schema) {
        if has_history_doc_changes(&status) {
            push_check(
                checks,
                "history-dirty-since-snapshot",
                "warning",
                ".harnesskit/history",
                "managed docs differ from latest HarnessKit history snapshot",
                &[
                    (
                        "latest_snapshot",
                        status.latest_snapshot.as_deref().unwrap_or(""),
                    ),
                    ("added", &status.added.len().to_string()),
                    ("changed", &status.changed.len().to_string()),
                    ("removed", &status.removed.len().to_string()),
                ],
            );
        }
        if status.schema_changed {
            push_check(
                checks,
                "history-schema-not-snapshotted",
                "warning",
                ".harnesskit/schema.yaml",
                "HarnessKit schema differs from latest history snapshot",
                &[("path", ".harnesskit/schema.yaml")],
            );
        }
    }
}

fn render_index_json(artifact: &IndexArtifact) -> String {
    let mut out = String::new();
    out.push_str("{\n");
    out.push_str(&format!(
        "  \"repo_root\": {},\n",
        json_string(&artifact.repo_root)
    ));
    out.push_str(&format!(
        "  \"managed_root\": {},\n",
        json_string(&artifact.managed_root)
    ));
    out.push_str(&format!(
        "  \"generated_at_unix\": {},\n",
        artifact.generated_at_unix
    ));
    out.push_str(&format!(
        "  \"history_latest_snapshot\": {},\n",
        json_opt_string(artifact.history_latest_snapshot.as_deref())
    ));
    out.push_str(&format!(
        "  \"history_dirty\": {},\n",
        json_bool(artifact.history_dirty)
    ));
    out.push_str(&format!(
        "  \"history_tracked_files_count\": {},\n",
        artifact.history_tracked_files_count
    ));
    out.push_str(&format!(
        "  \"host_git_head\": {},\n",
        json_opt_string(artifact.host_git_head.as_deref())
    ));
    out.push_str(&format!(
        "  \"host_git_branch\": {},\n",
        json_opt_string(artifact.host_git_branch.as_deref())
    ));
    out.push_str(&format!(
        "  \"host_git_dirty\": {},\n",
        json_bool(artifact.host_git_dirty)
    ));
    out.push_str("  \"docs\": [\n");
    for (idx, doc) in artifact.docs.iter().enumerate() {
        out.push_str("    {\n");
        out.push_str(&format!("      \"path\": {},\n", json_string(&doc.path)));
        out.push_str(&format!("      \"title\": {},\n", json_string(&doc.title)));
        out.push_str(&format!(
            "      \"summary\": {},\n",
            json_string(&doc.summary)
        ));
        out.push_str(&format!("      \"role\": {},\n", json_string(&doc.role)));
        out.push_str(&format!(
            "      \"status\": {},\n",
            json_opt_string(doc.status.as_deref())
        ));
        out.push_str(&format!(
            "      \"authority\": {},\n",
            json_opt_string(doc.authority.as_deref())
        ));
        out.push_str(&format!(
            "      \"collection\": {},\n",
            json_opt_string(doc.collection.as_deref())
        ));
        out.push_str(&format!(
            "      \"headings\": {},\n",
            json_string_array(&doc.headings)
        ));
        out.push_str(&format!(
            "      \"links\": {},\n",
            json_string_array(&doc.links)
        ));
        out.push_str(&format!(
            "      \"scope_paths\": {},\n",
            json_string_array(&doc.scope_paths)
        ));
        out.push_str(&format!(
            "      \"supersedes\": {},\n",
            json_string_array(&doc.supersedes)
        ));
        out.push_str(&format!(
            "      \"content_hash\": {},\n",
            json_string(&doc.content_hash)
        ));
        out.push_str(&format!(
            "      \"file_size_bytes\": {},\n",
            doc.file_size_bytes
        ));
        out.push_str(&format!(
            "      \"incoming_links\": {},\n",
            doc.incoming_links
        ));
        out.push_str(&format!(
            "      \"outgoing_links\": {},\n",
            doc.outgoing_links
        ));
        out.push_str(&format!(
            "      \"is_entrypoint\": {},\n",
            json_bool(doc.is_entrypoint)
        ));
        out.push_str(&format!(
            "      \"is_anchor_candidate\": {},\n",
            json_bool(doc.is_anchor_candidate)
        ));
        out.push_str(&format!(
            "      \"is_generated\": {},\n",
            json_bool(doc.is_generated)
        ));
        out.push_str(&format!(
            "      \"is_reference\": {},\n",
            json_bool(doc.is_reference)
        ));
        out.push_str(&format!("      \"mtime_unix\": {}\n", doc.mtime_unix));
        out.push_str("    }");
        if idx + 1 != artifact.docs.len() {
            out.push(',');
        }
        out.push('\n');
    }
    out.push_str("  ],\n");
    out.push_str("  \"relations\": [\n");
    for (idx, relation) in artifact.relations.iter().enumerate() {
        out.push_str("    {\n");
        out.push_str(&format!(
            "      \"src_path\": {},\n",
            json_string(&relation.src_path)
        ));
        out.push_str(&format!(
            "      \"dst_path\": {},\n",
            json_string(&relation.dst_path)
        ));
        out.push_str(&format!(
            "      \"relation_type\": {},\n",
            json_string(&relation.relation_type)
        ));
        out.push_str(&format!(
            "      \"reason\": {},\n",
            json_string(&relation.reason)
        ));
        out.push_str(&format!(
            "      \"explicit\": {},\n",
            json_bool(relation.explicit)
        ));
        out.push_str(&format!(
            "      \"confidence\": {:.2},\n",
            relation.confidence
        ));
        out.push_str(&format!(
            "      \"evidence_json\": {}\n",
            json_value_or_string(&relation.evidence_json)
        ));
        out.push_str("    }");
        if idx + 1 != artifact.relations.len() {
            out.push(',');
        }
        out.push('\n');
    }
    out.push_str("  ],\n");
    out.push_str("  \"checks\": [\n");
    for (idx, check) in artifact.checks.iter().enumerate() {
        out.push_str("    {\n");
        out.push_str(&format!(
            "      \"rule_id\": {},\n",
            json_string(&check.rule_id)
        ));
        out.push_str(&format!(
            "      \"severity\": {},\n",
            json_string(&check.severity)
        ));
        out.push_str(&format!(
            "      \"subject_path\": {},\n",
            json_string(&check.subject_path)
        ));
        out.push_str(&format!(
            "      \"message\": {}\n",
            json_string(&check.message)
        ));
        out.push_str("    }");
        if idx + 1 != artifact.checks.len() {
            out.push(',');
        }
        out.push('\n');
    }
    out.push_str("  ]\n");
    out.push_str("}\n");
    out
}

fn list_history_snapshots(target_dir: &Path) -> Result<Vec<HistorySnapshot>> {
    let dir = history_snapshots_dir(target_dir);
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut snapshots = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let text = fs::read_to_string(path)?;
        snapshots.push(parse_history_snapshot_json(&text)?);
    }
    snapshots.sort_by(|a, b| {
        a.created_at_unix
            .cmp(&b.created_at_unix)
            .then_with(|| a.id.cmp(&b.id))
    });
    Ok(snapshots)
}

fn parse_history_snapshot_json(text: &str) -> Result<HistorySnapshot> {
    let mut cursor = JsonCursor::default();
    let root = parse_json_object(text, &mut cursor)?;
    let snapshot = HistorySnapshot {
        id: json_object_required_string(&root, "id")?,
        created_at_unix: json_object_required_u64(&root, "created_at_unix")?,
        message: json_object_required_string(&root, "message")?,
        docs_root: json_object_required_string(&root, "docs_root")?,
        schema_hash: json_object_required_string(&root, "schema_hash")?,
        host_git_head: json_object_optional_string(&root, "host_git_head")?,
        host_git_branch: json_object_optional_string(&root, "host_git_branch")?,
        host_git_dirty: json_object_required_bool(&root, "host_git_dirty")?,
        files: json_object_required_array(&root, "files")?
            .iter()
            .map(|value| history_file_record_from_json(value))
            .collect::<Result<Vec<_>>>()?,
    };
    if snapshot.id.is_empty() {
        return Err("invalid history snapshot: missing id".into());
    }
    Ok(snapshot)
}

fn parse_json_string_value(value: &str) -> String {
    let mut cursor = JsonCursor::default();
    parse_json_string_token(value.trim(), &mut cursor).unwrap_or_default()
}

fn history_file_record_from_json(value: &str) -> Result<HistoryFileRecord> {
    let object = parse_json_object(value, &mut JsonCursor::default())?;
    Ok(HistoryFileRecord {
        path: json_object_required_string(&object, "path")?,
        hash: json_object_required_string(&object, "hash")?,
        size: json_object_required_u64(&object, "size")?,
        mtime_unix: json_object_required_u64(&object, "mtime_unix")?,
    })
}

fn parse_json_object(text: &str, cursor: &mut JsonCursor) -> Result<Vec<(String, String)>> {
    skip_json_ws(text, cursor);
    expect_json_char(text, cursor, '{')?;
    let mut entries = Vec::new();
    loop {
        skip_json_ws(text, cursor);
        if try_consume_json_char(text, cursor, '}') {
            break;
        }
        let key = parse_json_string_token(text, cursor)?;
        skip_json_ws(text, cursor);
        expect_json_char(text, cursor, ':')?;
        skip_json_ws(text, cursor);
        let value = parse_json_value_token(text, cursor)?;
        entries.push((key, value));
        skip_json_ws(text, cursor);
        if try_consume_json_char(text, cursor, ',') {
            continue;
        }
        expect_json_char(text, cursor, '}')?;
        break;
    }
    Ok(entries)
}

fn parse_json_array_tokens(text: &str, cursor: &mut JsonCursor) -> Result<Vec<String>> {
    skip_json_ws(text, cursor);
    expect_json_char(text, cursor, '[')?;
    let mut items = Vec::new();
    loop {
        skip_json_ws(text, cursor);
        if try_consume_json_char(text, cursor, ']') {
            break;
        }
        items.push(parse_json_value_token(text, cursor)?);
        skip_json_ws(text, cursor);
        if try_consume_json_char(text, cursor, ',') {
            continue;
        }
        expect_json_char(text, cursor, ']')?;
        break;
    }
    Ok(items)
}

fn parse_json_value_token(text: &str, cursor: &mut JsonCursor) -> Result<String> {
    skip_json_ws(text, cursor);
    let Some(ch) = peek_json_char(text, cursor) else {
        return Err("invalid json: unexpected end of input".into());
    };
    match ch {
        '"' => parse_json_string_literal(text, cursor),
        '{' => parse_json_balanced_block(text, cursor, '{', '}'),
        '[' => parse_json_balanced_block(text, cursor, '[', ']'),
        _ => parse_json_scalar_literal(text, cursor),
    }
}

fn parse_json_string_literal(text: &str, cursor: &mut JsonCursor) -> Result<String> {
    let start = cursor.pos;
    let _ = parse_json_string_token(text, cursor)?;
    Ok(text[start..cursor.pos].to_string())
}

fn parse_json_string_token(text: &str, cursor: &mut JsonCursor) -> Result<String> {
    expect_json_char(text, cursor, '"')?;
    let mut out = String::new();
    while let Some(ch) = next_json_char(text, cursor) {
        match ch {
            '"' => return Ok(out),
            '\\' => match next_json_char(text, cursor) {
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('t') => out.push('\t'),
                Some('b') => out.push('\u{0008}'),
                Some('f') => out.push('\u{000c}'),
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some('/') => out.push('/'),
                Some('u') => {
                    let code = read_json_hex_escape(text, cursor)?;
                    push_json_unicode_escape(text, cursor, code, &mut out)?;
                }
                Some(other) => out.push(other),
                None => return Err("invalid json: unterminated escape".into()),
            },
            other => out.push(other),
        }
    }
    Err("invalid json: unterminated string".into())
}

fn push_json_unicode_escape(
    text: &str,
    cursor: &mut JsonCursor,
    code: u32,
    out: &mut String,
) -> Result<()> {
    if (0xD800..=0xDBFF).contains(&code) {
        let saved = cursor.pos;
        if next_json_char(text, cursor) == Some('\\') && next_json_char(text, cursor) == Some('u') {
            let low = read_json_hex_escape(text, cursor)?;
            if (0xDC00..=0xDFFF).contains(&low) {
                let combined = 0x10000 + (((code - 0xD800) << 10) | (low - 0xDC00));
                if let Some(decoded) = char::from_u32(combined) {
                    out.push(decoded);
                    return Ok(());
                }
            }
        }
        cursor.pos = saved;
        out.push('\u{FFFD}');
        return Ok(());
    }
    if (0xDC00..=0xDFFF).contains(&code) {
        out.push('\u{FFFD}');
        return Ok(());
    }
    if let Some(decoded) = char::from_u32(code) {
        out.push(decoded);
    } else {
        out.push('\u{FFFD}');
    }
    Ok(())
}

fn parse_json_balanced_block(
    text: &str,
    cursor: &mut JsonCursor,
    open: char,
    close: char,
) -> Result<String> {
    let start = cursor.pos;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    while let Some(ch) = next_json_char(text, cursor) {
        if in_string {
            if escaped {
                escaped = false;
                continue;
            }
            match ch {
                '\\' => escaped = true,
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            c if c == open => depth += 1,
            c if c == close => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Ok(text[start..cursor.pos].to_string());
                }
            }
            _ => {}
        }
    }
    Err("invalid json: unterminated object or array".into())
}

fn parse_json_scalar_literal(text: &str, cursor: &mut JsonCursor) -> Result<String> {
    let start = cursor.pos;
    while let Some(ch) = peek_json_char(text, cursor) {
        if ch == ',' || ch == '}' || ch == ']' || ch.is_whitespace() {
            break;
        }
        let _ = next_json_char(text, cursor);
    }
    let token = text[start..cursor.pos].trim();
    if token.is_empty() {
        Err("invalid json: empty scalar".into())
    } else {
        Ok(token.to_string())
    }
}

fn json_object_required_string(entries: &[(String, String)], key: &str) -> Result<String> {
    let value = json_object_value(entries, key)
        .ok_or_else(|| format!("invalid json object: missing key `{}`", key))?;
    Ok(parse_json_string_value(value))
}

fn json_object_optional_string(entries: &[(String, String)], key: &str) -> Result<Option<String>> {
    match json_object_value(entries, key) {
        Some(value) => Ok(parse_json_opt_string_value(value)),
        None => Ok(None),
    }
}

fn json_object_required_u64(entries: &[(String, String)], key: &str) -> Result<u64> {
    let value = json_object_value(entries, key)
        .ok_or_else(|| format!("invalid json object: missing key `{}`", key))?;
    value
        .trim()
        .parse::<u64>()
        .map_err(|_| format!("invalid json object: expected u64 for `{}`", key).into())
}

fn json_object_required_bool(entries: &[(String, String)], key: &str) -> Result<bool> {
    let value = json_object_value(entries, key)
        .ok_or_else(|| format!("invalid json object: missing key `{}`", key))?;
    match value.trim() {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(format!("invalid json object: expected bool for `{}`", key).into()),
    }
}

fn json_object_required_array(entries: &[(String, String)], key: &str) -> Result<Vec<String>> {
    let value = json_object_value(entries, key)
        .ok_or_else(|| format!("invalid json object: missing key `{}`", key))?;
    parse_json_array_tokens(value, &mut JsonCursor::default())
}

fn json_object_value<'a>(entries: &'a [(String, String)], key: &str) -> Option<&'a str> {
    entries
        .iter()
        .find(|(entry_key, _)| entry_key == key)
        .map(|(_, value)| value.as_str())
}

fn skip_json_ws(text: &str, cursor: &mut JsonCursor) {
    while let Some(ch) = peek_json_char(text, cursor) {
        if ch.is_whitespace() {
            let _ = next_json_char(text, cursor);
        } else {
            break;
        }
    }
}

fn expect_json_char(text: &str, cursor: &mut JsonCursor, expected: char) -> Result<()> {
    match next_json_char(text, cursor) {
        Some(ch) if ch == expected => Ok(()),
        Some(ch) => Err(format!("invalid json: expected `{}`, got `{}`", expected, ch).into()),
        None => Err(format!("invalid json: expected `{}`, got end of input", expected).into()),
    }
}

fn try_consume_json_char(text: &str, cursor: &mut JsonCursor, expected: char) -> bool {
    if peek_json_char(text, cursor) == Some(expected) {
        let _ = next_json_char(text, cursor);
        true
    } else {
        false
    }
}

fn peek_json_char(text: &str, cursor: &JsonCursor) -> Option<char> {
    text[cursor.pos..].chars().next()
}

fn next_json_char(text: &str, cursor: &mut JsonCursor) -> Option<char> {
    let ch = peek_json_char(text, cursor)?;
    cursor.pos += ch.len_utf8();
    Some(ch)
}

fn read_json_hex_escape(text: &str, cursor: &mut JsonCursor) -> Result<u32> {
    let mut value = 0u32;
    for _ in 0..4 {
        let Some(ch) = next_json_char(text, cursor) else {
            return Err("invalid json: truncated unicode escape".into());
        };
        let digit = ch
            .to_digit(16)
            .ok_or_else(|| format!("invalid json: bad unicode escape digit `{}`", ch))?;
        value = (value << 4) | digit;
    }
    Ok(value)
}

fn parse_json_opt_string_value(value: &str) -> Option<String> {
    if value.trim() == "null" {
        None
    } else {
        Some(parse_json_string_value(value))
    }
}

fn latest_history_snapshot_id(target_dir: &Path) -> Option<String> {
    fs::read_to_string(history_refs_dir(target_dir).join("latest"))
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn resolve_snapshot_arg(target_dir: &Path, value: &str) -> Result<HistorySnapshot> {
    let id = if value == "latest" {
        latest_history_snapshot_id(target_dir).ok_or("no latest history snapshot found")?
    } else {
        value.to_string()
    };
    let path = history_snapshots_dir(target_dir).join(format!("{}.json", id));
    let text = fs::read_to_string(&path)?;
    parse_history_snapshot_json(&text)
}

fn latest_history_snapshot(target_dir: &Path) -> Result<Option<HistorySnapshot>> {
    match latest_history_snapshot_id(target_dir) {
        Some(id) => Ok(Some(resolve_snapshot_arg(target_dir, &id)?)),
        None => Ok(None),
    }
}

fn compute_history_status(target_dir: &Path, schema: &Schema) -> Result<HistoryStatus> {
    let current = collect_history_file_records(target_dir, schema)?;
    let current_map = current
        .iter()
        .map(|file| (file.path.clone(), file.hash.clone()))
        .collect::<BTreeMap<_, _>>();
    let latest = latest_history_snapshot(target_dir)?;
    let Some(snapshot) = latest else {
        return Ok(HistoryStatus {
            latest_snapshot: None,
            tracked_files_count: current.len(),
            added: current.iter().map(|file| file.path.clone()).collect(),
            changed: Vec::new(),
            removed: Vec::new(),
            schema_changed: !current.is_empty(),
        });
    };
    let snapshot_map = snapshot
        .files
        .iter()
        .map(|file| (file.path.clone(), file.hash.clone()))
        .collect::<BTreeMap<_, _>>();
    let mut added = Vec::new();
    let mut changed = Vec::new();
    let mut removed = Vec::new();
    for (path, hash) in &current_map {
        match snapshot_map.get(path) {
            None => added.push(path.clone()),
            Some(previous) if previous != hash => changed.push(path.clone()),
            Some(_) => {}
        }
    }
    for path in snapshot_map.keys() {
        if !current_map.contains_key(path) {
            removed.push(path.clone());
        }
    }
    added.sort();
    changed.sort();
    removed.sort();
    let schema_changed = current_map
        .get(".harnesskit/schema.yaml")
        .map(|hash| hash != &snapshot.schema_hash)
        .unwrap_or(!snapshot.schema_hash.is_empty());
    Ok(HistoryStatus {
        latest_snapshot: Some(snapshot.id),
        tracked_files_count: current.len(),
        added,
        changed,
        removed,
        schema_changed,
    })
}

fn is_history_dirty(status: &HistoryStatus) -> bool {
    has_history_doc_changes(status) || status.schema_changed
}

fn has_history_doc_changes(status: &HistoryStatus) -> bool {
    status
        .added
        .iter()
        .any(|path| path != ".harnesskit/schema.yaml")
        || status
            .changed
            .iter()
            .any(|path| path != ".harnesskit/schema.yaml")
        || status
            .removed
            .iter()
            .any(|path| path != ".harnesskit/schema.yaml")
}

fn print_history_status(status: &HistoryStatus) {
    println!(
        "History status: latest={} files={} dirty={}",
        status.latest_snapshot.as_deref().unwrap_or("-"),
        status.tracked_files_count,
        yes_no_bool(is_history_dirty(status))
    );
    print_named_path_group("Added", &status.added);
    print_named_path_group("Changed", &status.changed);
    print_named_path_group("Removed", &status.removed);
    println!("Schema changed: {}", yes_no_bool(status.schema_changed));
}

fn yes_no_bool(value: bool) -> &'static str {
    if value {
        "yes"
    } else {
        "no"
    }
}

#[derive(Default)]
struct HistoryDiff {
    added: Vec<String>,
    changed: Vec<String>,
    removed: Vec<String>,
}

fn diff_snapshots(a: &HistorySnapshot, b: &HistorySnapshot) -> HistoryDiff {
    let a_map = a
        .files
        .iter()
        .map(|file| (file.path.clone(), file.hash.clone()))
        .collect::<BTreeMap<_, _>>();
    let b_map = b
        .files
        .iter()
        .map(|file| (file.path.clone(), file.hash.clone()))
        .collect::<BTreeMap<_, _>>();
    diff_hash_maps(&a_map, &b_map)
}

fn diff_snapshot_to_worktree(
    target_dir: &Path,
    schema: &Schema,
    snapshot: &HistorySnapshot,
) -> Result<HistoryDiff> {
    let snapshot_map = snapshot
        .files
        .iter()
        .map(|file| (file.path.clone(), file.hash.clone()))
        .collect::<BTreeMap<_, _>>();
    let current_map = collect_history_file_records(target_dir, schema)?
        .iter()
        .map(|file| (file.path.clone(), file.hash.clone()))
        .collect::<BTreeMap<_, _>>();
    Ok(diff_hash_maps(&snapshot_map, &current_map))
}

fn diff_hash_maps(a: &BTreeMap<String, String>, b: &BTreeMap<String, String>) -> HistoryDiff {
    let mut diff = HistoryDiff::default();
    for (path, hash) in b {
        match a.get(path) {
            None => diff.added.push(path.clone()),
            Some(previous) if previous != hash => diff.changed.push(path.clone()),
            Some(_) => {}
        }
    }
    for path in a.keys() {
        if !b.contains_key(path) {
            diff.removed.push(path.clone());
        }
    }
    diff
}

fn print_history_diff(diff: &HistoryDiff) {
    print_named_path_group("Added", &diff.added);
    print_named_path_group("Changed", &diff.changed);
    print_named_path_group("Removed", &diff.removed);
}

fn print_named_path_group(label: &str, paths: &[String]) {
    println!("{}:", label);
    if paths.is_empty() {
        println!("- none");
    } else {
        for path in paths {
            println!("- {}", path);
        }
    }
}

#[derive(Default)]
struct RestorePlan {
    restore_files: Vec<HistoryFileRecord>,
    remove_paths: Vec<String>,
}

fn restore_plan(
    target_dir: &Path,
    schema: &Schema,
    snapshot: &HistorySnapshot,
) -> Result<RestorePlan> {
    let current = collect_history_file_records(target_dir, schema)?;
    let snapshot_paths = snapshot
        .files
        .iter()
        .map(|file| file.path.clone())
        .collect::<BTreeSet<_>>();
    let current_paths = current
        .iter()
        .map(|file| file.path.clone())
        .collect::<BTreeSet<_>>();
    let remove_paths = current_paths
        .difference(&snapshot_paths)
        .cloned()
        .collect::<Vec<_>>();
    Ok(RestorePlan {
        restore_files: snapshot.files.clone(),
        remove_paths,
    })
}

fn print_restore_plan(snapshot_id: &str, plan: &RestorePlan, apply: bool) {
    println!(
        "Restore {} snapshot {}:",
        if apply { "applying" } else { "preview for" },
        snapshot_id
    );
    println!("Files to restore:");
    for file in &plan.restore_files {
        println!("- {}", file.path);
    }
    print_named_path_group("Files to remove", &plan.remove_paths);
    if !apply {
        println!("Preview only. Re-run with --apply to write files.");
    }
}

fn apply_restore_plan(target_dir: &Path, plan: &RestorePlan) -> Result<()> {
    for file in &plan.restore_files {
        let object_path = history_object_path(target_dir, &file.hash);
        if !object_path.is_file() {
            return Err(format!(
                "missing history object for {}: {}",
                file.path,
                object_path.display()
            )
            .into());
        }
    }

    for file in &plan.restore_files {
        let object_path = history_object_path(target_dir, &file.hash);
        let target_path = target_dir.join(&file.path);
        if let Some(parent) = target_path.parent() {
            fs::create_dir_all(parent)?;
        }
        atomic_copy_file(&object_path, &target_path)?;
    }
    for path in &plan.remove_paths {
        let target_path = target_dir.join(path);
        if target_path.exists() {
            fs::remove_file(target_path)?;
        }
    }
    Ok(())
}

fn atomic_copy_file(src: &Path, dst: &Path) -> Result<()> {
    let content = fs::read(src)
        .map_err(|err| format!("failed to read restore object {}: {}", src.display(), err))?;
    let file_name = dst
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("restore-target");
    let tmp_path = dst.with_file_name(format!(
        ".{}.harnesskit-restore-tmp-{}",
        file_name,
        std::process::id()
    ));
    if let Err(err) = fs::write(&tmp_path, content) {
        let _ = fs::remove_file(&tmp_path);
        return Err(format!(
            "failed to write restore temp {}: {}",
            tmp_path.display(),
            err
        )
        .into());
    }
    if let Err(err) = fs::rename(&tmp_path, dst) {
        let _ = fs::remove_file(&tmp_path);
        return Err(format!(
            "failed to replace {} from restore temp: {}",
            dst.display(),
            err
        )
        .into());
    }
    Ok(())
}

fn render_index_summary(artifact: &IndexArtifact) -> String {
    let mut out = String::new();
    out.push_str("# HarnessKit Doc Index Summary\n\n");
    out.push_str(&format!("- Managed root: `{}`\n", artifact.managed_root));
    out.push_str(&format!("- Documents indexed: `{}`\n", artifact.docs.len()));
    out.push_str(&format!(
        "- History latest snapshot: `{}`\n",
        artifact.history_latest_snapshot.as_deref().unwrap_or("-")
    ));
    out.push_str(&format!("- History dirty: `{}`\n", artifact.history_dirty));
    out.push_str(&format!(
        "- Host git dirty: `{}`\n",
        artifact.host_git_dirty
    ));
    out.push_str(&format!(
        "- Relations built: `{}`\n\n",
        artifact.relations.len()
    ));

    out.push_str("## Top Anchor Docs\n\n");
    let mut anchor_docs = artifact
        .docs
        .iter()
        .filter(|doc| doc.is_anchor_candidate || doc.is_entrypoint)
        .collect::<Vec<_>>();
    anchor_docs.sort_by(|a, b| {
        b.incoming_links
            .cmp(&a.incoming_links)
            .then_with(|| b.outgoing_links.cmp(&a.outgoing_links))
            .then_with(|| a.path.cmp(&b.path))
    });
    for doc in anchor_docs.into_iter().take(12) {
        out.push_str(&format!(
            "- `{}` - {} (incoming {}, outgoing {})\n",
            doc.path, doc.title, doc.incoming_links, doc.outgoing_links
        ));
    }

    out.push_str("\n## Relation Types\n\n");
    let mut relation_counts = BTreeMap::<String, usize>::new();
    for relation in &artifact.relations {
        *relation_counts
            .entry(relation.relation_type.clone())
            .or_insert(0) += 1;
    }
    for (relation_type, count) in relation_counts {
        out.push_str(&format!("- `{}`: `{}`\n", relation_type, count));
    }

    out.push_str("\n## Collections\n\n");
    let mut counts = BTreeMap::<String, usize>::new();
    for doc in &artifact.docs {
        if let Some(collection) = &doc.collection {
            *counts.entry(collection.clone()).or_insert(0) += 1;
        }
    }
    for (collection, count) in counts {
        out.push_str(&format!("- `{}`: `{}` docs\n", collection, count));
    }

    if !artifact.checks.is_empty() {
        out.push_str("\n## Checks\n\n");
        for check in &artifact.checks {
            out.push_str(&format!(
                "- `[{}]` `{}` - {}\n",
                check.severity, check.subject_path, check.message
            ));
        }
    }

    out
}

fn sql_string_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn sql_opt_literal(value: Option<&str>) -> String {
    value
        .map(sql_string_literal)
        .unwrap_or_else(|| "NULL".to_string())
}

fn sql_bool(value: bool) -> i32 {
    if value {
        1
    } else {
        0
    }
}

fn print_rank_rows(result: &QueryResult) {
    println!("Top ranked docs:");
    for row in &result.rows {
        let path = cell(row, 0);
        let title = cell(row, 1);
        let score = cell(row, 2);
        let role = cell(row, 3);
        let summary = cell(row, 4);
        println!("- {} [{}] score={} :: {}", path, role, score, title);
        if !summary.is_empty() {
            println!("  summary: {}", summary);
        }
    }
}

fn print_list_docs_rows(result: &QueryResult) {
    println!("Managed docs:");
    for row in &result.rows {
        println!(
            "- {} [{}] status={} authority={} collection={} incoming={} outgoing={} :: {}",
            cell(row, 0),
            cell(row, 2),
            blank_as_dash(cell(row, 3)),
            blank_as_dash(cell(row, 4)),
            blank_as_dash(cell(row, 7)),
            cell(row, 5),
            cell(row, 6),
            cell(row, 1)
        );
    }
}

fn print_inspect_output(
    doc: &QueryResult,
    outgoing: &QueryResult,
    incoming: &QueryResult,
    checks: &QueryResult,
) {
    let row = &doc.rows[0];
    println!("Path: {}", cell(row, 0));
    println!("Title: {}", cell(row, 1));
    println!("Role: {}", cell(row, 2));
    println!("Status: {}", blank_as_dash(cell(row, 3)));
    println!("Authority: {}", blank_as_dash(cell(row, 4)));
    println!("Collection: {}", blank_as_dash(cell(row, 5)));
    println!("Incoming links: {}", cell(row, 7));
    println!("Outgoing links: {}", cell(row, 8));
    println!("Entrypoint: {}", yes_no(cell(row, 9)));
    println!("Anchor candidate: {}", yes_no(cell(row, 10)));
    println!("Generated: {}", yes_no(cell(row, 11)));
    println!("Reference: {}", yes_no(cell(row, 12)));
    println!("Size bytes: {}", cell(row, 13));
    println!("Modified: {}", cell(row, 14));
    if !cell(row, 6).is_empty() {
        println!("Summary: {}", cell(row, 6));
    }

    println!("\nOutgoing relations:");
    if outgoing.rows.is_empty() {
        println!("- none");
    } else {
        for relation in &outgoing.rows {
            println!(
                "- {} -> {} ({}, confidence={:.2})",
                cell(relation, 0),
                cell(relation, 1),
                cell(relation, 2),
                cell(relation, 3).parse::<f32>().unwrap_or(0.0)
            );
        }
    }

    println!("\nIncoming relations:");
    if incoming.rows.is_empty() {
        println!("- none");
    } else {
        for relation in &incoming.rows {
            println!(
                "- {} <- {} ({}, confidence={:.2})",
                cell(relation, 0),
                cell(relation, 1),
                cell(relation, 2),
                cell(relation, 3).parse::<f32>().unwrap_or(0.0)
            );
        }
    }

    println!("\nChecks:");
    if checks.rows.is_empty() {
        println!("- none");
    } else {
        for check in &checks.rows {
            println!(
                "- [{}] {} :: {}",
                cell(check, 0),
                cell(check, 1),
                cell(check, 2)
            );
            if !cell(check, 3).is_empty() && cell(check, 3) != "{}" {
                println!("  evidence: {}", cell(check, 3));
            }
        }
    }
}

fn print_refs_output(doc_path: &str, result: &QueryResult) {
    println!("Refs for {}:", doc_path);
    if result.rows.is_empty() {
        println!("- none");
        return;
    }
    for row in &result.rows {
        println!(
            "- {} via {} ({}, confidence={:.2})",
            cell(row, 0),
            cell(row, 1),
            cell(row, 2),
            cell(row, 3).parse::<f32>().unwrap_or(0.0)
        );
    }
}

fn print_query_candidates(query_terms: &str, candidates: &[QueryCandidate]) {
    println!("Query: {}", query_terms);
    if candidates.is_empty() {
        println!("No candidate docs found.");
        return;
    }
    for candidate in candidates {
        println!(
            "- {} [{}] status={} score={} :: {}",
            candidate.path,
            candidate.role,
            blank_as_dash(&candidate.status),
            candidate.score,
            candidate.title
        );
        println!("  why: {}", candidate.why);
        if !candidate.summary.is_empty() {
            println!("  summary: {}", candidate.summary);
        }
        if !candidate.snippet.is_empty() {
            println!("  snippet: {}", candidate.snippet);
            if !candidate.snippet_heading.is_empty() {
                println!("  heading: {}", candidate.snippet_heading);
            }
            if !candidate.snippet_location.is_empty() {
                println!("  location: {}", candidate.snippet_location);
            }
        }
        if !candidate.neighbors.is_empty() {
            println!("  neighbors: {}", candidate.neighbors.join(" | "));
        }
    }
}

fn print_context_packet(label: &str, input: &str, packet: &ContextPacket) {
    println!("{}: {}", label, input);
    println!(
        "Anchor: {} [{}] :: {}",
        packet.anchor.path, packet.anchor.role, packet.anchor.title
    );
    if !packet.anchor.summary.is_empty() {
        println!("Summary: {}", packet.anchor.summary);
    }
    if !packet.anchor.snippet.is_empty() {
        println!("Snippet: {}", packet.anchor.snippet);
        if !packet.anchor.snippet_location.is_empty() {
            println!("Location: {}", packet.anchor.snippet_location);
        }
    }

    println!("\nIncoming refs:");
    print_relation_list(&packet.incoming, true);
    println!("\nOutgoing refs:");
    print_relation_list(&packet.outgoing, false);
    println!("\nCollection siblings:");
    print_path_list(&packet.collection_siblings);
    println!("\nSame-scope docs:");
    print_path_list(&packet.same_scope_docs);
    println!("\nRecommended reading order:");
    print_path_list(&packet.recommended_reading_order);
}

fn print_relation_list(relations: &[DocRelation], incoming: bool) {
    if relations.is_empty() {
        println!("- none");
        return;
    }
    for relation in relations {
        let path = if incoming {
            &relation.src_path
        } else {
            &relation.dst_path
        };
        println!(
            "- {} via {} ({}, confidence={:.2})",
            path, relation.relation_type, relation.reason, relation.confidence
        );
    }
}

fn print_path_list(paths: &[String]) {
    if paths.is_empty() {
        println!("- none");
        return;
    }
    for path in paths {
        println!("- {}", path);
    }
}

fn filter_and_suppress_checks(
    result: &QueryResult,
    schema: &Schema,
    options: &CommandOptions,
) -> Vec<CheckView> {
    result
        .rows
        .iter()
        .filter_map(|row| {
            let rule_id = cell(row, 1).to_string();
            if let Some(rules) = &options.rules {
                if !rules.contains(&rule_group(&rule_id)) && !rules.contains(&rule_id) {
                    return None;
                }
            }
            let subject_path = cell(row, 2).to_string();
            let suppression = schema.suppressions.iter().find(|suppression| {
                suppression.rule_id == rule_id && suppression.path == subject_path
            });
            Some(CheckView {
                severity: cell(row, 0).to_string(),
                rule_id,
                subject_path,
                message: cell(row, 3).to_string(),
                evidence_json: cell(row, 4).to_string(),
                suppressed: suppression.is_some(),
                suppression_reason: suppression.map(|item| item.reason.clone()),
            })
        })
        .collect()
}

fn rule_group(rule_id: &str) -> String {
    match rule_id {
        "missing-entrypoint-doc"
        | "missing-required-doc"
        | "missing-collection-index"
        | "entrypoint-manifest-link"
        | "manifest-missing-doc"
        | "unresolved-managed-link"
        | "wrong-docs-navigation-target"
        | "orphan-managed-doc" => "manifest".to_string(),
        "stale-explicit-anchor" | "missing-scope-path" => "stale".to_string(),
        "duplicate-content" | "overlap-heading-fingerprint" => "duplicate".to_string(),
        "canonical-links-generated" | "canonical-links-reference" => "contamination".to_string(),
        "frontmatter-field-not-allowed" => "frontmatter".to_string(),
        "host-git-exclude-missing"
        | "managed-file-not-excluded"
        | "history-missing"
        | "history-dirty-since-snapshot"
        | "history-schema-not-snapshotted" => "history".to_string(),
        other => other.to_string(),
    }
}

fn render_check_json(
    target_dir: &Path,
    docs_count: usize,
    relation_count: usize,
    checks: &[CheckView],
) -> String {
    let mut out = String::new();
    out.push_str("{\n");
    out.push_str(&format!(
        "  \"target_dir\": {},\n",
        json_string(&absolute_display_path(target_dir))
    ));
    out.push_str(&format!("  \"docs_count\": {},\n", docs_count));
    out.push_str(&format!("  \"relation_count\": {},\n", relation_count));
    out.push_str("  \"checks\": [\n");
    for (idx, check) in checks.iter().enumerate() {
        out.push_str("    {\n");
        out.push_str(&format!(
            "      \"severity\": {},\n",
            json_string(&check.severity)
        ));
        out.push_str(&format!(
            "      \"rule_id\": {},\n",
            json_string(&check.rule_id)
        ));
        out.push_str(&format!(
            "      \"rule_group\": {},\n",
            json_string(&rule_group(&check.rule_id))
        ));
        out.push_str(&format!(
            "      \"subject_path\": {},\n",
            json_string(&check.subject_path)
        ));
        out.push_str(&format!(
            "      \"message\": {},\n",
            json_string(&check.message)
        ));
        out.push_str(&format!(
            "      \"evidence_json\": {},\n",
            json_value_or_string(&check.evidence_json)
        ));
        out.push_str(&format!(
            "      \"suppressed\": {},\n",
            json_bool(check.suppressed)
        ));
        out.push_str(&format!(
            "      \"suppression_reason\": {}\n",
            json_opt_string(check.suppression_reason.as_deref())
        ));
        out.push_str("    }");
        if idx + 1 != checks.len() {
            out.push(',');
        }
        out.push('\n');
    }
    out.push_str("  ]\n}\n");
    out
}

fn render_query_json(query_terms: &str, candidates: &[QueryCandidate]) -> String {
    let mut out = String::new();
    out.push_str("{\n");
    out.push_str(&format!("  \"query\": {},\n", json_string(query_terms)));
    out.push_str("  \"candidates\": [\n");
    for (idx, candidate) in candidates.iter().enumerate() {
        out.push_str(&render_query_candidate_json(candidate, 4));
        if idx + 1 != candidates.len() {
            out.push(',');
        }
        out.push('\n');
    }
    out.push_str("  ]\n}\n");
    out
}

fn render_context_packet_json(kind: &str, input: &str, packet: &ContextPacket) -> String {
    let mut out = String::new();
    out.push_str("{\n");
    out.push_str(&format!("  \"kind\": {},\n", json_string(kind)));
    out.push_str(&format!("  \"input\": {},\n", json_string(input)));
    out.push_str("  \"anchor\": ");
    out.push_str(&render_query_candidate_json(&packet.anchor, 2));
    out.push_str(",\n");
    out.push_str(&format!(
        "  \"incoming\": {},\n",
        render_relations_json(&packet.incoming)
    ));
    out.push_str(&format!(
        "  \"outgoing\": {},\n",
        render_relations_json(&packet.outgoing)
    ));
    out.push_str(&format!(
        "  \"collection_siblings\": {},\n",
        json_string_array(&packet.collection_siblings)
    ));
    out.push_str(&format!(
        "  \"same_scope_docs\": {},\n",
        json_string_array(&packet.same_scope_docs)
    ));
    out.push_str(&format!(
        "  \"recommended_reading_order\": {}\n",
        json_string_array(&packet.recommended_reading_order)
    ));
    out.push_str("}\n");
    out
}

fn render_query_candidate_json(candidate: &QueryCandidate, indent: usize) -> String {
    let pad = " ".repeat(indent);
    let inner = " ".repeat(indent + 2);
    format!(
        "{pad}{{\n{inner}\"path\": {},\n{inner}\"title\": {},\n{inner}\"role\": {},\n{inner}\"status\": {},\n{inner}\"score\": {},\n{inner}\"why\": {},\n{inner}\"summary\": {},\n{inner}\"neighbors\": {},\n{inner}\"snippet_heading\": {},\n{inner}\"snippet_location\": {},\n{inner}\"snippet\": {}\n{pad}}}",
        json_string(&candidate.path),
        json_string(&candidate.title),
        json_string(&candidate.role),
        json_string(&candidate.status),
        json_string(&candidate.score),
        json_string(&candidate.why),
        json_string(&candidate.summary),
        json_string_array(&candidate.neighbors),
        json_string(&candidate.snippet_heading),
        json_string(&candidate.snippet_location),
        json_string(&candidate.snippet),
        pad = pad,
        inner = inner
    )
}

fn render_relations_json(relations: &[DocRelation]) -> String {
    let mut out = String::from("[");
    for (idx, relation) in relations.iter().enumerate() {
        if idx > 0 {
            out.push_str(", ");
        }
        out.push_str(&format!(
            "{{\"src_path\":{},\"dst_path\":{},\"relation_type\":{},\"reason\":{},\"confidence\":{:.2},\"explicit\":{}}}",
            json_string(&relation.src_path),
            json_string(&relation.dst_path),
            json_string(&relation.relation_type),
            json_string(&relation.reason),
            relation.confidence,
            json_bool(relation.explicit)
        ));
    }
    out.push(']');
    out
}

fn cell(row: &[Option<String>], index: usize) -> &str {
    row.get(index)
        .and_then(|value| value.as_deref())
        .unwrap_or("")
}

fn blank_as_dash(value: &str) -> &str {
    if value.is_empty() {
        "-"
    } else {
        value
    }
}

fn yes_no(value: &str) -> &str {
    if value == "1" {
        "yes"
    } else {
        "no"
    }
}

fn json_string_array(values: &[String]) -> String {
    format!(
        "[{}]",
        values
            .iter()
            .map(|value| json_string(value))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn json_value_or_string(value: &str) -> String {
    let trimmed = value.trim();
    if parse_json_value_token(trimmed, &mut JsonCursor::default()).is_ok() {
        trimmed.to_string()
    } else {
        json_string(value)
    }
}

fn json_opt_string(value: Option<&str>) -> String {
    value.map(json_string).unwrap_or_else(|| "null".to_string())
}

fn json_bool(value: bool) -> &'static str {
    if value {
        "true"
    } else {
        "false"
    }
}

fn json_string(value: &str) -> String {
    let mut escaped = String::new();
    for ch in value.chars() {
        match ch {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            '\u{08}' => escaped.push_str("\\b"),
            '\u{0c}' => escaped.push_str("\\f"),
            ch if ch < '\u{20}' => escaped.push_str(&format!("\\u{:04x}", ch as u32)),
            _ => escaped.push(ch),
        }
    }
    format!("\"{}\"", escaped)
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn record_write(outcome: WriteOutcome, stats: &mut InitStats) {
    match outcome {
        WriteOutcome::Created => stats.created_files += 1,
        WriteOutcome::Skipped => stats.skipped_files += 1,
    }
}

fn write_if_allowed(path: &Path, content: &str, force: bool) -> Result<WriteOutcome> {
    if path.exists() && !force {
        return Ok(WriteOutcome::Skipped);
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    fs::write(path, content)?;
    Ok(WriteOutcome::Created)
}

fn render_template(rel_path: &str, docs_root: &str) -> Result<String> {
    let raw = load_bundled_text(rel_path)?;
    Ok(raw.replace("{{docs_root}}", docs_root))
}

fn render_entrypoint_template(
    rel_path: &str,
    schema: &Schema,
    paths: &RenderPaths,
) -> Result<String> {
    let raw = load_bundled_text(rel_path)?;
    Ok(raw
        .replace("{{docs_root}}", &schema.managed_root)
        .replace("{{architecture_path}}", &paths.architecture)
        .replace("{{manifest_path}}", &paths.manifest)
        .replace("{{project_map_path}}", &paths.project_map)
        .replace("{{commands_path}}", &paths.commands)
        .replace("{{product_specs_index_path}}", &paths.product_specs_index)
        .replace("{{design_docs_index_path}}", &paths.design_docs_index)
        .replace("{{decisions_index_path}}", &paths.decisions_index)
        .replace(
            "{{exec_plans_active_index_path}}",
            &paths.exec_plans_active_index,
        )
        .replace("{{worklog_active_index_path}}", &paths.worklog_active_index)
        .replace("{{generated_index_path}}", &paths.generated_index)
        .replace("{{references_index_path}}", &paths.references_index))
}

fn render_core_template(rel_path: &str, schema: &Schema, paths: &RenderPaths) -> Result<String> {
    let mut raw = load_bundled_text(rel_path)?;
    raw = raw.replace("{{docs_root}}", &schema.managed_root);
    raw = raw.replace("{{architecture_path}}", &paths.architecture);
    raw = raw.replace("{{manifest_path}}", &paths.manifest);
    raw = raw.replace("{{project_map_path}}", &paths.project_map);
    raw = raw.replace("{{commands_path}}", &paths.commands);
    raw = raw.replace("{{product_specs_index_path}}", &paths.product_specs_index);
    raw = raw.replace("{{design_docs_index_path}}", &paths.design_docs_index);
    raw = raw.replace("{{decisions_index_path}}", &paths.decisions_index);

    if rel_path.ends_with("index.md.tpl") {
        raw = raw
            .replace("{{core_rows}}", &render_core_rows(schema, paths))
            .replace("{{collection_rows}}", &render_collection_rows(schema))
            .replace("{{template_rows}}", &render_template_rows(schema));
    }

    Ok(raw)
}

fn render_template_rows(schema: &Schema) -> String {
    let mut seen = BTreeSet::new();
    let mut rows = Vec::new();

    for spec in &schema.doc_collections {
        if seen.insert(spec.template.clone()) {
            rows.push(format!(
                "| `{}/templates/{}.md` | Starter template for `{}` docs |",
                schema.managed_root, spec.template, spec.path
            ));
        }
    }

    rows.join("\n")
}

fn render_core_rows(schema: &Schema, paths: &RenderPaths) -> String {
    let mut rows = [
        format!(
            "| `{}` | Project orientation and stable boundaries | Active |",
            paths.architecture
        ),
        format!(
            "| `{}` | Top-level docs manifest and reading order | Active |",
            paths.manifest
        ),
        format!(
            "| `{}` | Current project map and active focus | Active |",
            paths.project_map
        ),
        format!(
            "| `{}` | Documentation rules and update policy | Stable |",
            format!("{}/DOCUMENTATION_SYSTEM.md", schema.managed_root)
        ),
        format!(
            "| `{}` | Validation and operating commands | Active |",
            paths.commands
        ),
    ];
    rows.sort();
    rows.join("\n")
}

fn render_collection_rows(schema: &Schema) -> String {
    let mut rows = Vec::new();

    for spec in &schema.doc_collections {
        let authority = spec.authority.as_deref().unwrap_or("unknown");
        let status = render_default_status(spec);
        let template = format!("{}/templates/{}.md", schema.managed_root, spec.template);
        rows.push(format!(
            "| `{}/{}/index.md` | {} | `{}` | `{}` | `{}` |",
            schema.managed_root,
            spec.path,
            collection_purpose(spec),
            authority,
            status,
            template
        ));
    }

    rows.join("\n")
}

fn render_collection_index_template(
    schema: &Schema,
    spec: &DocCollectionSpec,
    paths: &RenderPaths,
) -> Result<String> {
    let raw = load_bundled_text("templates/core/collection-index.md.tpl")?;
    let title = collection_title(spec);
    let purpose = collection_purpose(spec);
    let when_to_read = collection_when_to_read(spec);
    let default_authority = spec.authority.as_deref().unwrap_or("unspecified");
    let default_status = render_default_status(spec);
    let anchor_guidance = if spec.anchor_candidate { "yes" } else { "no" };
    let allowed_frontmatter = render_allowed_frontmatter(spec);
    let template_path = format!("{}/templates/{}.md", schema.managed_root, spec.template);
    let collection_repo_path = format!("{}/{}", schema.managed_root, spec.path);
    let collection_index_path = format!("{}/index.md", collection_repo_path);

    Ok(raw
        .replace("{{collection_title}}", &title)
        .replace("{{collection_repo_path}}", &collection_repo_path)
        .replace("{{collection_purpose}}", &purpose)
        .replace("{{collection_when_to_read}}", &when_to_read)
        .replace("{{default_authority}}", default_authority)
        .replace("{{default_status}}", default_status)
        .replace("{{anchor_guidance}}", anchor_guidance)
        .replace("{{template_path}}", &template_path)
        .replace("{{allowed_frontmatter}}", &allowed_frontmatter)
        .replace("{{architecture_path}}", &paths.architecture)
        .replace("{{manifest_path}}", &paths.manifest)
        .replace("{{collection_index_path}}", &collection_index_path))
}

fn collection_label(name: &str, path: &str) -> String {
    match name {
        "product_specs" => "product specification".to_string(),
        "design_docs" => "design".to_string(),
        "decisions" => "decision".to_string(),
        "exec_plans_active" => "active execution plan".to_string(),
        "exec_plans_completed" => "completed execution plan".to_string(),
        "worklog_active" => "active worklog".to_string(),
        "worklog_archive" => "archived worklog".to_string(),
        "operations" => "operation".to_string(),
        "generated" => "generated artifact".to_string(),
        "references" => "reference".to_string(),
        _ => path.replace(['-', '/'], " "),
    }
}

fn collection_title(spec: &DocCollectionSpec) -> String {
    match spec.name.as_str() {
        "product_specs" => "Product Specs Index".to_string(),
        "design_docs" => "Design Docs Index".to_string(),
        "decisions" => "Decisions Index".to_string(),
        "exec_plans_active" => "Active Execution Plans Index".to_string(),
        "exec_plans_completed" => "Completed Execution Plans Index".to_string(),
        "worklog_active" => "Active Worklog Index".to_string(),
        "worklog_archive" => "Archived Worklog Index".to_string(),
        "operations" => "Operations Index".to_string(),
        "generated" => "Generated Artifacts Index".to_string(),
        "references" => "References Index".to_string(),
        _ => format!("{} Index", collection_label(&spec.name, &spec.path)),
    }
}

fn collection_purpose(spec: &DocCollectionSpec) -> String {
    match spec.name.as_str() {
        "product_specs" => {
            "User-visible behavior, product intent, and feature boundaries.".to_string()
        }
        "design_docs" => "Technical designs, tradeoffs, and implementation boundaries.".to_string(),
        "decisions" => "Durable decisions and their consequences.".to_string(),
        "exec_plans_active" => {
            "Current multi-step work that may need recovery or continuation.".to_string()
        }
        "exec_plans_completed" => {
            "Archived execution plans kept for history and recovery reference.".to_string()
        }
        "worklog_active" => {
            "Ongoing change logs, validation notes, and handoff context.".to_string()
        }
        "worklog_archive" => {
            "Archived worklogs kept for traceability, not first-pass reading.".to_string()
        }
        "operations" => "Operational procedures, runbooks, and maintenance guidance.".to_string(),
        "generated" => {
            "Generated reports, inventories, and machine-produced artifacts.".to_string()
        }
        "references" => "External or imported material used as supporting reference.".to_string(),
        _ => format!("Focused docs for `{}`.", spec.path),
    }
}

fn collection_when_to_read(spec: &DocCollectionSpec) -> String {
    match spec.name.as_str() {
        "product_specs" => "- Read when you need expected behavior, scope, or user-facing intent.".to_string(),
        "design_docs" => "- Read when you need architecture, tradeoffs, or implementation constraints.".to_string(),
        "decisions" => "- Read when a prior decision may constrain the current task.".to_string(),
        "exec_plans_active" => "- Read when the task is part of ongoing implementation work or may need resume support.".to_string(),
        "exec_plans_completed" => "- Read only when current active docs are insufficient and historical execution context matters.".to_string(),
        "worklog_active" => "- Read when you need recent validation, failures, or handoff details.".to_string(),
        "worklog_archive" => "- Read only for historical traceability.".to_string(),
        "operations" => "- Read when the task touches operating procedures or maintenance.".to_string(),
        "generated" => "- Read only when you need generated evidence or inventories.".to_string(),
        "references" => "- Read only when focused docs point here or external material is required.".to_string(),
        _ => "- Read when this collection is the smallest relevant source of truth.".to_string(),
    }
}

fn render_default_status(spec: &DocCollectionSpec) -> &str {
    spec.status.as_deref().unwrap_or("not set by default")
}

fn render_allowed_frontmatter(spec: &DocCollectionSpec) -> String {
    if spec.allowed_frontmatter.is_empty() {
        "none by default".to_string()
    } else {
        spec.allowed_frontmatter
            .iter()
            .map(|field| format!("`{}`", field))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

fn build_render_paths(schema: &Schema) -> RenderPaths {
    let manifest = schema
        .entrypoints
        .manifest
        .clone()
        .unwrap_or_else(|| format!("{}/index.md", schema.managed_root));
    let project_map = schema
        .entrypoints
        .project_map
        .clone()
        .unwrap_or_else(|| format!("{}/project.md", schema.managed_root));
    let architecture = resolve_architecture_path(schema, &project_map);
    let commands = format!("{}/commands.md", schema.managed_root);

    RenderPaths {
        manifest,
        project_map,
        architecture,
        commands,
        product_specs_index: collection_index_path(schema, "product_specs", "product-specs"),
        design_docs_index: collection_index_path(schema, "design_docs", "design-docs"),
        decisions_index: collection_index_path(schema, "decisions", "decisions"),
        exec_plans_active_index: collection_index_path(
            schema,
            "exec_plans_active",
            "exec-plans/active",
        ),
        worklog_active_index: collection_index_path(schema, "worklog_active", "worklog/active"),
        generated_index: collection_index_path(schema, "generated", "generated"),
        references_index: collection_index_path(schema, "references", "references"),
    }
}

fn collection_index_path(schema: &Schema, name: &str, fallback: &str) -> String {
    for spec in &schema.doc_collections {
        if spec.name == name {
            return format!("{}/{}/index.md", schema.managed_root, spec.path);
        }
    }

    format!("{}/{}/index.md", schema.managed_root, fallback)
}

fn configured_architecture_path(schema: &Schema) -> Option<String> {
    if let Some(path) = &schema.entrypoints.architecture {
        return Some(path.clone());
    }

    for spec in &schema.core_files {
        if spec.template == "architecture" {
            return Some(core_file_rel_path(schema, spec));
        }
    }

    None
}

fn resolve_architecture_path(schema: &Schema, fallback: &str) -> String {
    configured_architecture_path(schema).unwrap_or_else(|| fallback.to_string())
}

fn load_bundled_text(rel_path: &str) -> Result<String> {
    let abs = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel_path);
    Ok(fs::read_to_string(abs)?)
}

fn core_template_path(name: &str) -> Option<&'static str> {
    match name {
        "architecture" => Some("templates/core/ARCHITECTURE.md.tpl"),
        "docs-index" => Some("templates/core/index.md.tpl"),
        "project-map" => Some("templates/core/project.md.tpl"),
        "documentation-system" => Some("templates/core/DOCUMENTATION_SYSTEM.md.tpl"),
        "commands" => Some("templates/core/commands.md.tpl"),
        _ => None,
    }
}

fn doc_template_path(name: &str) -> Option<&'static str> {
    match name {
        "product-spec" => Some("templates/docs/product-spec.md.tpl"),
        "design-doc" => Some("templates/docs/design-doc.md.tpl"),
        "decision" => Some("templates/docs/decision.md.tpl"),
        "exec-plan" => Some("templates/docs/exec-plan.md.tpl"),
        "worklog" => Some("templates/docs/worklog.md.tpl"),
        "operation" => Some("templates/docs/operation.md.tpl"),
        "generated" => Some("templates/docs/generated.md.tpl"),
        "reference" => Some("templates/docs/reference.md.tpl"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn bundled_schema() -> Schema {
        let raw =
            fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(BUNDLED_SCHEMA_PATH))
                .unwrap();
        parse_schema(&raw).unwrap()
    }

    fn unique_temp_dir(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = env::temp_dir().join(format!(
            "harnesskit-{}-{}-{}",
            name,
            std::process::id(),
            nanos
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn bundled_schema_exposes_architecture_and_collections() {
        let schema = bundled_schema();

        assert_eq!(schema.managed_root, "docs");
        assert_eq!(
            schema.entrypoints.architecture.as_deref(),
            Some("ARCHITECTURE.md")
        );
        assert!(schema
            .core_files
            .iter()
            .any(|spec| spec.template == "architecture"
                && spec.root_path.as_deref() == Some("ARCHITECTURE.md")));
        assert!(schema
            .doc_collections
            .iter()
            .any(|spec| spec.name == "product_specs" && spec.path == "product-specs"));
        assert!(schema
            .doc_collections
            .iter()
            .any(|spec| spec.name == "exec_plans_active" && spec.path == "exec-plans/active"));
    }

    #[test]
    fn init_materializes_progressive_disclosure_scaffold() {
        let target = unique_temp_dir("default-init");
        let schema = bundled_schema();
        let schema_copy = render_schema_copy(&schema);

        let stats = materialize_from_schema(&target, &schema, &schema_copy, false).unwrap();

        assert!(stats.created_files >= 20);
        assert!(target.join("AGENTS.md").exists());
        assert!(target.join("CLAUDE.md").exists());
        assert!(target.join("ARCHITECTURE.md").exists());
        assert!(target.join("docs/index.md").exists());
        assert!(target.join("docs/product-specs/index.md").exists());
        assert!(target.join("docs/design-docs/index.md").exists());
        assert!(target.join("docs/decisions/index.md").exists());
        assert!(target.join("docs/exec-plans/active/index.md").exists());
        assert!(target.join("docs/worklog/active/index.md").exists());
        assert!(target.join("docs/templates/design-doc.md").exists());
        assert!(target.join(".harnesskit/schema.yaml").exists());

        let agents = fs::read_to_string(target.join("AGENTS.md")).unwrap();
        assert!(agents.contains("ARCHITECTURE.md"));
        assert!(agents.contains("docs/product-specs/index.md"));

        let docs_index = fs::read_to_string(target.join("docs/index.md")).unwrap();
        assert!(docs_index.contains("docs/design-docs/index.md"));
        assert!(docs_index.contains("docs/templates/design-doc.md"));

        fs::remove_dir_all(target).unwrap();
    }

    #[test]
    fn docs_root_override_rewrites_generated_paths() {
        let target = unique_temp_dir("docs-root-override");
        let mut schema = bundled_schema();
        let old_root = schema.managed_root.clone();
        schema.managed_root = "repo-docs".to_string();
        rewrite_docs_root_bound_paths(&mut schema, &old_root, "repo-docs");
        let schema_copy = render_schema_copy(&schema);

        materialize_from_schema(&target, &schema, &schema_copy, false).unwrap();

        assert!(target.join("repo-docs/index.md").exists());
        assert!(target.join("repo-docs/product-specs/index.md").exists());
        assert!(target.join("repo-docs/templates/product-spec.md").exists());

        let agents = fs::read_to_string(target.join("AGENTS.md")).unwrap();
        assert!(agents.contains("repo-docs/index.md"));
        assert!(agents.contains("repo-docs/product-specs/index.md"));

        let copied_schema = fs::read_to_string(target.join(".harnesskit/schema.yaml")).unwrap();
        assert!(copied_schema.contains("managed_root: repo-docs"));

        fs::remove_dir_all(target).unwrap();
    }

    #[test]
    fn parse_schema_rejects_invalid_boolean_values() {
        let raw = r#"
schema_version: 0
managed_root: docs

entrypoints:
  agents: AGENTS.md
  manifest: docs/index.md
  project_map: docs/project.md

core_files:
  index:
    path: index.md
    required: maybe
    template: docs-index

doc_collections:
  product_specs:
    path: product-specs
    authority: canonical
    anchor_candidate: true
    template: product-spec
    allowed_frontmatter: []

rules:
  path_defines_doc_type: true
"#;

        match parse_schema(raw) {
            Ok(_) => panic!("expected parse_schema to reject invalid boolean"),
            Err(err) => assert!(err.to_string().contains("expected boolean")),
        }
    }

    #[test]
    fn parse_schema_rejects_tabs_with_clear_message() {
        let raw = "schema_version: 0\nmanaged_root: docs\nentrypoints:\n\tagents: AGENTS.md\n";

        match parse_schema(raw) {
            Ok(_) => panic!("expected parse_schema to reject tab indentation"),
            Err(err) => assert!(err.to_string().contains("tab indentation")),
        }
    }

    #[test]
    fn parse_inline_list_respects_quoted_commas() {
        let values = parse_inline_list("[status, \"owner, team\", authority]");

        assert_eq!(
            values,
            vec![
                "status".to_string(),
                "owner, team".to_string(),
                "authority".to_string()
            ]
        );
    }

    #[test]
    fn command_options_no_strict_overrides_schema_strict() {
        let mut schema = Schema::default();
        schema.rules.strict_checks = Some(true);
        let args = vec!["--no-strict".to_string()];

        let options = command_options(&args, Some(&schema));

        assert!(!options.strict);
    }

    #[test]
    fn index_builds_docs_relations_and_checks() {
        let target = unique_temp_dir("index-artifact");
        let schema = bundled_schema();
        let schema_copy = render_schema_copy(&schema);

        materialize_from_schema(&target, &schema, &schema_copy, false).unwrap();

        fs::write(
            target.join("docs/product-specs/example.md"),
            "---\nstatus: active\nauthority: canonical\nscope_paths:\n  - src/example/\n---\n\n# Product Spec: Example\n\nThis spec references `docs/design-docs/example-design.md`.\n",
        )
        .unwrap();
        fs::write(
            target.join("docs/design-docs/example-design.md"),
            "---\nstatus: active\nauthority: canonical\nsupersedes:\n  - docs/design-docs/older.md\n---\n\n# Design Doc: Example Design\n\nImplements the product spec in `docs/product-specs/example.md`.\n",
        )
        .unwrap();
        let artifact = build_index_artifact(&target, &schema).unwrap();

        assert!(artifact
            .docs
            .iter()
            .any(|doc| doc.path == "docs/product-specs/example.md"));
        assert!(artifact
            .docs
            .iter()
            .any(|doc| doc.path == "docs/design-docs/example-design.md"));
        assert!(artifact
            .relations
            .iter()
            .any(|relation| relation.relation_type == "doc_indexes_doc"
                && relation.dst_path == "docs/product-specs/example.md"));
        assert!(artifact
            .relations
            .iter()
            .any(|relation| relation.relation_type == "doc_scopes_code"
                && relation.dst_path == "src/example"));
        assert!(artifact
            .relations
            .iter()
            .any(|relation| relation.relation_type == "supersedes"
                && relation.dst_path == "docs/design-docs/older.md"));
        assert!(artifact
            .docs
            .iter()
            .any(|doc| doc.path == "AGENTS.md" && doc.is_entrypoint));
        assert!(artifact
            .docs
            .iter()
            .any(|doc| doc.path == "docs/index.md" && doc.role == "manifest"));

        let json = render_index_json(&artifact);
        assert!(json.contains("\"docs\""));
        assert!(json.contains("\"relations\""));
        assert!(json.contains("\"checks\""));

        fs::remove_dir_all(target).unwrap();
    }

    #[test]
    fn fenced_markdown_links_do_not_create_doc_relations() {
        let target = unique_temp_dir("fenced-markdown-link");
        let schema = bundled_schema();
        let schema_copy = render_schema_copy(&schema);

        materialize_from_schema(&target, &schema, &schema_copy, false).unwrap();
        fs::write(
            target.join("docs/design-docs/source.md"),
            "---\nstatus: active\nauthority: canonical\n---\n\n# Design Doc: Source\n\n```md\n[Target](target.md)\n```\n",
        )
        .unwrap();
        fs::write(
            target.join("docs/design-docs/target.md"),
            "---\nstatus: active\nauthority: canonical\n---\n\n# Design Doc: Target\n",
        )
        .unwrap();

        let artifact = build_index_artifact(&target, &schema).unwrap();

        assert!(!artifact.relations.iter().any(|relation| {
            relation.src_path == "docs/design-docs/source.md"
                && relation.dst_path == "docs/design-docs/target.md"
                && relation.reason == "markdown-link"
        }));

        fs::remove_dir_all(target).unwrap();
    }

    #[test]
    fn fenced_headings_do_not_enter_doc_title_or_headings() {
        let parsed = parse_doc_text(
            "docs/design-docs/fenced-heading.md",
            "---\nstatus: active\nauthority: canonical\n---\n\n```md\n# Fake Title\n## Fake Section\n```\n\n# Real Title\n\n## Real Section\n",
        );

        assert_eq!(parsed.title.as_deref(), Some("Real Title"));
        assert_eq!(
            parsed.headings,
            vec!["Real Title".to_string(), "Real Section".to_string()]
        );
    }

    #[test]
    fn short_backtick_lines_are_not_treated_as_fences() {
        let parsed = parse_doc_text(
            "docs/design-docs/source.md",
            "---\nstatus: active\nauthority: canonical\n---\n\n# Source\n\n`` [Target](target.md)\n",
        );

        assert_eq!(parsed.links, vec!["docs/design-docs/target.md".to_string()]);
    }

    #[test]
    fn summary_ignores_fenced_code_blocks() {
        let parsed = parse_doc_text(
            "docs/design-docs/summary.md",
            "---\nstatus: active\nauthority: canonical\n---\n\n```md\nThis fenced example should not become the summary.\n```\n\nReal summary starts here.\n\nMore detail.\n",
        );

        assert_eq!(parsed.summary, "Real summary starts here.");
    }

    #[test]
    fn split_frontmatter_accepts_closing_marker_at_eof() {
        let parsed = parse_doc_text(
            "docs/design-docs/frontmatter-only.md",
            "---\nstatus: active\nauthority: canonical\n---",
        );

        assert_eq!(
            parsed.frontmatter.get("status").map(String::as_str),
            Some("active")
        );
        assert_eq!(
            parsed.frontmatter.get("authority").map(String::as_str),
            Some("canonical")
        );
        assert!(parsed.body_text.is_empty());
    }

    #[test]
    fn parse_doc_text_recognizes_crlf_frontmatter() {
        let parsed = parse_doc_text(
            "docs/design-docs/crlf.md",
            "---\r\nstatus: active\r\nauthority: canonical\r\nscope_paths:\r\n  - src/example\r\n---\r\n\r\n# CRLF Doc\r\n\r\nSummary line.\r\n",
        );

        assert_eq!(
            parsed.frontmatter.get("status").map(String::as_str),
            Some("active")
        );
        assert_eq!(
            parsed.frontmatter.get("authority").map(String::as_str),
            Some("canonical")
        );
        assert_eq!(parsed.scope_paths, vec!["src/example".to_string()]);
        assert_eq!(parsed.title.as_deref(), Some("CRLF Doc"));
    }

    #[test]
    fn project_map_is_not_architecture_when_architecture_is_unconfigured() {
        let raw = r#"
schema_version: 0
managed_root: docs

entrypoints:
  agents: AGENTS.md
  manifest: docs/index.md
  project_map: docs/project.md

core_files:
  project_map:
    path: project.md
    required: true
    template: project-map

doc_collections:
  design_docs:
    path: design-docs
    authority: canonical
    anchor_candidate: true
    template: design-doc
    allowed_frontmatter: [status, authority]

rules:
  index_required: true
"#;
        let schema = parse_schema(raw).unwrap();

        assert_eq!(infer_index_role(&schema, "docs/project.md", None), "doc");
    }

    #[test]
    fn collection_authority_marks_generated_and_reference_docs() {
        let raw = r#"
schema_version: 0
managed_root: docs

entrypoints:
  agents: AGENTS.md
  manifest: docs/index.md
  project_map: docs/project.md

core_files:
  index:
    path: index.md
    required: true
    template: docs-index

doc_collections:
  machine_outputs:
    path: machine-outputs
    authority: generated
    anchor_candidate: false
    template: generated
    allowed_frontmatter: [status, authority]
  source_notes:
    path: source-notes
    authority: reference
    anchor_candidate: false
    template: reference
    allowed_frontmatter: [status, authority]

rules:
  index_required: true
"#;
        let target = unique_temp_dir("authority-flags");
        let schema = parse_schema(raw).unwrap();
        fs::create_dir_all(target.join("docs/machine-outputs")).unwrap();
        fs::create_dir_all(target.join("docs/source-notes")).unwrap();
        fs::write(target.join(".harnesskit-schema-placeholder"), "").unwrap();
        fs::write(target.join("AGENTS.md"), "# Agents\n").unwrap();
        fs::write(target.join("docs/index.md"), "# Index\n").unwrap();
        fs::write(target.join("docs/project.md"), "# Project\n").unwrap();
        fs::write(target.join("docs/machine-outputs/report.md"), "# Report\n").unwrap();
        fs::write(target.join("docs/source-notes/source.md"), "# Source\n").unwrap();
        fs::create_dir_all(target.join(".harnesskit")).unwrap();
        fs::write(target.join(".harnesskit/schema.yaml"), raw).unwrap();

        let artifact = build_index_artifact(&target, &schema).unwrap();
        let generated = artifact
            .docs
            .iter()
            .find(|doc| doc.path == "docs/machine-outputs/report.md")
            .unwrap();
        let reference = artifact
            .docs
            .iter()
            .find(|doc| doc.path == "docs/source-notes/source.md")
            .unwrap();

        assert!(generated.is_generated);
        assert!(!generated.is_reference);
        assert!(reference.is_reference);
        assert!(!reference.is_generated);

        fs::remove_dir_all(target).unwrap();
    }

    #[test]
    fn index_reads_non_utf8_markdown_lossily() {
        let target = unique_temp_dir("non-utf8-markdown");
        let schema = bundled_schema();
        let schema_copy = render_schema_copy(&schema);

        materialize_from_schema(&target, &schema, &schema_copy, false).unwrap();
        fs::write(
            target.join("docs/design-docs/non-utf8.md"),
            b"---\nstatus: active\nauthority: canonical\n---\n\n# Non UTF8\n\nInvalid byte: \xff\n",
        )
        .unwrap();

        let artifact = build_index_artifact(&target, &schema).unwrap();

        assert!(artifact
            .docs
            .iter()
            .any(|doc| doc.path == "docs/design-docs/non-utf8.md"));

        fs::remove_dir_all(target).unwrap();
    }

    #[test]
    fn index_only_extracts_path_mentions_from_inline_code() {
        let target = unique_temp_dir("index-inline-code");
        let schema = bundled_schema();
        let schema_copy = render_schema_copy(&schema);

        materialize_from_schema(&target, &schema, &schema_copy, false).unwrap();

        fs::write(
            target.join("docs/product-specs/example.md"),
            "---\nstatus: active\nauthority: canonical\n---\n\n# Product Spec: Example\n\nPlain text mention docs/design-docs/example-design.md should stay weak and ignored.\n\nInline code mention `docs/design-docs/example-design.md` should become a relation.\n\n```md\nfenced docs/design-docs/example-design.md should not count\n```\n",
        )
        .unwrap();
        fs::write(
            target.join("docs/design-docs/example-design.md"),
            "---\nstatus: active\nauthority: canonical\n---\n\n# Design Doc: Example Design\n",
        )
        .unwrap();

        let artifact = build_index_artifact(&target, &schema).unwrap();
        let matching_relations = artifact
            .relations
            .iter()
            .filter(|relation| {
                relation.src_path == "docs/product-specs/example.md"
                    && relation.dst_path == "docs/design-docs/example-design.md"
            })
            .collect::<Vec<_>>();

        assert_eq!(matching_relations.len(), 1);
        assert_eq!(
            matching_relations[0].relation_type,
            "doc_mentions_code_path"
        );
        assert_eq!(matching_relations[0].reason, "inline-code-path");

        fs::remove_dir_all(target).unwrap();
    }

    #[test]
    fn inline_code_triple_backticks_do_not_toggle_fence() {
        let mentions =
            extract_inline_code_path_mentions("Inline `src/lib.rs ``` docs/project.md` mention.\n");

        assert!(mentions.contains(&"src/lib.rs".to_string()));
        assert!(mentions.contains(&"docs/project.md".to_string()));
    }

    #[test]
    fn index_emits_missing_doc_checks_when_required_docs_are_absent() {
        let target = unique_temp_dir("index-missing-docs");
        let schema = bundled_schema();
        let schema_copy = render_schema_copy(&schema);

        materialize_from_schema(&target, &schema, &schema_copy, false).unwrap();

        fs::remove_file(target.join("CLAUDE.md")).unwrap();
        fs::remove_file(target.join("docs/commands.md")).unwrap();
        fs::remove_file(target.join("docs/generated/index.md")).unwrap();

        let artifact = build_index_artifact(&target, &schema).unwrap();

        assert!(artifact
            .checks
            .iter()
            .any(|check| check.rule_id == "missing-entrypoint-doc"
                && check.subject_path == "CLAUDE.md"));
        assert!(artifact
            .checks
            .iter()
            .any(|check| check.rule_id == "missing-required-doc"
                && check.subject_path == "docs/commands.md"));
        assert!(artifact
            .checks
            .iter()
            .any(|check| check.rule_id == "missing-collection-index"
                && check.subject_path == "docs/generated/index.md"));

        fs::remove_dir_all(target).unwrap();
    }

    #[test]
    fn fact_store_persists_file_states_and_relations() {
        let target = unique_temp_dir("fact-store");
        let schema = bundled_schema();
        let schema_copy = render_schema_copy(&schema);

        materialize_from_schema(&target, &schema, &schema_copy, false).unwrap();
        fs::write(
            target.join("docs/design-docs/example-design.md"),
            "---\nstatus: active\nauthority: canonical\n---\n\n# Design Doc: Example Design\n\nLinks to [project map](../project.md).\n",
        )
        .unwrap();

        let artifact = build_index_artifact(&target, &schema).unwrap();
        write_fact_store(&target, &artifact).unwrap();

        let db = open_fact_store(&target).unwrap();
        let file_states = db
            .query("SELECT path, content_hash, file_size_bytes, mtime_unix FROM file_states WHERE path = 'docs/design-docs/example-design.md';")
            .unwrap();
        assert_eq!(file_states.rows.len(), 1);
        assert_eq!(
            cell(&file_states.rows[0], 0),
            "docs/design-docs/example-design.md"
        );
        assert!(!cell(&file_states.rows[0], 1).is_empty());
        assert!(cell(&file_states.rows[0], 2).parse::<u64>().unwrap() > 0);

        let relations = db
            .query("SELECT relation_type, confidence, evidence_json FROM relations WHERE src_path = 'docs/design-docs/example-design.md';")
            .unwrap();
        assert!(relations
            .rows
            .iter()
            .any(|row| cell(row, 0) == "doc_links_doc" || cell(row, 0) == "doc_indexes_doc"));
        assert!(relations
            .rows
            .iter()
            .all(|row| !cell(row, 1).is_empty() && cell(row, 2).contains("\"reason\"")));

        fs::remove_dir_all(target).unwrap();
    }

    #[test]
    fn fact_store_read_preserves_exact_mtime_ns_from_file_states() {
        let target = unique_temp_dir("fact-store-mtime-ns");
        let schema = bundled_schema();
        let schema_copy = render_schema_copy(&schema);

        materialize_from_schema(&target, &schema, &schema_copy, false).unwrap();
        let artifact = build_index_artifact(&target, &schema).unwrap();
        write_fact_store(&target, &artifact).unwrap();

        let db = open_fact_store(&target).unwrap();
        db.exec(
            "UPDATE file_states SET mtime_ns = 1234567890123456789 WHERE path = 'docs/project.md';",
        )
        .unwrap();
        let read_back = read_index_artifact_from_fact_store(&db).unwrap();
        let project = read_back
            .docs
            .iter()
            .find(|doc| doc.path == "docs/project.md")
            .unwrap();

        assert_eq!(project.mtime_ns, 1234567890123456789);

        fs::remove_dir_all(target).unwrap();
    }

    #[test]
    fn fact_store_state_path_errors_include_recovery_hint() {
        let target = unique_temp_dir("fact-store-state-path-error");
        fs::create_dir_all(target.join(".harnesskit")).unwrap();
        fs::write(target.join(".harnesskit/state"), "not a directory").unwrap();

        let err = match open_fact_store(&target) {
            Ok(_) => panic!("expected fact store open to fail"),
            Err(err) => err.to_string(),
        };

        assert!(err.contains("HarnessKit fact store"));
        assert!(err.contains(".harnesskit"));
        assert!(err.contains("target_dir:"));
        assert!(err.contains("current_dir:"));
        assert!(err.contains("PWD:"));
        assert!(err.contains("facts_path:"));
        assert!(err.contains("intended writable project root"));
        assert!(err.contains("Do not delete `.harnesskit/history`"));

        fs::remove_dir_all(target).unwrap();
    }

    #[test]
    fn fact_store_refresh_detects_file_changes() {
        let target = unique_temp_dir("fact-refresh");
        let schema = bundled_schema();
        let schema_copy = render_schema_copy(&schema);

        materialize_from_schema(&target, &schema, &schema_copy, false).unwrap();
        let artifact = build_index_artifact(&target, &schema).unwrap();
        write_fact_store(&target, &artifact).unwrap();

        let context = EngineContext {
            target_dir: target.clone(),
            schema: schema.clone(),
        };
        let delta = compute_fact_store_delta(&context).unwrap();
        assert!(delta.added_paths.is_empty());
        assert!(delta.changed_paths.is_empty());
        assert!(delta.removed_paths.is_empty());

        fs::write(
            target.join("docs/project.md"),
            "---\nstatus: active\nauthority: canonical\n---\n\n# Project Map\n\nChanged summary.\n",
        )
        .unwrap();
        let delta = compute_fact_store_delta(&context).unwrap();
        assert!(!delta.changed_paths.is_empty());

        fs::remove_dir_all(target).unwrap();
    }

    #[test]
    fn query_sql_returns_scores_and_neighbors() {
        let target = unique_temp_dir("query-surface");
        let schema = bundled_schema();
        let schema_copy = render_schema_copy(&schema);

        materialize_from_schema(&target, &schema, &schema_copy, false).unwrap();
        fs::write(
            target.join("docs/design-docs/schema-engine.md"),
            "---\nstatus: active\nauthority: canonical\n---\n\n# Design Doc: Schema Engine\n\nThis document explains schema parsing and query behavior.\n\nLinks to [project map](../project.md).\n",
        )
        .unwrap();
        let artifact = build_index_artifact(&target, &schema).unwrap();
        write_fact_store(&target, &artifact).unwrap();
        let db = open_fact_store(&target).unwrap();

        let query = sql_string_literal("schema");
        let result = db
            .query(&format!(
                "WITH lexical AS (
                    SELECT d.path,
                           d.title,
                           d.role,
                           COALESCE(d.status, '') AS status,
                           d.summary,
                           d.incoming_links,
                           CASE
                               WHEN lower(d.title) LIKE '%' || lower({0}) || '%' THEN 4.0
                               WHEN lower(d.path) LIKE '%' || lower({0}) || '%' THEN 3.0
                               WHEN lower(d.headings_text) LIKE '%' || lower({0}) || '%' THEN 2.0
                               WHEN lower(d.summary) LIKE '%' || lower({0}) || '%' THEN 1.5
                               ELSE 0.0
                           END AS lexical_score,
                           CASE
                               WHEN lower(d.title) LIKE '%' || lower({0}) || '%' THEN 'title matched query'
                               WHEN lower(d.path) LIKE '%' || lower({0}) || '%' THEN 'path matched query'
                               WHEN lower(d.headings_text) LIKE '%' || lower({0}) || '%' THEN 'heading matched query'
                               WHEN lower(d.summary) LIKE '%' || lower({0}) || '%' THEN 'summary matched query'
                               ELSE ''
                           END AS why
                    FROM docs d
                    WHERE lower(d.path) LIKE '%' || lower({0}) || '%'
                       OR lower(d.title) LIKE '%' || lower({0}) || '%'
                       OR lower(d.headings_text) LIKE '%' || lower({0}) || '%'
                       OR lower(d.summary) LIKE '%' || lower({0}) || '%'
                ),
                fts AS (
                    SELECT d.path,
                           d.title,
                           d.role,
                           COALESCE(d.status, '') AS status,
                           d.summary,
                           d.incoming_links,
                           2.5 AS lexical_score,
                           'full-text matched query' AS why
                    FROM docs_fts f
                    JOIN docs d ON d.path = f.path
                    WHERE docs_fts MATCH {0}
                ),
                base AS (
                    SELECT * FROM lexical
                    UNION ALL
                    SELECT * FROM fts
                ),
                merged AS (
                    SELECT path,
                           max(title) AS title,
                           max(role) AS role,
                           max(status) AS status,
                           max(summary) AS summary,
                           max(incoming_links) AS incoming_links,
                           max(lexical_score) AS lexical_score,
                           group_concat(DISTINCT why) AS why
                    FROM base
                    GROUP BY path
                ),
                expanded AS (
                    SELECT m.path AS anchor_path,
                           r.dst_path AS neighbor_path
                    FROM merged m
                    JOIN relations r ON r.src_path = m.path
                    WHERE r.dst_path LIKE '%.md'
                    UNION
                    SELECT m.path AS anchor_path,
                           r.src_path AS neighbor_path
                    FROM merged m
                    JOIN relations r ON r.dst_path = m.path
                    WHERE r.src_path LIKE '%.md'
                ),
                neighbor_summary AS (
                    SELECT anchor_path,
                           group_concat(neighbor_path, ' | ') AS neighbors
                    FROM (
                        SELECT anchor_path, neighbor_path
                        FROM expanded
                        ORDER BY anchor_path, neighbor_path
                    )
                    GROUP BY anchor_path
                )
                SELECT m.path,
                       m.title,
                       m.role,
                       m.status,
                       printf('%.2f', m.lexical_score + (m.incoming_links * 0.05)) AS score,
                       m.why,
                       m.summary,
                       COALESCE(n.neighbors, '')
                FROM merged m
                LEFT JOIN neighbor_summary n ON n.anchor_path = m.path
                ORDER BY (m.lexical_score + (m.incoming_links * 0.05)) DESC, m.path ASC
                LIMIT 12;",
                query
            ))
            .unwrap();

        assert!(result
            .rows
            .iter()
            .any(|row| cell(row, 0) == "docs/design-docs/schema-engine.md"));
        let row = result
            .rows
            .iter()
            .find(|row| cell(row, 0) == "docs/design-docs/schema-engine.md")
            .unwrap();
        assert!(!cell(row, 4).is_empty());
        assert!(!cell(row, 5).is_empty());
        assert!(cell(row, 7).contains("docs/project.md"));

        fs::remove_dir_all(target).unwrap();
    }

    #[test]
    fn compute_fact_store_delta_detects_added_changed_and_removed_paths() {
        let target = unique_temp_dir("delta-detect");
        let schema = bundled_schema();
        let schema_copy = render_schema_copy(&schema);

        materialize_from_schema(&target, &schema, &schema_copy, false).unwrap();
        let artifact = build_index_artifact(&target, &schema).unwrap();
        write_fact_store(&target, &artifact).unwrap();

        fs::write(
            target.join("docs/project.md"),
            "---\nstatus: active\nauthority: canonical\n---\n\n# Project\n\nChanged body.\n",
        )
        .unwrap();
        fs::write(
            target.join("docs/design-docs/new-one.md"),
            "---\nstatus: active\nauthority: canonical\n---\n\n# Design Doc: New One\n",
        )
        .unwrap();
        fs::remove_file(target.join("docs/commands.md")).unwrap();

        let context = EngineContext {
            target_dir: target.clone(),
            schema: schema.clone(),
        };
        let delta = compute_fact_store_delta(&context).unwrap();

        assert!(delta.changed_paths.contains(&"docs/project.md".to_string()));
        assert!(delta
            .added_paths
            .contains(&"docs/design-docs/new-one.md".to_string()));
        assert!(delta
            .removed_paths
            .contains(&"docs/commands.md".to_string()));

        fs::remove_dir_all(target).unwrap();
    }

    #[test]
    fn incremental_refresh_updates_fact_store_without_dropping_unchanged_docs() {
        let target = unique_temp_dir("incremental-refresh");
        let schema = bundled_schema();
        let schema_copy = render_schema_copy(&schema);

        materialize_from_schema(&target, &schema, &schema_copy, false).unwrap();
        let artifact = build_index_artifact(&target, &schema).unwrap();
        write_index_artifact(&target, &artifact).unwrap();
        write_fact_store(&target, &artifact).unwrap();

        let unchanged_before = fs::read_to_string(target.join("docs/project.md")).unwrap();
        fs::write(
            target.join("docs/DOCUMENTATION_SYSTEM.md"),
            "---\nstatus: stable\nauthority: canonical\n---\n\n# Documentation System\n\nIncrementally updated summary.\n",
        )
        .unwrap();
        fs::remove_file(target.join("docs/commands.md")).unwrap();
        fs::write(
            target.join("docs/design-docs/incremental-added.md"),
            "---\nstatus: active\nauthority: canonical\n---\n\n# Design Doc: Incremental Added\n\nLinked from `docs/index.md`.\n",
        )
        .unwrap();

        let context = EngineContext {
            target_dir: target.clone(),
            schema: schema.clone(),
        };
        refresh_fact_store(&context).unwrap();

        let db = open_fact_store(&target).unwrap();
        assert_eq!(
            scalar_count(
                &db,
                "SELECT COUNT(*) FROM docs WHERE path = 'docs/DOCUMENTATION_SYSTEM.md';"
            )
            .unwrap(),
            1
        );
        assert_eq!(
            scalar_count(
                &db,
                "SELECT COUNT(*) FROM docs WHERE path = 'docs/commands.md';"
            )
            .unwrap(),
            0
        );
        assert_eq!(
            scalar_count(
                &db,
                "SELECT COUNT(*) FROM docs WHERE path = 'docs/design-docs/incremental-added.md';"
            )
            .unwrap(),
            1
        );
        let project_row = db
            .query("SELECT summary FROM docs WHERE path = 'docs/DOCUMENTATION_SYSTEM.md';")
            .unwrap();
        assert!(cell(&project_row.rows[0], 0).contains("Incrementally updated summary"));

        let meta_rows = db
            .query("SELECT key, value FROM meta WHERE key IN ('last_refresh_mode', 'last_delta_changed') ORDER BY key ASC;")
            .unwrap();
        assert!(meta_rows
            .rows
            .iter()
            .any(|row| cell(row, 0) == "last_refresh_mode" && cell(row, 1) == "delta"));
        assert!(meta_rows
            .rows
            .iter()
            .any(|row| cell(row, 0) == "last_delta_changed" && cell(row, 1) == "1"));

        let commands_row = db
            .query("SELECT summary FROM docs WHERE path = 'docs/project.md';")
            .unwrap();
        assert!(!cell(&commands_row.rows[0], 0).is_empty());
        assert_eq!(
            unchanged_before,
            fs::read_to_string(target.join("docs/project.md")).unwrap()
        );

        fs::remove_dir_all(target).unwrap();
    }

    #[test]
    fn delta_refresh_records_real_host_git_dirty() {
        let target = unique_temp_dir("delta-host-git-dirty");
        let schema = bundled_schema();
        let schema_copy = render_schema_copy(&schema);

        materialize_from_schema(&target, &schema, &schema_copy, false).unwrap();
        Command::new("git")
            .arg("init")
            .current_dir(&target)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(&target)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.name", "HarnessKit Test"])
            .current_dir(&target)
            .output()
            .unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(&target)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "baseline"])
            .current_dir(&target)
            .output()
            .unwrap();

        let artifact = build_index_artifact(&target, &schema).unwrap();
        assert!(!artifact.host_git_dirty);
        write_index_artifact(&target, &artifact).unwrap();
        write_fact_store(&target, &artifact).unwrap();

        fs::write(
            target.join("docs/project.md"),
            "---\nstatus: active\nauthority: canonical\n---\n\n# Project\n\nDirty delta body.\n",
        )
        .unwrap();
        let context = EngineContext {
            target_dir: target.clone(),
            schema: schema.clone(),
        };
        refresh_fact_store(&context).unwrap();

        let db = open_fact_store(&target).unwrap();
        let dirty = meta_value(&db, "host_git_dirty").unwrap();
        let mode = meta_value(&db, "last_refresh_mode").unwrap();
        assert_eq!(dirty.as_deref(), Some("true"));
        assert_eq!(mode.as_deref(), Some("delta"));

        fs::remove_dir_all(target).unwrap();
    }

    #[test]
    fn delta_quick_check_reuses_hash_when_file_metadata_is_unchanged() {
        let target = unique_temp_dir("delta-quick-check");
        let schema = bundled_schema();
        let schema_copy = render_schema_copy(&schema);

        materialize_from_schema(&target, &schema, &schema_copy, false).unwrap();
        let artifact = build_index_artifact(&target, &schema).unwrap();
        write_fact_store(&target, &artifact).unwrap();

        let db = open_fact_store(&target).unwrap();
        let rows = db
            .query("SELECT path, content_hash, file_size_bytes, mtime_unix, mtime_ns FROM file_states WHERE path = 'docs/project.md';")
            .unwrap();
        let previous = parse_file_state_rows(&rows);
        let current = collect_current_file_states(&target, &schema, &previous).unwrap();

        assert_eq!(
            previous.get("docs/project.md").unwrap().content_hash,
            current.get("docs/project.md").unwrap().content_hash
        );

        fs::remove_dir_all(target).unwrap();
    }

    #[test]
    fn refresh_upgrades_older_fact_store_schema() {
        let target = unique_temp_dir("schema-upgrade");
        let schema = bundled_schema();
        let schema_copy = render_schema_copy(&schema);

        materialize_from_schema(&target, &schema, &schema_copy, false).unwrap();
        let artifact = build_index_artifact(&target, &schema).unwrap();
        write_fact_store(&target, &artifact).unwrap();

        let db = open_fact_store(&target).unwrap();
        db.exec("DROP TABLE path_mentions;").unwrap();

        fs::write(
            target.join("docs/project.md"),
            "---\nstatus: active\nauthority: canonical\n---\n\n# Project\n\nUpgrade path check.\n",
        )
        .unwrap();

        let context = EngineContext {
            target_dir: target.clone(),
            schema: schema.clone(),
        };
        refresh_fact_store(&context).unwrap();

        let db = open_fact_store(&target).unwrap();
        let table_rows = db
            .query("SELECT name FROM sqlite_master WHERE type='table' AND name='path_mentions';")
            .unwrap();
        assert_eq!(table_rows.rows.len(), 1);
        let version_rows = db
            .query("SELECT value FROM meta WHERE key='fact_schema_version';")
            .unwrap();
        assert_eq!(cell(&version_rows.rows[0], 0), FACT_SCHEMA_VERSION);

        fs::remove_dir_all(target).unwrap();
    }

    #[test]
    fn full_build_writes_fact_schema_version_meta() {
        let target = unique_temp_dir("fact-schema-version");
        let schema = bundled_schema();
        let schema_copy = render_schema_copy(&schema);

        materialize_from_schema(&target, &schema, &schema_copy, false).unwrap();
        let artifact = build_index_artifact(&target, &schema).unwrap();
        write_fact_store(&target, &artifact).unwrap();

        let db = open_fact_store(&target).unwrap();
        let version_rows = db
            .query("SELECT value FROM meta WHERE key='fact_schema_version';")
            .unwrap();
        assert_eq!(cell(&version_rows.rows[0], 0), FACT_SCHEMA_VERSION);

        fs::remove_dir_all(target).unwrap();
    }

    #[test]
    fn delta_refresh_recovers_relations_when_new_target_doc_appears() {
        let target = unique_temp_dir("delta-link-recovery");
        let schema = bundled_schema();
        let schema_copy = render_schema_copy(&schema);

        materialize_from_schema(&target, &schema, &schema_copy, false).unwrap();
        fs::write(
            target.join("docs/design-docs/source.md"),
            "---\nstatus: active\nauthority: canonical\n---\n\n# Design Doc: Source\n\nLinks to `docs/design-docs/target.md`.\n",
        )
        .unwrap();

        let artifact = build_index_artifact(&target, &schema).unwrap();
        write_index_artifact(&target, &artifact).unwrap();
        write_fact_store(&target, &artifact).unwrap();

        let db = open_fact_store(&target).unwrap();
        let before = scalar_count(
            &db,
            "SELECT COUNT(*) FROM relations WHERE src_path = 'docs/design-docs/source.md' AND dst_path = 'docs/design-docs/target.md';",
        )
        .unwrap();
        assert_eq!(before, 0);

        fs::write(
            target.join("docs/design-docs/target.md"),
            "---\nstatus: active\nauthority: canonical\n---\n\n# Design Doc: Target\n",
        )
        .unwrap();

        let context = EngineContext {
            target_dir: target.clone(),
            schema: schema.clone(),
        };
        refresh_fact_store(&context).unwrap();

        let db = open_fact_store(&target).unwrap();
        let after = scalar_count(
            &db,
            "SELECT COUNT(*) FROM relations WHERE src_path = 'docs/design-docs/source.md' AND dst_path = 'docs/design-docs/target.md';",
        )
        .unwrap();
        assert_eq!(after, 1);

        fs::remove_dir_all(target).unwrap();
    }

    #[test]
    fn delta_refresh_removes_stale_reverse_scope_relations() {
        let target = unique_temp_dir("delta-scope-reverse");
        let schema = bundled_schema();
        let schema_copy = render_schema_copy(&schema);

        materialize_from_schema(&target, &schema, &schema_copy, false).unwrap();
        fs::create_dir_all(target.join("src/old")).unwrap();
        fs::create_dir_all(target.join("src/new")).unwrap();
        fs::write(target.join("src/old/lib.rs"), "pub fn old() {}\n").unwrap();
        fs::write(target.join("src/new/lib.rs"), "pub fn new() {}\n").unwrap();
        fs::write(
            target.join("docs/design-docs/scope.md"),
            "---\nstatus: active\nauthority: canonical\nscope_paths:\n  - src/old\n---\n\n# Design Doc: Scope\n\nOld scope.\n",
        )
        .unwrap();

        let artifact = build_index_artifact(&target, &schema).unwrap();
        write_index_artifact(&target, &artifact).unwrap();
        write_fact_store(&target, &artifact).unwrap();

        let db = open_fact_store(&target).unwrap();
        assert_eq!(
            scalar_count(
                &db,
                "SELECT COUNT(*) FROM relations WHERE src_path = 'src/old' AND dst_path = 'docs/design-docs/scope.md' AND relation_type = 'code_mentions_doc';"
            )
            .unwrap(),
            1
        );

        fs::write(
            target.join("docs/design-docs/scope.md"),
            "---\nstatus: active\nauthority: canonical\nscope_paths:\n  - src/new\n---\n\n# Design Doc: Scope\n\nNew scope.\n",
        )
        .unwrap();

        let context = EngineContext {
            target_dir: target.clone(),
            schema: schema.clone(),
        };
        refresh_fact_store(&context).unwrap();

        let db = open_fact_store(&target).unwrap();
        assert_eq!(
            scalar_count(
                &db,
                "SELECT COUNT(*) FROM relations WHERE src_path = 'src/old' AND dst_path = 'docs/design-docs/scope.md' AND relation_type = 'code_mentions_doc';"
            )
            .unwrap(),
            0
        );
        assert_eq!(
            scalar_count(
                &db,
                "SELECT COUNT(*) FROM relations WHERE src_path = 'src/new' AND dst_path = 'docs/design-docs/scope.md' AND relation_type = 'code_mentions_doc';"
            )
            .unwrap(),
            1
        );

        fs::remove_dir_all(target).unwrap();
    }

    #[test]
    fn phase2_checks_cover_manifest_stale_duplicate_contamination_and_frontmatter() {
        let target = unique_temp_dir("phase2-checks");
        let schema = bundled_schema();
        let schema_copy = render_schema_copy(&schema);

        materialize_from_schema(&target, &schema, &schema_copy, false).unwrap();
        let docs_index = fs::read_to_string(target.join("docs/index.md")).unwrap();
        fs::write(
            target.join("docs/index.md"),
            docs_index.replace("`docs/operations/index.md`", "`docs/operations/README.md`"),
        )
        .unwrap();
        fs::create_dir_all(target.join("src/fresh")).unwrap();
        fs::write(target.join("src/fresh/lib.rs"), "pub fn fresh() {}\n").unwrap();
        fs::write(
            target.join("docs/design-docs/stale.md"),
            "---\nstatus: active\nauthority: canonical\nunknown_field: value\nscope_paths:\n  - src/fresh\n---\n\n# Design Doc: Stale\n\nCanonical source doc links to [generated](../generated/report.md).\n",
        )
        .unwrap();
        let initial_artifact = build_index_artifact(&target, &schema).unwrap();
        let stale_doc_mtime = initial_artifact
            .docs
            .iter()
            .find(|doc| doc.path == "docs/design-docs/stale.md")
            .unwrap()
            .mtime_unix;
        fs::write(
            target.join("docs/design-docs/dup-a.md"),
            "---\nstatus: active\nauthority: canonical\n---\n\n# Duplicate Shape\n\nBody A.\n\n## Shared\n\n## Details\n",
        )
        .unwrap();
        fs::write(
            target.join("docs/design-docs/dup-b.md"),
            "---\nstatus: active\nauthority: canonical\n---\n\n# Duplicate Shape\n\nBody B.\n\n## Shared\n\n## Details\n",
        )
        .unwrap();
        fs::write(
            target.join("docs/generated/report.md"),
            "# Generated Report\n\nMachine output.\n",
        )
        .unwrap();
        for attempt in 0..5 {
            std::thread::sleep(std::time::Duration::from_millis(1100));
            fs::write(
                target.join("src/fresh/lib.rs"),
                format!("pub fn fresher_{}() {{}}\n", attempt),
            )
            .unwrap();
            if newest_existing_path_mtime(&target.join("src/fresh")).unwrap_or(0) > stale_doc_mtime
            {
                break;
            }
        }

        let artifact = build_index_artifact(&target, &schema).unwrap();
        let rule_ids = artifact
            .checks
            .iter()
            .map(|check| check.rule_id.as_str())
            .collect::<BTreeSet<_>>();

        assert!(artifact
            .relations
            .iter()
            .any(|relation| relation.relation_type == "code_mentions_doc"
                && relation.dst_path == "docs/design-docs/stale.md"));
        assert!(rule_ids.contains("manifest-missing-doc"));
        assert!(rule_ids.contains("stale-explicit-anchor"));
        assert!(rule_ids.contains("overlap-heading-fingerprint"));
        assert!(rule_ids.contains("canonical-links-generated"));
        assert!(rule_ids.contains("frontmatter-field-not-allowed"));
        assert!(artifact
            .checks
            .iter()
            .all(|check| !check.evidence_json.is_empty()));

        fs::remove_dir_all(target).unwrap();
    }

    #[test]
    fn schema_parses_suppressions_and_check_filter_applies_them() {
        let raw = r#"
schema_version: 0
managed_root: docs

entrypoints:
  agents: AGENTS.md
  manifest: docs/index.md
  project_map: docs/project.md

core_files:
  index:
    path: index.md
    required: true
    template: docs-index

doc_collections:
  design_docs:
    path: design-docs
    authority: canonical
    anchor_candidate: true
    template: design-doc
    allowed_frontmatter: [status, authority]

rules:
  index_required: true

suppressions:
  known_missing:
    rule_id: manifest-missing-doc
    path: docs/design-docs/index.md
    reason: accepted during migration
"#;
        let schema = parse_schema(raw).unwrap();
        assert_eq!(schema.suppressions.len(), 1);

        let result = QueryResult {
            rows: vec![
                vec![
                    Some("warning".to_string()),
                    Some("manifest-missing-doc".to_string()),
                    Some("docs/design-docs/index.md".to_string()),
                    Some("not linked".to_string()),
                    Some("{}".to_string()),
                ],
                vec![
                    Some("info".to_string()),
                    Some("frontmatter-field-not-allowed".to_string()),
                    Some("docs/design-docs/x.md".to_string()),
                    Some("extra key".to_string()),
                    Some("{}".to_string()),
                ],
            ],
        };
        let options = CommandOptions {
            json: false,
            strict: false,
            rules: Some(["manifest".to_string()].into_iter().collect()),
        };
        let checks = filter_and_suppress_checks(&result, &schema, &options);

        assert_eq!(checks.len(), 1);
        assert!(checks[0].suppressed);
        assert_eq!(
            checks[0].suppression_reason.as_deref(),
            Some("accepted during migration")
        );
    }

    #[test]
    fn engine_context_prefers_project_schema_after_init() {
        let target = unique_temp_dir("project-schema-default");
        let schema = bundled_schema();
        let mut schema_copy = render_schema_copy(&schema);
        schema_copy.push_str("\nsuppressions:\n  local_only:\n    rule_id: missing-required-doc\n    path: docs/commands.md\n    reason: local project config\n");

        materialize_from_schema(&target, &schema, &schema_copy, false).unwrap();
        let args = vec![target.to_string_lossy().to_string()];
        let context = load_engine_context(&args, 0).unwrap();

        assert_eq!(context.schema.suppressions.len(), 1);
        assert_eq!(
            context.schema.suppressions[0].reason,
            "local project config"
        );

        fs::remove_dir_all(target).unwrap();
    }

    #[test]
    fn query_candidates_include_snippet_heading_location_and_json() {
        let target = unique_temp_dir("query-snippet");
        let schema = bundled_schema();
        let schema_copy = render_schema_copy(&schema);

        materialize_from_schema(&target, &schema, &schema_copy, false).unwrap();
        fs::write(
            target.join("docs/design-docs/semantic-layer.md"),
            "---\nstatus: active\nauthority: canonical\n---\n\n# Design Doc: Semantic Layer\n\nOpening summary.\n\n## Query Semantics\n\nThe semantic layer supports phrase search and context packet retrieval.\n",
        )
        .unwrap();
        let artifact = build_index_artifact(&target, &schema).unwrap();
        write_fact_store(&target, &artifact).unwrap();
        let db = open_fact_store(&target).unwrap();

        let rows = query_candidates(&db, &target, "phrase search").unwrap();
        let candidates = query_rows_to_candidates(&rows, &target, "phrase search");
        let candidate = candidates
            .iter()
            .find(|candidate| candidate.path == "docs/design-docs/semantic-layer.md")
            .unwrap();

        assert_eq!(candidate.snippet_heading, "Query Semantics");
        assert!(candidate.snippet_location.starts_with("line "));
        assert!(candidate.snippet.contains("phrase search"));
        let json = render_query_json("phrase search", &candidates);
        assert!(json.contains("\"snippet_location\""));

        fs::remove_dir_all(target).unwrap();
    }

    #[test]
    fn query_candidates_handles_punctuation_only_query() {
        let target = unique_temp_dir("query-punctuation");
        let schema = bundled_schema();
        let schema_copy = render_schema_copy(&schema);

        materialize_from_schema(&target, &schema, &schema_copy, false).unwrap();
        let artifact = build_index_artifact(&target, &schema).unwrap();
        write_fact_store(&target, &artifact).unwrap();
        let db = open_fact_store(&target).unwrap();

        let rows = query_candidates(&db, &target, "--- ?! ::").unwrap();
        assert!(rows.rows.is_empty());

        fs::remove_dir_all(target).unwrap();
    }

    #[test]
    fn query_candidates_limits_before_neighbor_expansion() {
        let target = unique_temp_dir("query-neighbor-limit");
        let schema = bundled_schema();
        let schema_copy = render_schema_copy(&schema);

        materialize_from_schema(&target, &schema, &schema_copy, false).unwrap();
        for idx in 0..30 {
            fs::write(
                target
                    .join("docs/references")
                    .join(format!("sample-{:02}.md", idx)),
                format!(
                    "---\nstatus: active\nauthority: reference\n---\n\n# Sample {:02}\n\nShared sample body.\n",
                    idx
                ),
            )
            .unwrap();
        }
        let artifact = build_index_artifact(&target, &schema).unwrap();
        write_fact_store(&target, &artifact).unwrap();
        let db = open_fact_store(&target).unwrap();

        let rows = query_candidates(&db, &target, "sample").unwrap();

        assert_eq!(rows.rows.len(), 12);
        assert!(rows
            .rows
            .iter()
            .all(|row| split_pipe_list(cell(row, 7)).len() <= 8));

        fs::remove_dir_all(target).unwrap();
    }

    #[test]
    fn history_hash_is_wide_for_history_objects() {
        let hash = history_content_hash(b"history content");
        assert_eq!(hash.len(), 32);
        assert!(hash.chars().all(|ch| ch.is_ascii_hexdigit()));
    }

    #[test]
    fn parse_history_snapshot_json_accepts_compact_json_layout() {
        let raw = r#"{"id":"snap-1","created_at_unix":123,"message":"baseline","docs_root":"docs","schema_hash":"abcd","host_git_head":null,"host_git_branch":"main","host_git_dirty":false,"files":[{"path":"docs/index.md","hash":"beef","size":10,"mtime_unix":456}]}"#;

        let snapshot = parse_history_snapshot_json(raw).unwrap();

        assert_eq!(snapshot.id, "snap-1");
        assert_eq!(snapshot.host_git_branch.as_deref(), Some("main"));
        assert_eq!(snapshot.files.len(), 1);
        assert_eq!(snapshot.files[0].path, "docs/index.md");
    }

    #[test]
    fn json_parser_decodes_surrogate_pairs() {
        let value = parse_json_string_value(r#""emoji \uD83D\uDE00""#);

        assert_eq!(value, "emoji \u{1F600}");
    }

    #[test]
    fn json_string_escapes_all_control_characters() {
        let rendered = json_string("a\u{0001}\u{0008}\u{000c}z");
        assert_eq!(rendered, "\"a\\u0001\\b\\fz\"");
    }

    #[test]
    fn render_check_json_emits_evidence_as_json_value() {
        let checks = vec![CheckView {
            severity: "warning".to_string(),
            rule_id: "history-dirty-since-snapshot".to_string(),
            subject_path: ".harnesskit/history".to_string(),
            message: "dirty".to_string(),
            evidence_json: "{\"changed\":\"1\"}".to_string(),
            suppressed: false,
            suppression_reason: None,
        }];

        let rendered = render_check_json(Path::new("/tmp/demo"), 1, 2, &checks);

        assert!(rendered.contains("\"evidence_json\": {\"changed\":\"1\"}"));
    }

    #[test]
    fn context_packet_collects_graph_neighbors_and_reading_order() {
        let target = unique_temp_dir("context-packet");
        let schema = bundled_schema();
        let schema_copy = render_schema_copy(&schema);

        materialize_from_schema(&target, &schema, &schema_copy, false).unwrap();
        fs::create_dir_all(target.join("src/shared")).unwrap();
        fs::write(target.join("src/shared/lib.rs"), "pub fn shared() {}\n").unwrap();
        fs::write(
            target.join("docs/design-docs/semantic-layer.md"),
            "---\nstatus: active\nauthority: canonical\nscope_paths:\n  - src/shared\n---\n\n# Design Doc: Semantic Layer\n\nThe semantic layer anchors query context.\n\nLinks to [aggregation](aggregation-layer.md).\n",
        )
        .unwrap();
        fs::write(
            target.join("docs/design-docs/aggregation-layer.md"),
            "---\nstatus: active\nauthority: canonical\nscope_paths:\n  - src/shared\n---\n\n# Design Doc: Aggregation Layer\n\nSibling design doc.\n",
        )
        .unwrap();
        let artifact = build_index_artifact(&target, &schema).unwrap();
        write_fact_store(&target, &artifact).unwrap();
        let db = open_fact_store(&target).unwrap();

        let packet =
            context_packet_for_doc(&db, &target, "docs/design-docs/semantic-layer.md").unwrap();

        assert!(packet
            .incoming
            .iter()
            .any(|relation| relation.src_path == "docs/design-docs/index.md"));
        assert!(packet
            .outgoing
            .iter()
            .any(|relation| relation.dst_path == "docs/design-docs/aggregation-layer.md"));
        assert!(packet
            .collection_siblings
            .contains(&"docs/design-docs/aggregation-layer.md".to_string()));
        assert!(packet
            .same_scope_docs
            .contains(&"docs/design-docs/aggregation-layer.md".to_string()));
        assert_eq!(
            packet.recommended_reading_order.first().map(String::as_str),
            Some("docs/design-docs/semantic-layer.md")
        );
        assert!(render_context_packet_json("context", "semantic", &packet)
            .contains("\"recommended_reading_order\""));

        fs::remove_dir_all(target).unwrap();
    }

    #[test]
    fn init_writes_host_git_exclude_and_history_dirs() {
        let target = unique_temp_dir("git-exclude");
        fs::create_dir_all(target.join(".git/info")).unwrap();
        let schema = bundled_schema();
        let schema_copy = render_schema_copy(&schema);

        materialize_from_schema(&target, &schema, &schema_copy, false).unwrap();
        let stats = ensure_host_git_exclude(&target, &schema).unwrap().unwrap();

        assert!(stats.0 >= 5);
        let exclude = fs::read_to_string(target.join(".git/info/exclude")).unwrap();
        assert!(exclude.contains("docs/"));
        assert!(exclude.contains("AGENTS.md"));
        assert!(exclude.contains("CLAUDE.md"));
        assert!(exclude.contains("ARCHITECTURE.md"));
        assert!(exclude.contains(".harnesskit/"));
        assert!(target.join(".harnesskit/history/objects").is_dir());
        assert!(target.join(".harnesskit/history/snapshots").is_dir());
        assert!(target.join(".harnesskit/history/refs").is_dir());

        fs::remove_dir_all(target).unwrap();
    }

    #[test]
    fn init_tracked_mode_leaves_git_exclude_unchanged() {
        let target = unique_temp_dir("git-tracked");
        fs::create_dir_all(target.join(".git/info")).unwrap();
        fs::write(target.join(".git/info/exclude"), "# local excludes\n").unwrap();
        let schema = bundled_schema();
        let schema_copy = render_schema_copy(&schema);

        materialize_from_schema(&target, &schema, &schema_copy, false).unwrap();

        let exclude = fs::read_to_string(target.join(".git/info/exclude")).unwrap();
        assert_eq!(exclude, "# local excludes\n");
        assert!(target.join("AGENTS.md").exists());
        assert!(target.join("docs/index.md").exists());

        fs::remove_dir_all(target).unwrap();
    }

    #[test]
    fn init_preview_paths_do_not_write_files() {
        let parent = unique_temp_dir("init-preview-plan");
        let target = parent.join("preview-target");
        let schema = bundled_schema();
        let paths = init_plan_paths(&schema);

        assert!(paths.contains(&"AGENTS.md".to_string()));
        assert!(paths.contains(&"CLAUDE.md".to_string()));
        assert!(paths.contains(&"ARCHITECTURE.md".to_string()));
        assert!(paths.contains(&"docs/index.md".to_string()));
        assert!(paths.contains(&".harnesskit/state/".to_string()));
        assert!(paths.contains(&".harnesskit/history/".to_string()));
        assert!(!target.exists());
        fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn history_snapshot_status_diff_and_restore_roundtrip() {
        let target = unique_temp_dir("history-roundtrip");
        let schema = bundled_schema();
        let schema_copy = render_schema_copy(&schema);

        materialize_from_schema(&target, &schema, &schema_copy, false).unwrap();
        let first = create_history_snapshot(&target, &schema, "baseline").unwrap();
        let original_project = fs::read_to_string(target.join("docs/project.md")).unwrap();

        fs::write(
            target.join("docs/project.md"),
            "---\nstatus: active\nauthority: canonical\n---\n\n# Project\n\nChanged for history.\n",
        )
        .unwrap();
        fs::write(
            target.join("docs/design-docs/history-added.md"),
            "---\nstatus: active\nauthority: canonical\n---\n\n# Design Doc: Added\n",
        )
        .unwrap();

        let status = compute_history_status(&target, &schema).unwrap();
        assert_eq!(status.latest_snapshot.as_deref(), Some(first.id.as_str()));
        assert!(status.changed.contains(&"docs/project.md".to_string()));
        assert!(status
            .added
            .contains(&"docs/design-docs/history-added.md".to_string()));

        let diff = diff_snapshot_to_worktree(&target, &schema, &first).unwrap();
        assert!(diff.changed.contains(&"docs/project.md".to_string()));
        assert!(diff
            .added
            .contains(&"docs/design-docs/history-added.md".to_string()));

        let plan = restore_plan(&target, &schema, &first).unwrap();
        assert!(plan
            .remove_paths
            .contains(&"docs/design-docs/history-added.md".to_string()));
        apply_restore_plan(&target, &plan).unwrap();

        assert_eq!(
            fs::read_to_string(target.join("docs/project.md")).unwrap(),
            original_project
        );
        assert!(!target.join("docs/design-docs/history-added.md").exists());

        fs::remove_dir_all(target).unwrap();
    }

    #[test]
    fn history_restore_preflight_rejects_missing_objects_without_writing() {
        let target = unique_temp_dir("history-restore-preflight");
        let schema = bundled_schema();
        let schema_copy = render_schema_copy(&schema);

        materialize_from_schema(&target, &schema, &schema_copy, false).unwrap();
        let snapshot = create_history_snapshot(&target, &schema, "baseline").unwrap();
        let project_path = target.join("docs/project.md");
        let changed_project = "---\nstatus: active\nauthority: canonical\n---\n\n# Project\n\nChanged before failed restore.\n";
        fs::write(&project_path, changed_project).unwrap();

        let missing_object = history_object_path(&target, &snapshot.files[0].hash);
        fs::remove_file(missing_object).unwrap();
        let plan = restore_plan(&target, &schema, &snapshot).unwrap();
        let err = apply_restore_plan(&target, &plan).unwrap_err();

        assert!(err.to_string().contains("missing history object"));
        assert_eq!(fs::read_to_string(project_path).unwrap(), changed_project);

        fs::remove_dir_all(target).unwrap();
    }

    #[test]
    fn history_schema_only_change_has_specific_check() {
        let target = unique_temp_dir("history-schema-only");
        fs::create_dir_all(target.join(".git/info")).unwrap();
        let schema = bundled_schema();
        let mut schema_copy = render_schema_copy(&schema);

        materialize_from_schema(&target, &schema, &schema_copy, false).unwrap();
        ensure_host_git_exclude(&target, &schema).unwrap();
        create_history_snapshot(&target, &schema, "baseline").unwrap();
        schema_copy.push_str("\n# local schema comment\n");
        fs::write(target.join(".harnesskit/schema.yaml"), schema_copy).unwrap();

        let checks = build_index_artifact(&target, &schema).unwrap().checks;

        assert!(checks
            .iter()
            .any(|check| check.rule_id == "history-schema-not-snapshotted"));
        assert!(!checks
            .iter()
            .any(|check| check.rule_id == "history-dirty-since-snapshot"));

        fs::remove_dir_all(target).unwrap();
    }

    #[test]
    fn history_checks_and_index_meta_reflect_snapshot_state() {
        let target = unique_temp_dir("history-checks");
        fs::create_dir_all(target.join(".git/info")).unwrap();
        let schema = bundled_schema();
        let schema_copy = render_schema_copy(&schema);

        materialize_from_schema(&target, &schema, &schema_copy, false).unwrap();
        let artifact_without_snapshot = build_index_artifact(&target, &schema).unwrap();
        assert!(artifact_without_snapshot
            .checks
            .iter()
            .any(|check| check.rule_id == "history-missing"));

        ensure_host_git_exclude(&target, &schema).unwrap();
        let snapshot = create_history_snapshot(&target, &schema, "tracked").unwrap();
        fs::write(
            target.join("docs/project.md"),
            "---\nstatus: active\nauthority: canonical\n---\n\n# Project\n\nDirty after snapshot.\n",
        )
        .unwrap();

        let artifact = build_index_artifact(&target, &schema).unwrap();
        assert_eq!(
            artifact.history_latest_snapshot.as_deref(),
            Some(snapshot.id.as_str())
        );
        assert!(artifact.history_dirty);
        assert!(artifact.history_tracked_files_count > 0);
        assert!(artifact
            .checks
            .iter()
            .any(|check| check.rule_id == "history-dirty-since-snapshot"));

        write_fact_store(&target, &artifact).unwrap();
        let db = open_fact_store(&target).unwrap();
        let meta = db
            .query("SELECT value FROM meta WHERE key = 'history_latest_snapshot';")
            .unwrap();
        assert_eq!(cell(&meta.rows[0], 0), snapshot.id);

        fs::remove_dir_all(target).unwrap();
    }
}
