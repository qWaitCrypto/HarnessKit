//! W8 migration planning and safe application.
//!
//! Migration is deliberately conservative.  The plan reports semantic work
//! (lifecycle decisions and anchors) together with deterministic edits.  The
//! apply path only writes schema/frontmatter-shaped metadata, `.gitignore`,
//! and the durable knowledge lock; document prose is never rewritten.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{json, Value};

use crate::discover::{discover_docs, Coverage};
use crate::fs_utils::{normalize_path, read_text_lossy};
use crate::history::create_history_snapshot;
use crate::index::EngineContext;
use crate::lifecycle::{banner_status, path_status};
use crate::output::println;
use crate::parse::parse_doc_text;
use crate::render::{compact, cursor_for_offset, cursor_offset, token_cost, RenderOptions};
use crate::schema::Schema;
use crate::suggest::AnchorSuggestion;
use crate::Result;

mod schema_edit;

pub(crate) const CURRENT_SCHEMA_VERSION: u32 = 1;

pub(crate) const CATEGORY_SCHEMA: &str = "schema";
pub(crate) const CATEGORY_LAYOUT: &str = "layout";
pub(crate) const CATEGORY_LIFECYCLE: &str = "lifecycle";
pub(crate) const CATEGORY_ANCHORS: &str = "anchors";
pub(crate) const CATEGORY_GITIGNORE: &str = "gitignore";
pub(crate) const CATEGORY_KNOWLEDGE_LOCK: &str = "knowledge-lock";

#[derive(Clone, Debug, Serialize)]
pub(crate) struct MigrationItem {
    pub(crate) id: String,
    pub(crate) category: String,
    pub(crate) path: String,
    pub(crate) message: String,
    pub(crate) evidence: Value,
    pub(crate) action: String,
    pub(crate) fixable: bool,
}

#[derive(Clone, Default)]
pub(crate) struct MigrationAnalysis {
    pub(crate) items: Vec<MigrationItem>,
    pub(crate) schema_path: PathBuf,
    pub(crate) gitignore_path: PathBuf,
    pub(crate) needs_gitignore: bool,
    pub(crate) needs_lock: bool,
    pub(crate) layout_patterns: Vec<(String, String)>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct MigrationApplyReport {
    pub(crate) applied: Vec<String>,
    pub(crate) applied_count: usize,
    pub(crate) changed: bool,
    pub(crate) snapshot: Option<String>,
    pub(crate) snapshot_skipped: bool,
    pub(crate) snapshot_message: String,
    pub(crate) skipped: Vec<String>,
    pub(crate) remaining: Vec<MigrationItem>,
}

#[derive(Clone, Debug, Serialize)]
struct MigrationJsonPage<'a> {
    items: &'a [MigrationItem],
    total: usize,
    truncated: usize,
    next_cursor: Option<String>,
}

/// Normalize the user-facing category spelling while keeping a small set of
/// obvious aliases useful for scripts written against design terminology.
pub(crate) fn normalize_category(value: &str) -> Result<String> {
    let normalized = value.trim().to_ascii_lowercase().replace('_', "-");
    let category = match normalized.as_str() {
        "schema" | "schema-upgrade" | "schema-upgrades" => CATEGORY_SCHEMA,
        "layout" | "inferred-layout" => CATEGORY_LAYOUT,
        "lifecycle" | "superseded" => CATEGORY_LIFECYCLE,
        "anchor" | "anchors" => CATEGORY_ANCHORS,
        "gitignore" | "git-ignore" => CATEGORY_GITIGNORE,
        "knowledge-lock" | "knowledge" | "lock" => CATEGORY_KNOWLEDGE_LOCK,
        _ => return Err(format!("unknown migration category `{}`", value).into()),
    };
    Ok(category.to_string())
}

pub(crate) fn schema_has_key(raw: &str, key: &str) -> bool {
    raw.lines().any(|line| {
        !line.starts_with(' ')
            && crate::schema::strip_schema_comment(line)
                .split_once(':')
                .map(|(name, _)| name.trim() == key)
                .unwrap_or(false)
    })
}

