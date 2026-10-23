use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::{env, fs};

use crate::fs_utils::{absolute_display_path, normalize_path};
use crate::Result;

pub(crate) const BUNDLED_SCHEMA_PATH: &str = "schemas/file-first-v0.yaml";

const BUNDLED_SCHEMA_LABEL: &str = "bundled:schemas/file-first-v0.yaml";

pub(crate) const BUNDLED_SCHEMA_TEXT: &str = include_str!("../schemas/file-first-v0.yaml");

#[derive(Default, Clone)]
pub(crate) struct Schema {
    pub(crate) schema_version: u32,
    pub(crate) managed_root: String,
    pub(crate) entrypoints: EntryPoints,
    pub(crate) core_files: Vec<CoreFileSpec>,
    pub(crate) doc_collections: Vec<DocCollectionSpec>,
    pub(crate) rules: Rules,
    pub(crate) suppressions: Vec<SuppressionSpec>,
    pub(crate) include: Vec<IncludeSpec>,
    pub(crate) ignore: Vec<String>,
    /// None uses the built-in noise filter; an explicit list replaces it.
    pub(crate) cochange_ignore: Option<Vec<String>>,
    // Runtime-only explanation of zero-config root inference, never a schema key.
    pub(crate) inferred_root: Option<String>,
    // Default schema mappings are suggestions until a project opts into a schema.
    pub(crate) inferred_layout: bool,
}

#[derive(Clone)]
pub(crate) struct IncludeSpec {
    pub(crate) pattern: String,
    pub(crate) collection: String,
}

#[derive(Default, Clone)]
pub(crate) struct EntryPoints {
    pub(crate) agents: Option<String>,
    pub(crate) claude: Option<String>,
    pub(crate) architecture: Option<String>,
    pub(crate) manifest: Option<String>,
    pub(crate) project_map: Option<String>,
}

#[derive(Default, Clone)]
pub(crate) struct CoreFileSpec {
    pub(crate) name: String,
    pub(crate) path: String,
    pub(crate) authority: Option<String>,
    pub(crate) status: Option<String>,
    pub(crate) required: bool,
    pub(crate) template: String,
    pub(crate) root_path: Option<String>,
}

#[derive(Default, Clone)]
pub(crate) struct DocCollectionSpec {
    pub(crate) name: String,
    pub(crate) path: String,
    pub(crate) authority: Option<String>,
    pub(crate) status: Option<String>,
    pub(crate) anchor_candidate: bool,
    pub(crate) template: String,
    pub(crate) allowed_frontmatter: Vec<String>,
}

#[derive(Default, Clone)]
pub(crate) struct Rules {
    pub(crate) path_defines_doc_type: Option<bool>,
    pub(crate) h1_is_title: Option<bool>,
    pub(crate) first_paragraph_is_summary: Option<bool>,
    pub(crate) index_required: Option<bool>,
    pub(crate) prefer_directory_defaults: Option<bool>,
    pub(crate) minimal_frontmatter_only: Option<bool>,
    pub(crate) stale_explicit_anchor: Option<bool>,
    pub(crate) duplicate_detection: Option<bool>,
    pub(crate) contamination_checks: Option<bool>,
    pub(crate) strict_checks: Option<bool>,
    pub(crate) entrypoint_max_lines: Option<u32>,
    pub(crate) stale_active_plan_days: Option<u32>,
    /// Maximum number of files in a commit considered for inferred cochange
    /// edges.  Large formatting/vendor commits are ignored by default.
    pub(crate) cochange_max_files: Option<u32>,
    /// Minimum same-commit history count before an undocumented hotspot is reported.
    pub(crate) undocumented_hotspot_min_commits: Option<u32>,
    /// Maximum number of undocumented hotspots shown by check.
    pub(crate) undocumented_hotspot_max: Option<u32>,
}

#[derive(Default, Clone)]
pub(crate) struct SuppressionSpec {
    pub(crate) name: String,
    pub(crate) rule_id: String,
    pub(crate) path_glob: bool,
    pub(crate) path: String,
    pub(crate) reason: String,
}

pub(crate) struct SchemaLoad {
    pub(crate) text: String,
    pub(crate) source: String,
}

#[derive(Debug)]
pub(crate) struct SchemaParseError {
    pub(crate) path: String,
    pub(crate) message: String,
}

