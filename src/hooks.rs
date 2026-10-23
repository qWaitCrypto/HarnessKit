//! Agent edit hooks and Claude Code settings integration.
//!
//! Hooks consume the already-built fact store and remain cheap and safe to
//! call from an edit tool.

use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};

use crate::code::{regions_for_text, CodeRegion};
use crate::fs_utils::normalize_path;
use crate::index::cell;
use crate::knowledge::{read_anchor_lock, LockEntry};
use crate::output::println;
use crate::render::compact;
use crate::sqlite::SqliteConnection;
use crate::Result;

const HOOK_SESSION_TTL: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Clone)]
struct HookContext {
    root: PathBuf,
    path: String,
    session: Option<String>,
    output: HookOutput,
    hook_event_name: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum HookOutput {
    Plain,
    ClaudeJson,
}

pub(crate) fn run_hook_pre_edit(path_arg: Option<&str>) -> Result<()> {
    let Some(context) = hook_context(path_arg)? else { return Ok(()); };
    let db_path = context.root.join(".harnesskit/state/facts.sqlite");
    if !db_path.exists() {
        return Ok(());
    }
    let db = SqliteConnection::open(&db_path)?;
    let state_dir = context.root.join(".harnesskit/state/hooks");
    cleanup_hook_state(&state_dir);
    let marker = context
        .session
        .as_deref()
        .map(|session| state_dir.join(state_key(session, &context.path)));
    if marker.as_ref().map(|marker| marker.exists()).unwrap_or(false) {
        return Ok(());
    }
    let constraints = hook_constraints(&db, &context.path)?;
    if constraints.is_empty() {
        return Ok(());
    }
    let constraints = constraints.into_iter().take(3).collect::<Vec<_>>();
    emit_hook_output(&context, "PreToolUse", &constraints)?;
    if let Some(marker) = marker {
        if fs::create_dir_all(&state_dir).is_ok() {
            let _ = fs::write(marker, format!("{}\n", now_seconds()));
        }
    }
    Ok(())
}

pub(crate) fn run_hook_post_edit(path_arg: Option<&str>) -> Result<()> {
    let Some(context) = hook_context(path_arg)? else { return Ok(()); };
    let lock = read_anchor_lock(&context.root)?;
    if lock.is_empty() {
        return Ok(());
    }
    let source = context.root.join(&context.path);
    let text = fs::read_to_string(&source).ok();
    let Some(text) = text else { return Ok(()); };
    let regions = regions_for_text(&context.path, &text)?;
    let mut messages = Vec::new();
    for entry in lock.iter().filter(|entry| entry_affects_path(entry, &context.path)) {
        if anchor_drifted(entry, &regions) {
            messages.push(limit_hook_line(&format!(
                "Anchor drift: code changed since review; update the document or confirm unchanged requirements with `harnesskit check --ack {}`.",
                entry.doc
            )));
            break;
        }
    }
    emit_hook_output(&context, "PostToolUse", &messages)?;
    Ok(())
}

fn hook_constraints(db: &SqliteConnection, path: &str) -> Result<Vec<String>> {
    let rows = db.query(
        "SELECT e.src, e.kind, e.weight, e.evidence_json, n.path, n.name,
                COALESCE(d.role, ''), COALESCE(d.status, ''),
                COALESCE(d.summary, ''), COALESCE(section.body, ''), n.signature
         FROM edges e JOIN nodes n ON n.id = e.dst
         LEFT JOIN docs d ON d.path = json_extract(e.evidence_json, '$.doc')
         LEFT JOIN doc_sections section ON section.node_id = e.src
         WHERE e.kind IN ('anchor', 'mention', 'cochange')
           AND n.path = ?1
         ORDER BY e.kind, e.weight DESC, e.src, n.path, n.name;",
        [path],
    )?;
    let mut candidates = Vec::<(u8, String, String)>::new();
    for row in &rows.rows {
        let evidence: Value = serde_json::from_str(cell(row, 3)).unwrap_or_default();
        let Some(doc) = evidence["doc"].as_str() else { continue };
        let role = cell(row, 6);
        let status = cell(row, 7);
        let kind = cell(row, 1);
        let constraint = role == "decisions" || role == "decision" || kind == "anchor";
        let priority = if constraint {
            0
        } else if kind == "mention" {
            1
        } else if status == "active" || role.starts_with("exec_plans") || role.starts_with("worklog") {
            2
        } else {
            3
        };
        let section = evidence["section"].as_str().unwrap_or_default();
        let snippet = crate::render::local_anchor_names(&hook_kwic(cell(row, 9), cell(row, 8)), path);
        let snippet = if cell(row, 10).is_empty() { snippet } else { format!("{}; {}", snippet, cell(row, 10)) };
        let prefix = if constraint { "Constraint:" } else { "Related ~:" };
        let line = if section.is_empty() {
            format!("{prefix} {doc}: {snippet}")
        } else {
            format!("{prefix} {doc} §{section}: {snippet}")
        };
        candidates.push((priority, doc.to_string(), limit_hook_line(&line)));
    }
    candidates.sort_by(|left, right| (&left.0, &left.1, &left.2).cmp(&(&right.0, &right.1, &right.2)));
    candidates.dedup_by(|left, right| left.1 == right.1);
    Ok(candidates.into_iter().map(|(_, _, line)| line).collect())
}

fn hook_kwic(body: &str, fallback: &str) -> String {
    let text = if body.trim().is_empty() { fallback } else { body };
    compact(
        text.split(|ch| matches!(ch, '!' | '?' | '。' | '！' | '？' | '\n'))
            .map(str::trim)
            .find(|part| !part.is_empty() && !part.starts_with('#'))
            .unwrap_or(text),
        180,
    )
}

fn limit_hook_line(line: &str) -> String {
    const MAX_CHARS: usize = 160;
    let compacted = line.split_whitespace().collect::<Vec<_>>().join(" ");
    if compacted.chars().count() <= MAX_CHARS {
        return compacted;
    }
    let mut result = compacted
        .chars()
        .take(MAX_CHARS.saturating_sub(1))
        .collect::<String>();
    result.push('…');
    result
}

fn entry_affects_path(entry: &LockEntry, path: &str) -> bool {
    let anchor_path = entry.anchor.split_once('#').map(|(path, _)| path).unwrap_or(&entry.anchor);
    anchor_path == path
        || (anchor_path.ends_with('/') && path.starts_with(anchor_path))
        || (entry.anchor.split_once('#').is_none() && entry.anchor.contains("::"))
}

fn anchor_drifted(entry: &LockEntry, regions: &[CodeRegion]) -> bool {
    if !entry.anchor.contains('/') && !entry.anchor.contains('#') && entry.anchor.contains("::") {
        return regions
            .iter()
            .find(|region| {
                region.kind == "symbol"
                    && (region.name == entry.anchor
                        || region.name.ends_with(&format!("::{}", entry.anchor)))
            })
            .map(|region| region.fingerprint.as_str() != entry.fingerprint)
            .unwrap_or(true);
    }
    let (path, name) = entry.anchor.split_once('#').unwrap_or((&entry.anchor, ""));
    let target = regions.iter().find(|region| {
        region.path == path
            && if name.is_empty() {
                region.kind == "file"
            } else {
                region.kind == "symbol" && (region.name == name || region.name.ends_with(&format!("::{name}")))
            }
    });
    target.map(|region| region.fingerprint.as_str() != entry.fingerprint).unwrap_or(true)
}

fn hook_context(path_arg: Option<&str>) -> Result<Option<HookContext>> {
    let (json_input, output) = if path_arg.is_some() {
        (None, HookOutput::Plain)
    } else {
        let mut input = String::new();
        io::stdin().read_to_string(&mut input)?;
        let json_input = serde_json::from_str::<Value>(&input).ok();
        let output = json_input
            .as_ref()
            .and_then(|value| value.get("hook_event_name"))
            .and_then(Value::as_str)
            .map(|_| HookOutput::ClaudeJson)
            .unwrap_or(HookOutput::Plain);
        (json_input, output)
    };
    let path = path_arg
        .map(str::to_string)
        .or_else(|| json_input.as_ref().and_then(|value| {
            value
                .get("tool_input")
                .and_then(|input| input.get("file_path"))
                .and_then(Value::as_str)
                .map(str::to_string)
        }))
        .or_else(|| json_input.as_ref().and_then(|value| value.get("file_path")).and_then(Value::as_str).map(str::to_string));
    let Some(path) = path else { return Ok(None); };
    let session = json_input
        .as_ref()
        .and_then(|value| value.get("session_id"))
        .and_then(Value::as_str)
        .or_else(|| {
            json_input
                .as_ref()
                .and_then(|value| value.get("tool_input"))
                .and_then(|value| value.get("session_id"))
                .and_then(Value::as_str)
        })
        .map(str::to_string);
    let raw = PathBuf::from(&path);
    let start = if raw.is_absolute() {
        if raw.is_dir() { raw.clone() } else { raw.parent().unwrap_or(Path::new("/")).to_path_buf() }
    } else {
        std::env::current_dir()?
    };
    let root = find_project_root(&start).unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let relative = if raw.is_absolute() {
        raw.strip_prefix(&root).ok().map(normalize_path)
    } else {
        Some(normalize_path(Path::new(&path)))
    };
    let Some(relative) = relative else { return Ok(None); };
    let hook_event_name = (output == HookOutput::ClaudeJson)
        .then(|| {
            json_input
                .as_ref()
                .and_then(|value| value.get("hook_event_name"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .flatten();
    Ok(Some(HookContext {
        root,
        path: relative,
        session,
        output,
        hook_event_name,
    }))
}

fn emit_hook_output(context: &HookContext, default_event: &str, lines: &[String]) -> Result<()> {
    if lines.is_empty() {
        return Ok(());
    }
    match context.output {
        HookOutput::Plain => {
            for line in lines {
                println!("{}", line);
            }
        }
        HookOutput::ClaudeJson => {
            let hook_event_name = context
                .hook_event_name
                .as_deref()
                .filter(|event| matches!(*event, "PreToolUse" | "PostToolUse"))
                .unwrap_or(default_event);
            println!(
                "{}",
                json!({
                    "hookSpecificOutput": {
                        "hookEventName": hook_event_name,
                        "additionalContext": lines.join("\n"),
                    }
                })
            );
        }
    }
    Ok(())
}

fn find_project_root(start: &Path) -> Option<PathBuf> {
    start.ancestors().find_map(|ancestor| {
        (ancestor.join(".harnesskit/state/facts.sqlite").is_file()
            || ancestor.join(".harnesskit/schema.yaml").is_file())
            .then(|| ancestor.to_path_buf())
    })
}

fn state_key(session: &str, path: &str) -> String {
    blake3::hash(format!("{}\0{}", session, path).as_bytes())
        .to_hex()
        .to_string()
}

fn cleanup_hook_state(state_dir: &Path) {
    let Ok(entries) = fs::read_dir(state_dir) else { return; };
    let cutoff = SystemTime::now().checked_sub(HOOK_SESSION_TTL);
    for entry in entries.flatten() {
        let remove = cutoff
            .zip(entry.metadata().ok().and_then(|metadata| metadata.modified().ok()))
            .map(|(cutoff, modified)| modified < cutoff)
            .unwrap_or(false);
        if remove {
            let _ = fs::remove_file(entry.path());
        }
    }
}

fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn hook_settings_fragment() -> Value {
    json!({
        "hooks": {
            "PreToolUse": [{
                "matcher": "Edit|Write|MultiEdit",
                "hooks": [{"type": "command", "command": "harnesskit hook pre-edit"}]
            }],
            "PostToolUse": [{
                "matcher": "Edit|Write|MultiEdit",
                "hooks": [{"type": "command", "command": "harnesskit hook post-edit"}]
            }]
        }
    })
}

pub(crate) fn install_hooks(target: &Path, write: bool) -> Result<()> {
    let fragment = hook_settings_fragment();
    println!("Claude Code settings fragment:");
    println!("{}", serde_json::to_string_pretty(&fragment)?);
    println!("Other agents: call `harnesskit hook pre-edit <path>` / `post-edit <path>` around Edit/Write, pass hook JSON via stdin, or use `context --for <path>`. Hooks stay silent when no association is found.\n");
    if !write {
        println!("Preview only; pass --write to update .claude/settings.json after a backup.");
        return Ok(());
    }
    let settings_dir = target.join(".claude");
    let settings_path = settings_dir.join("settings.json");
    let backup_path = settings_dir.join("settings.json.bak");
    fs::create_dir_all(&settings_dir)?;
    let mut settings = if settings_path.exists() {
        fs::copy(&settings_path, &backup_path)?;
        serde_json::from_str::<Value>(&fs::read_to_string(&settings_path)?)?
    } else {
        Value::Object(serde_json::Map::new())
    };
    merge_hook_settings(&mut settings, &fragment)?;
    fs::write(&settings_path, serde_json::to_string_pretty(&settings)? + "\n")?;
    println!("Wrote {} (backup: {}).", settings_path.display(), backup_path.display());
    Ok(())
}

fn merge_hook_settings(settings: &mut Value, fragment: &Value) -> Result<()> {
    let object = settings
        .as_object_mut()
        .ok_or("Claude Code settings must be a JSON object")?;
    let hooks = object
        .entry("hooks")
        .or_insert_with(|| Value::Object(serde_json::Map::new()))
        .as_object_mut()
        .ok_or("Claude Code settings hooks must be a JSON object")?;
    for event in ["PreToolUse", "PostToolUse"] {
        let addition = fragment["hooks"][event]
            .as_array()
            .and_then(|items| items.first())
            .cloned()
            .unwrap_or(Value::Null);
        let list = hooks.entry(event).or_insert_with(|| Value::Array(Vec::new()));
        let values = list.as_array_mut().ok_or("Claude Code hook list must be an array")?;
        if !values.iter().any(|value| value == &addition) {
            values.push(addition);
        }
    }
    Ok(())
}