pub(crate) fn build_analysis(
    context: &EngineContext,
    raw_schema: &str,
    schema_exists: bool,
    anchor_suggestions: &[AnchorSuggestion],
    lock_was_missing: bool,
    has_resolved_anchors: bool,
) -> Result<MigrationAnalysis> {
    let root = &context.target_dir;
    let coverage = discover_docs(root, &context.schema)?;
    let schema_path = root.join(".harnesskit/schema.yaml");
    let gitignore_path = root.join(".gitignore");
    let mut analysis = MigrationAnalysis {
        schema_path,
        gitignore_path,
        needs_gitignore: !has_state_gitignore(root)?,
        needs_lock: lock_was_missing && has_resolved_anchors,
        ..MigrationAnalysis::default()
    };

    analysis
        .items
        .extend(schema_migration(&context.schema, raw_schema, schema_exists));

    let layout = layout_migration(&context.schema, &coverage);
    analysis.layout_patterns = layout.patterns.clone();
    analysis.items.extend(layout.items);

    analysis
        .items
        .extend(lifecycle_migrations(root, &coverage)?);
    analysis.items.extend(anchor_migrations(anchor_suggestions));

    if analysis.needs_gitignore {
        analysis.items.push(MigrationItem {
            id: "gitignore-state".to_string(),
            category: CATEGORY_GITIGNORE.to_string(),
            path: ".gitignore".to_string(),
            message: "`.harnesskit/state/` is not ignored by Git.".to_string(),
            evidence: json!({
                "path": ".harnesskit/state/",
                "exists": root.join(".harnesskit/state").is_dir(),
            }),
            action: "Append `.harnesskit/state/` to .gitignore.".to_string(),
            fixable: true,
        });
    }
    if analysis.needs_lock {
        analysis
            .items
            .push(lock_item(root.join(".harnesskit/knowledge.lock").is_file()));
    }

    let order = [
        CATEGORY_SCHEMA,
        CATEGORY_LAYOUT,
        CATEGORY_LIFECYCLE,
        CATEGORY_ANCHORS,
        CATEGORY_GITIGNORE,
        CATEGORY_KNOWLEDGE_LOCK,
    ];
    analysis.items.sort_by(|left, right| {
        order
            .iter()
            .position(|category| *category == left.category)
            .unwrap_or(order.len())
            .cmp(
                &order
                    .iter()
                    .position(|category| *category == right.category)
                    .unwrap_or(order.len()),
            )
            .then_with(|| left.path.cmp(&right.path))
            .then_with(|| left.id.cmp(&right.id))
    });
    Ok(analysis)
}

/// Doctor only nudges projects with an explicit schema to upgrade. Optional
/// layout/anchor review does not make zero-config projects require migration,
/// and checking the schema never scans or parses document bodies.
pub(crate) fn has_pending_migrations(
    context: &EngineContext,
    raw_schema: &str,
    schema_exists: bool,
) -> bool {
    !schema_migration(&context.schema, raw_schema, schema_exists).is_empty()
}

fn lock_item(exists: bool) -> MigrationItem {
    MigrationItem {
        id: "knowledge-lock-init".to_string(),
        category: CATEGORY_KNOWLEDGE_LOCK.to_string(),
        path: ".harnesskit/knowledge.lock".to_string(),
        message: "The durable knowledge lock has not been initialized.".to_string(),
        evidence: json!({
            "path": ".harnesskit/knowledge.lock",
            "exists": exists,
            "observed_before_index": !exists,
        }),
        action: "Initialize knowledge.lock with the resolved explicit code anchors after taking a snapshot.".to_string(),
        fixable: true,
    }
}