impl std::fmt::Display for SchemaParseError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.path, self.message)
    }
}

impl std::error::Error for SchemaParseError {}

#[derive(Copy, Clone, PartialEq, Eq)]
enum Section {
    None,
    EntryPoints,
    CoreFiles,
    DocCollections,
    Rules,
    Suppressions,
    Include,
    Ignore,
    CochangeIgnore,
}

pub(crate) fn managed_memory_paths(schema: &Schema) -> Vec<String> {
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

pub(crate) fn push_unique_string(values: &mut Vec<String>, value: String) {
    if !value.is_empty() && !values.contains(&value) {
        values.push(value);
    }
}

pub(crate) fn default_context_schema_arg(target_dir: &Path) -> String {
    let project_schema = target_dir.join(".harnesskit").join("schema.yaml");
    if project_schema.exists() {
        absolute_display_path(&project_schema)
    } else {
        BUNDLED_SCHEMA_PATH.to_string()
    }
}

pub(crate) fn load_schema(target_dir: &Path, schema_path: &str) -> Result<SchemaLoad> {
    match resolve_schema_path(target_dir, schema_path)? {
        Some(path) => Ok(SchemaLoad {
            text: fs::read_to_string(&path)?,
            source: absolute_display_path(&path),
        }),
        None if schema_path == BUNDLED_SCHEMA_PATH => Ok(SchemaLoad {
            text: BUNDLED_SCHEMA_TEXT.to_string(),
            source: BUNDLED_SCHEMA_LABEL.to_string(),
        }),
        None => Err(format!("schema not found: {}", schema_path).into()),
    }
}

fn resolve_schema_path(target_dir: &Path, schema_path: &str) -> Result<Option<PathBuf>> {
    let requested = PathBuf::from(schema_path);
    let candidates = if requested.is_absolute() {
        vec![requested]
    } else {
        vec![
            env::current_dir()?.join(schema_path),
            target_dir.join(schema_path),
        ]
    };

    for candidate in candidates {
        if candidate.exists() {
            return Ok(Some(candidate));
        }
    }

    Ok(None)
}

pub(crate) fn parse_schema(text: &str) -> Result<Schema> {
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
        let line_without_comment = strip_schema_comment(raw_line).trim_end();
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
                        "include" => Section::Include,
                        "ignore" => Section::Ignore,
                        "cochange_ignore" => {
                            schema.cochange_ignore = Some(Vec::new());
                            Section::CochangeIgnore
                        }
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
                        "ignore" => schema.ignore = parse_inline_list(&value),
                        "cochange_ignore" => {
                            schema.cochange_ignore = Some(parse_inline_list(&value));
                        }
                        "cochange_max_files" | "max_cochange_files" | "max_commit_files"
                        | "undocumented_hotspot_min_commits"
                        | "hotspot_min_commits"
                        | "undocumented_hotspot_threshold"
                        | "hotspot_threshold"
                        | "undocumented_hotspot_max"
                        | "hotspot_max" => {
                            let parsed = parse_u32(&value)?;
                            if parsed == 0 {
                                return Err(
                                    format!("invalid schema: {} must be positive", key).into()
                                );
                            }
                            match key.as_str() {
                                "cochange_max_files" | "max_cochange_files" | "max_commit_files" => {
                                    schema.rules.cochange_max_files = Some(parsed)
                                }
                                "undocumented_hotspot_max" | "hotspot_max" => {
                                    schema.rules.undocumented_hotspot_max = Some(parsed)
                                }
                                _ => schema.rules.undocumented_hotspot_min_commits = Some(parsed),
                            }
                        }
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
                Section::Include => {
                    let (pattern, collection) = parse_key_value(trimmed)
                        .ok_or("invalid schema: include expects glob: collection")?;
                    schema.include.push(IncludeSpec {
                        pattern: unquote(&pattern)?,
                        collection: unquote(&collection)?,
                    });
                }
                Section::Ignore => {
                    let pattern = trimmed
                        .strip_prefix("- ")
                        .ok_or("invalid schema: ignore expects a list of globs")?;
                    schema.ignore.push(unquote(pattern)?);
                }
                Section::CochangeIgnore => {
                    let pattern = trimmed
                        .strip_prefix("- ")
                        .ok_or("invalid schema: cochange_ignore expects a list of globs")?;
                    schema
                        .cochange_ignore
                        .get_or_insert_with(Vec::new)
                        .push(unquote(pattern)?);
                }
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
                    let mut spec = SuppressionSpec::default();
                    if let Some(item) = trimmed.strip_prefix("- ") {
                        let (key, value) = parse_key_value(item)
                            .ok_or("invalid schema: suppression expects rule: <rule-id>")?;
                        if !matches!(key.as_str(), "rule" | "rule_id") {
                            return Err(
                                "invalid schema: suppression expects rule or rule_id".into()
                            );
                        }
                        spec.name = format!("suppression_{}", schema.suppressions.len() + 1);
                        spec.rule_id = unquote(&value)?;
                        spec.path_glob = key == "rule";
                    } else {
                        spec.name = parse_item_header(trimmed, "suppression")?;
                    }
                    current_suppression = Some(spec);
                }
                Section::Rules => {
                    if let Some((key, value)) = parse_key_value(trimmed) {
                        if matches!(
                            key.as_str(),
                            "entrypoint_max_lines"
                                | "stale_active_plan_days"
                                | "cochange_max_files"
                                | "max_cochange_files"
                                | "max_commit_files"
                                | "undocumented_hotspot_min_commits"
                                | "hotspot_min_commits"
                                | "undocumented_hotspot_threshold"
                                | "hotspot_threshold"
                                | "undocumented_hotspot_max"
                                | "hotspot_max"
                        ) {
                            let parsed = parse_u32(&value)?;
                            if parsed == 0 {
                                return Err(
                                    format!("invalid schema: {} must be positive", key).into()
                                );
                            }
                            match key.as_str() {
                                "entrypoint_max_lines" => {
                                    schema.rules.entrypoint_max_lines = Some(parsed)
                                }
                                "stale_active_plan_days" => {
                                    schema.rules.stale_active_plan_days = Some(parsed)
                                }
                                "cochange_max_files" | "max_cochange_files"
                                | "max_commit_files" => {
                                    schema.rules.cochange_max_files = Some(parsed)
                                }
                                "undocumented_hotspot_max" | "hotspot_max" => {
                                    schema.rules.undocumented_hotspot_max = Some(parsed)
                                }
                                _ => schema.rules.undocumented_hotspot_min_commits = Some(parsed),
                            }
                            continue;
                        }
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
                            "rule" | "rule_id" => {
                                spec.rule_id = unquote(&value)?;
                                spec.path_glob = key == "rule";
                            }
                            "path" => spec.path = unquote(&value)?,
                            "reason" => spec.reason = unquote(&value)?,
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
                Section::Rules
                | Section::EntryPoints
                | Section::None
                | Section::Include
                | Section::Ignore
                | Section::CochangeIgnore => {
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

fn unquoted_delimiter(line: &str, delimiter: char) -> Option<usize> {
    let mut quote = None;
    let mut escaped = false;
    for (index, ch) in line.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' && quote == Some('"') {
            escaped = true;
            continue;
        }
        if quote == Some(ch) {
            quote = None;
        } else if quote.is_none() && matches!(ch, '\'' | '"') {
            quote = Some(ch);
        } else if quote.is_none() && ch == delimiter {
            return Some(index);
        }
    }
    None
}

fn strip_schema_comment(line: &str) -> &str {
    unquoted_delimiter(line, '#')
        .map(|index| &line[..index])
        .unwrap_or(line)
}

fn parse_key_value(line: &str) -> Option<(String, String)> {
    let index = unquoted_delimiter(line, ':')?;
    Some((
        line[..index].trim().to_string(),
        line[index + 1..].trim().to_string(),
    ))
}

fn unquote(value: &str) -> Result<String> {
    if value.starts_with('"') {
        Ok(serde_json::from_str(value)?)
    } else {
        Ok(value.trim_matches('\'').replace("''", "'"))
    }
}

pub(crate) fn glob_matcher(pattern: &str) -> Result<globset::GlobMatcher> {
    Ok(globset::GlobBuilder::new(pattern)
        .literal_separator(true)
        .build()?
        .compile_matcher())
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

pub(crate) fn parse_inline_list(value: &str) -> Vec<String> {
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

pub(crate) fn rewrite_docs_root_bound_paths(schema: &mut Schema, old_root: &str, new_root: &str) {
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
        normalize_path(&Path::new(new_root).join(suffix))
    } else if path == old_root {
        new_root.to_string()
    } else {
        path
    }
}

fn validate_schema(schema: &Schema) -> Result<()> {
    for pattern in schema
        .ignore
        .iter()
        .chain(schema.include.iter().map(|item| &item.pattern))
        .chain(schema.cochange_ignore.iter().flatten())
    {
        glob_matcher(pattern)
            .map_err(|error| format!("invalid schema glob {:?}: {}", pattern, error))?;
    }
    for item in &schema.include {
        if !schema
            .doc_collections
            .iter()
            .any(|spec| spec.name == item.collection)
            && !matches!(
                item.collection.as_str(),
                "notes"
                    | "references"
                    | "decisions"
                    | "exec_plans"
                    | "design_docs"
                    | "product_specs"
                    | "operations"
            )
        {
            return Err(format!(
                "invalid schema: unknown include collection {:?}",
                item.collection
            )
            .into());
        }
    }
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
        if spec.path_glob {
            glob_matcher(&spec.path).map_err(|error| {
                format!(
                    "invalid schema: suppression {} has invalid path glob: {}",
                    spec.name, error
                )
            })?;
        }
    }

    Ok(())
}

pub(crate) fn render_schema_copy(schema: &Schema) -> String {
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
    if let Some(value) = schema.rules.entrypoint_max_lines {
        lines.push(format!("  entrypoint_max_lines: {}", value));
    }
    if let Some(value) = schema.rules.stale_active_plan_days {
        lines.push(format!("  stale_active_plan_days: {}", value));
    }
    if let Some(value) = schema.rules.cochange_max_files {
        lines.push(format!("  cochange_max_files: {}", value));
    }
    if let Some(value) = schema.rules.undocumented_hotspot_min_commits {
        lines.push(format!("  undocumented_hotspot_min_commits: {}", value));
    }
    if let Some(value) = schema.rules.undocumented_hotspot_max {
        lines.push(format!("  undocumented_hotspot_max: {}", value));
    }

    if !schema.suppressions.is_empty() {
        lines.push(String::new());
        lines.push("suppressions:".to_string());
        for spec in &schema.suppressions {
            lines.push(format!("  {}:", spec.name));
            lines.push(format!(
                "    {}: {}",
                if spec.path_glob { "rule" } else { "rule_id" },
                spec.rule_id
            ));
            lines.push(format!(
                "    path: {}",
                serde_json::to_string(&spec.path).unwrap()
            ));
            lines.push(format!(
                "    reason: {}",
                serde_json::to_string(&spec.reason).unwrap()
            ));
        }
    }

    if !schema.include.is_empty() {
        lines.push(String::new());
        lines.push("include:".to_string());
        for item in &schema.include {
            lines.push(format!(
                "  {}: {}",
                serde_json::to_string(&item.pattern).unwrap(),
                item.collection
            ));
        }
    }
    if !schema.ignore.is_empty() {
        lines.push(String::new());
        lines.push("ignore:".to_string());
        for pattern in &schema.ignore {
            lines.push(format!("  - {}", serde_json::to_string(pattern).unwrap()));
        }
    }
    if let Some(patterns) = &schema.cochange_ignore {
        lines.push(String::new());
        if patterns.is_empty() {
            lines.push("cochange_ignore: []".to_string());
        } else {
            lines.push("cochange_ignore:".to_string());
            for pattern in patterns {
                lines.push(format!("  - {}", serde_json::to_string(pattern).unwrap()));
            }
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

pub(crate) fn core_file_rel_path(schema: &Schema, spec: &CoreFileSpec) -> String {
    spec.root_path
        .clone()
        .unwrap_or_else(|| normalize_path(&Path::new(&schema.managed_root).join(&spec.path)))
}

pub(crate) fn configured_architecture_path(schema: &Schema) -> Option<String> {
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

pub(crate) fn resolve_architecture_path(schema: &Schema, fallback: &str) -> String {
    configured_architecture_path(schema).unwrap_or_else(|| fallback.to_string())
}