fn schema_migration(schema: &Schema, raw: &str, schema_exists: bool) -> Vec<MigrationItem> {
    if !schema_exists {
        return Vec::new();
    }
    let mut items = Vec::new();
    let old_version = schema.schema_version;
    if !schema_has_key(raw, "schema_version") {
        items.push(schema_item(
            "schema-version-missing",
            "The project schema has no schema_version field.",
            json!({ "schema_version": "missing", "target": CURRENT_SCHEMA_VERSION }),
            format!(
                "Add `schema_version: {}` to .harnesskit/schema.yaml.",
                CURRENT_SCHEMA_VERSION
            ),
        ));
    } else if old_version < CURRENT_SCHEMA_VERSION {
        items.push(schema_item(
            "schema-version-upgrade",
            &format!(
                "Schema version {} is older than the current version {}.",
                old_version, CURRENT_SCHEMA_VERSION
            ),
            json!({ "schema_version": old_version, "target": CURRENT_SCHEMA_VERSION }),
            format!(
                "Update schema_version to {} after adding the compatible fields.",
                CURRENT_SCHEMA_VERSION
            ),
        ));
    }

    for collection in &schema.doc_collections {
        let missing = missing_frontmatter_fields(collection);
        if !missing.is_empty() {
            items.push(schema_item(
                &format!("schema-frontmatter-{}", collection.name),
                &format!("Collection `{}` does not allow the lifecycle fields introduced by Weave.", collection.name),
                json!({ "collection": collection.name, "path": collection.path, "missing_fields": missing }),
                format!("Add `{}` to doc_collections.{}.allowed_frontmatter.", missing.join("`, `"), collection.name),
            ));
        }
    }

    let missing_rules = [
        (
            "entrypoint_max_lines",
            schema.rules.entrypoint_max_lines.is_none(),
            120u32,
        ),
        (
            "stale_active_plan_days",
            schema.rules.stale_active_plan_days.is_none(),
            30u32,
        ),
    ];
    for (name, missing, default) in missing_rules {
        if missing {
            items.push(schema_item(
                &format!("schema-rule-{}", name),
                &format!("The schema does not declare the {} maintenance rule.", name),
                json!({ "rule": name, "default": default }),
                format!("Add `{}: {}` under rules.", name, default),
            ));
        }
    }

    for (index, suppression) in schema.suppressions.iter().enumerate() {
        if !suppression.path_glob {
            items.push(schema_item(
                &format!("schema-suppression-{}", index + 1),
                &format!(
                    "Suppression `{}` uses the legacy rule_id form.",
                    suppression.name
                ),
                json!({
                    "name": suppression.name,
                    "rule_id": suppression.rule_id,
                    "path": suppression.path,
                }),
                "Rewrite `rule_id` as the glob-aware `rule` suppression key.".to_string(),
            ));
        }
    }

    items
}

fn missing_frontmatter_fields(collection: &crate::schema::DocCollectionSpec) -> Vec<&'static str> {
    let mut fields = vec!["superseded_by", "supersedes_parts_of"];
    if collection.name.starts_with("exec_plans") || collection.name.starts_with("worklog") {
        fields.push("status");
    }
    fields.retain(|field| {
        !collection
            .allowed_frontmatter
            .iter()
            .any(|value| value == field)
    });
    fields
}

fn schema_item(id: &str, message: &str, evidence: Value, action: String) -> MigrationItem {
    MigrationItem {
        id: id.to_string(),
        category: CATEGORY_SCHEMA.to_string(),
        path: ".harnesskit/schema.yaml".to_string(),
        message: message.to_string(),
        evidence,
        action,
        fixable: true,
    }
}

struct LayoutMigration {
    patterns: Vec<(String, String)>,
    items: Vec<MigrationItem>,
}

fn layout_migration(schema: &Schema, coverage: &Coverage) -> LayoutMigration {
    let mut by_dir = BTreeMap::<String, Vec<String>>::new();
    for mapping in coverage
        .mappings
        .iter()
        .filter(|item| item.status == "unmanaged")
    {
        let dir = Path::new(&mapping.path)
            .parent()
            .map(normalize_path)
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| ".".to_string());
        by_dir.entry(dir).or_default().push(mapping.path.clone());
    }
    let mut patterns = Vec::new();
    let mut items = Vec::new();
    for (dir, mut paths) in by_dir {
        paths.sort();
        let pattern = if dir == "." {
            "*.md".to_string()
        } else {
            format!("{}/**/*.md", globset::escape(&dir))
        };
        if schema.include.iter().any(|item| item.pattern == pattern) {
            continue;
        }
        patterns.push((pattern.clone(), "notes".to_string()));
        items.push(MigrationItem {
            id: format!("layout-{}", slug(&dir)),
            category: CATEGORY_LAYOUT.to_string(),
            path: ".harnesskit/schema.yaml".to_string(),
            message: format!(
                "{} Markdown file(s) in {} have no writable collection mapping.",
                paths.len(),
                dir
            ),
            evidence: json!({
                "directory": dir,
                "paths": paths.iter().take(8).collect::<Vec<_>>(),
                "status": "unmanaged",
            }),
            action: format!("Add `{}: notes` under schema include mappings.", pattern),
            fixable: true,
        });
    }
    LayoutMigration { patterns, items }
}

fn lifecycle_migrations(root: &Path, coverage: &Coverage) -> Result<Vec<MigrationItem>> {
    let mut items = Vec::new();
    let managed = coverage.managed_paths();
    let managed_set = managed.iter().cloned().collect::<BTreeSet<_>>();
    let mut parsed = BTreeMap::new();
    for path in &managed {
        let text = read_text_lossy(&root.join(path))?;
        parsed.insert(path.clone(), parse_doc_text(path, &text));
    }
    for path in &managed {
        let Some(doc) = parsed.get(path) else {
            continue;
        };
        let explicit = doc.frontmatter.get("status").map(String::as_str);
        let clue = banner_status(&doc.summary);
        let path_clue = path_status(path);
        if (clue.is_some() || matches!(path_clue, Some("completed" | "archived")))
            && !matches!(
                explicit,
                Some("superseded" | "completed" | "archived" | "historical")
            )
        {
            let clue_text = clue.or(path_clue).unwrap_or("historical");
            items.push(MigrationItem {
                id: format!("lifecycle-{}", slug(path)),
                category: CATEGORY_LIFECYCLE.to_string(),
                path: path.clone(),
                message: format!("{} has a `{}` lifecycle clue but no confirming status.", path, clue_text),
                evidence: json!({
                    "path_status": path_clue,
                    "banner_status": clue,
                    "frontmatter_status": explicit,
                    "summary": compact(&doc.summary, 240),
                }),
                action: format!("Review {}; add or correct lifecycle frontmatter only after confirming whether it is superseded, completed, or archived.", path),
                fixable: false,
            });
        }
        for old in &doc.supersedes {
            if !managed_set.contains(old) {
                continue;
            }
            let previous = parsed.get(old);
            let marked = previous
                .and_then(|value| value.frontmatter.get("status"))
                .map(|status| status.eq_ignore_ascii_case("superseded"))
                .unwrap_or(false);
            if !marked {
                items.push(MigrationItem {
                    id: format!("lifecycle-supersedes-{}-{}", slug(path), slug(old)),
                    category: CATEGORY_LIFECYCLE.to_string(),
                    path: old.clone(),
                    message: format!("{} declares that it supersedes {}, whose status is unconfirmed.", path, old),
                    evidence: json!({ "successor": path, "superseded": old, "frontmatter_status": previous.and_then(|value| value.frontmatter.get("status")) }),
                    action: format!("Review the relationship and, if confirmed, add `status: superseded` and `superseded_by: {}` to {}.", path, old),
                    fixable: false,
                });
            }
        }
    }
    Ok(items)
}

fn anchor_migrations(suggestions: &[AnchorSuggestion]) -> Vec<MigrationItem> {
    let mut documents = BTreeMap::<&str, Vec<&AnchorSuggestion>>::new();
    for suggestion in suggestions {
        documents
            .entry(&suggestion.doc)
            .or_default()
            .push(suggestion);
    }
    documents.into_iter().map(|(doc, candidates)| {
        let preview = candidates.iter().take(3).map(|candidate| json!({
            "anchor": candidate.anchor,
            "section": candidate.section,
            "summary": candidate.reason,
            "evidence": candidate.evidence,
        })).collect::<Vec<_>>();
        MigrationItem {
            id: format!("anchors-{}", slug(doc)),
            category: CATEGORY_ANCHORS.to_string(),
            path: doc.to_string(),
            message: format!("{} code anchor candidate(s): {}.", candidates.len(), candidates.iter().take(3).map(|candidate| format!("`{}`", candidate.anchor)).collect::<Vec<_>>().join(", ")),
            evidence: json!({
                "candidate_count": candidates.len(),
                "candidates": preview,
                "omitted_candidates": candidates.len().saturating_sub(3),
            }),
            action: format!("Run `harnesskit suggest-anchors {}` to review candidates and evidence, then apply selected suggestions explicitly.", doc),
            fixable: false,
        }
    }).collect()
}

fn slug(value: &str) -> String {
    value
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect::<String>()
        .trim_matches('-')
        .chars()
        .take(60)
        .collect()
}

fn has_state_gitignore(root: &Path) -> Result<bool> {
    let Ok(text) = fs::read_to_string(root.join(".gitignore")) else {
        return Ok(false);
    };
    Ok(text.lines().map(str::trim).any(|line| {
        !line.is_empty()
            && !line.starts_with('#')
            && (line == ".harnesskit/state/"
                || line == ".harnesskit/state"
                || line == "/.harnesskit/state/"
                || line == "/.harnesskit/state"
                || line == ".harnesskit/state/**"
                || line == "/.harnesskit/state/**"
                || line == ".harnesskit/"
                || line == "/.harnesskit/"
                || line == ".harnesskit/**"
                || line == "/.harnesskit/**")
    }))
}

pub(crate) fn apply_analysis(
    context: &EngineContext,
    analysis: &MigrationAnalysis,
    only: Option<&str>,
) -> Result<MigrationApplyReport> {
    let selected = only.map(normalize_category).transpose()?;
    let selected_items = analysis
        .items
        .iter()
        .filter(|item| {
            selected
                .as_deref()
                .map(|category| category == item.category)
                .unwrap_or(true)
        })
        .collect::<Vec<_>>();
    let mut automatic = selected_items
        .iter()
        .filter(|item| item.fixable)
        .map(|item| item.category.as_str())
        .collect::<BTreeSet<_>>();
    // A schema file can be created by the layout category even when no
    // explicit schema upgrade item exists.
    if selected.as_deref() == Some(CATEGORY_LAYOUT) && !analysis.layout_patterns.is_empty() {
        automatic.insert(CATEGORY_LAYOUT);
    }
    if automatic.is_empty() {
        return Ok(MigrationApplyReport {
            applied: Vec::new(),
            applied_count: 0,
            changed: false,
            snapshot: None,
            snapshot_skipped: true,
            snapshot_message: "No safe structured changes are pending; review lifecycle and anchor items with an agent.".to_string(),
            skipped: selected_items.iter().map(|item| item.id.clone()).collect(),
            remaining: selected_items.into_iter().cloned().collect(),
        });
    }

    let snapshot = create_history_snapshot(
        &context.target_dir,
        &context.schema,
        "before migrate --apply",
        false,
    )?;
    let mut applied = Vec::new();
    let mut changed = false;

    if automatic.contains(CATEGORY_SCHEMA) || automatic.contains(CATEGORY_LAYOUT) {
        let raw = if analysis.schema_path.exists() {
            fs::read_to_string(&analysis.schema_path)?
        } else {
            // Applying an optional layout mapping opts into a project schema.
            // Start from the effective defaults only when there is no source
            // text to preserve.
            crate::schema::render_schema_copy(&context.schema)
        };
        let patterns = if automatic.contains(CATEGORY_LAYOUT) {
            analysis.layout_patterns.as_slice()
        } else {
            &[]
        };
        let rendered = schema_edit::edit_schema(
            &raw,
            &context.schema,
            automatic.contains(CATEGORY_SCHEMA) || !analysis.schema_path.exists(),
            patterns,
        )?;
        if fs::read_to_string(&analysis.schema_path).ok().as_deref() != Some(rendered.as_str()) {
            fs::create_dir_all(analysis.schema_path.parent().unwrap())?;
            fs::write(&analysis.schema_path, rendered)?;
            changed = true;
        }
        applied.extend(
            selected_items
                .iter()
                .filter(|item| item.category == CATEGORY_SCHEMA || item.category == CATEGORY_LAYOUT)
                .map(|item| item.id.clone()),
        );
    }

    if automatic.contains(CATEGORY_GITIGNORE) && analysis.needs_gitignore {
        append_gitignore(&analysis.gitignore_path)?;
        changed = true;
        applied.push("gitignore-state".to_string());
    }
    if automatic.contains(CATEGORY_KNOWLEDGE_LOCK) && analysis.needs_lock {
        let path = context.target_dir.join(".harnesskit/knowledge.lock");
        if !path.exists() {
            let db = crate::index::open_fact_store(&context.target_dir)?;
            crate::knowledge::refresh(&context.target_dir, &db)?;
            changed |= path.is_file();
        }
        applied.push("knowledge-lock-init".to_string());
    }

    let skipped = selected_items
        .iter()
        .filter(|item| !applied.contains(&item.id))
        .map(|item| item.id.clone())
        .collect::<Vec<_>>();
    let remaining = selected_items
        .iter()
        .filter(|item| !applied.contains(&item.id))
        .cloned()
        .cloned()
        .collect::<Vec<_>>();
    let mut snapshot_message = if snapshot.skipped {
        "History snapshot skipped because the pre-migration managed contents were unchanged."
            .to_string()
    } else {
        format!(
            "History snapshot {} created before migration.",
            snapshot.snapshot.id
        )
    };
    if changed {
        snapshot_message.push_str(" Review the migration diff, then run `harnesskit history snapshot -m \"reviewed migration\"` to capture the updated schema and documents.");
    }
    Ok(MigrationApplyReport {
        applied_count: applied.len(),
        applied,
        changed,
        snapshot: (!snapshot.skipped).then_some(snapshot.snapshot.id),
        snapshot_skipped: snapshot.skipped,
        snapshot_message,
        skipped,
        remaining,
    })
}

fn append_gitignore(path: &Path) -> Result<()> {
    let mut bytes = fs::read(path).unwrap_or_default();
    if !bytes.is_empty() && !bytes.ends_with(b"\n") {
        bytes.push(b'\n');
    }
    bytes.extend_from_slice(b".harnesskit/state/\n");
    fs::write(path, bytes)?;
    Ok(())
}

pub(crate) fn render_text(
    target: &Path,
    analysis: &MigrationAnalysis,
    options: &RenderOptions,
    applying: Option<&MigrationApplyReport>,
) {
    println!(
        "{}",
        if applying.is_some() {
            format!("HarnessKit migration apply: {}", target.display())
        } else {
            format!("HarnessKit migration plan: {}", target.display())
        }
    );
    if let Some(report) = applying {
        println!(
            "Applied {} safe change(s); {} remaining review item(s).",
            report.applied_count,
            report.remaining.len()
        );
        println!("{}", report.snapshot_message);
        if !report.applied.is_empty() {
            println!("Applied: {}", report.applied.join(", "));
        }
        if report.applied_count == 0 {
            println!("No safe changes needed.");
        }
    } else if analysis.items.is_empty() {
        println!("No migration items found; the project is at the current schema/layout baseline.");
    } else {
        println!(
            "{} migration item(s) need review or application.",
            analysis.items.len()
        );
    }
    let items = applying
        .map(|report| report.remaining.as_slice())
        .unwrap_or(&analysis.items);
    let range = page_range(items, options);
    for item in &items[range.clone()] {
        println!("{}", item_text(item));
    }
    if range.end < items.len() {
        println!(
            "… {} more item(s) (harnesskit migrate --page {} --cursor {})",
            items.len() - range.end,
            options.page.saturating_add(1),
            cursor_for_offset(range.end, "migrate")
        );
    }
}

fn item_text(item: &MigrationItem) -> String {
    let evidence = if item.category == CATEGORY_ANCHORS {
        item.evidence["candidates"]
            .as_array()
            .into_iter()
            .flatten()
            .enumerate()
            .map(|(index, candidate)| {
                format!(
                    "{}. {}",
                    index + 1,
                    compact(candidate["summary"].as_str().unwrap_or_default(), 130)
                )
            })
            .collect::<Vec<_>>()
            .join("; ")
    } else {
        compact(&item.evidence.to_string(), 420)
    };
    format!(
        "- [{}] {}: {}\n  Evidence: {}\n  Action: {}",
        item.category, item.path, item.message, evidence, item.action
    )
}

fn page_range(items: &[MigrationItem], options: &RenderOptions) -> std::ops::Range<usize> {
    // One item is an indivisible block. Even a very small budget returns a
    // complete first item so that its evidence and action can never be lost.
    let page_end = |start: usize| {
        let mut end = start;
        let mut used = 0usize;
        while end < items.len() {
            let cost = token_cost(&item_text(&items[end]));
            if end > start && used + cost > options.budget {
                break;
            }
            used += cost;
            end += 1;
        }
        end
    };
    let start = if let Some(offset) = options.cursor.as_deref().and_then(cursor_offset) {
        offset.min(items.len())
    } else {
        let mut start = 0;
        for _ in 1..options.page {
            start = page_end(start);
            if start == items.len() {
                break;
            }
        }
        start
    };
    start..page_end(start)
}

pub(crate) fn render_json(
    target: &Path,
    coverage: &Coverage,
    hints: &[String],
    analysis: &MigrationAnalysis,
    options: &RenderOptions,
    applying: Option<&MigrationApplyReport>,
) -> String {
    let items = applying
        .map(|report| &report.remaining)
        .unwrap_or(&analysis.items);
    let range = page_range(items, options);
    let end = range.end;
    let truncated = items.len().saturating_sub(end);
    let next = (end < items.len()).then(|| cursor_for_offset(end, "migrate"));
    let page = MigrationJsonPage {
        items: &items[range],
        total: items.len(),
        truncated,
        next_cursor: next.clone(),
    };
    let mut value = serde_json::to_value(page).expect("serialize migration page");
    if let Some(report) = applying {
        value["apply"] = serde_json::to_value(report).expect("serialize migration apply report");
    }
    value["target_dir"] = target.display().to_string().into();
    crate::render::render_covered_json_page(
        &value,
        coverage,
        hints,
        next.as_deref(),
        truncated,
        options.full,
    )
}
