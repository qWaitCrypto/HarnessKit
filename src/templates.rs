use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::{env, fs};

use crate::schema::{
    push_unique_string, resolve_architecture_path, DocCollectionSpec, Schema, BUNDLED_SCHEMA_PATH,
    BUNDLED_SCHEMA_TEXT,
};
use crate::Result;

#[derive(Default)]
pub(crate) struct InitStats {
    pub(crate) created_files: usize,
    pub(crate) skipped_files: usize,
}

enum WriteOutcome {
    Created,
    Skipped,
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

pub(crate) fn init_plan_paths(schema: &Schema) -> Vec<String> {
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

pub(crate) fn validate_init_templates(schema: &Schema) -> Result<()> {
    if schema.entrypoints.agents.is_some() {
        ensure_bundled_text("templates/entrypoints/AGENTS.md.tpl")?;
    }
    if schema.entrypoints.claude.is_some() {
        ensure_bundled_text("templates/entrypoints/CLAUDE.md.tpl")?;
    }

    for spec in &schema.core_files {
        let template_path = core_template_path(&spec.template)
            .ok_or_else(|| format!("unknown core template: {} ({})", spec.template, spec.name))?;
        ensure_bundled_text(template_path)?;
    }

    ensure_bundled_text("templates/core/collection-index.md.tpl")?;

    let mut seen_templates = BTreeSet::new();
    for spec in &schema.doc_collections {
        if seen_templates.insert(spec.template.clone()) {
            let template_path = doc_template_path(&spec.template).ok_or_else(|| {
                format!("unknown doc template: {} ({})", spec.template, spec.name)
            })?;
            ensure_bundled_text(template_path)?;
        }
    }

    Ok(())
}

fn ensure_bundled_text(rel_path: &str) -> Result<()> {
    bundled_text(rel_path)
        .map(|_| ())
        .ok_or_else(|| format!("bundled template not found: {}", rel_path).into())
}

pub(crate) fn materialize_from_schema(
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
            "| `{}/DOCUMENTATION_SYSTEM.md` | Documentation rules and update policy | Stable |",
            schema.managed_root
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

fn load_bundled_text(rel_path: &str) -> Result<String> {
    if let Some(text) = bundled_text(rel_path) {
        return Ok(text.to_string());
    }

    let abs = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel_path);
    if let Ok(text) = fs::read_to_string(&abs) {
        return Ok(text);
    }
    Err(format!("bundled template not found: {}", rel_path).into())
}

fn bundled_text(rel_path: &str) -> Option<&'static str> {
    match rel_path {
        BUNDLED_SCHEMA_PATH => Some(BUNDLED_SCHEMA_TEXT),
        "templates/core/ARCHITECTURE.md.tpl" => {
            Some(include_str!("../templates/core/ARCHITECTURE.md.tpl"))
        }
        "templates/core/DOCUMENTATION_SYSTEM.md.tpl" => Some(include_str!(
            "../templates/core/DOCUMENTATION_SYSTEM.md.tpl"
        )),
        "templates/core/collection-index.md.tpl" => {
            Some(include_str!("../templates/core/collection-index.md.tpl"))
        }
        "templates/core/commands.md.tpl" => Some(include_str!("../templates/core/commands.md.tpl")),
        "templates/core/index.md.tpl" => Some(include_str!("../templates/core/index.md.tpl")),
        "templates/core/project.md.tpl" => Some(include_str!("../templates/core/project.md.tpl")),
        "templates/docs/decision.md.tpl" => Some(include_str!("../templates/docs/decision.md.tpl")),
        "templates/docs/design-doc.md.tpl" => {
            Some(include_str!("../templates/docs/design-doc.md.tpl"))
        }
        "templates/docs/exec-plan.md.tpl" => {
            Some(include_str!("../templates/docs/exec-plan.md.tpl"))
        }
        "templates/docs/generated.md.tpl" => {
            Some(include_str!("../templates/docs/generated.md.tpl"))
        }
        "templates/docs/operation.md.tpl" => {
            Some(include_str!("../templates/docs/operation.md.tpl"))
        }
        "templates/docs/product-spec.md.tpl" => {
            Some(include_str!("../templates/docs/product-spec.md.tpl"))
        }
        "templates/docs/reference.md.tpl" => {
            Some(include_str!("../templates/docs/reference.md.tpl"))
        }
        "templates/docs/worklog.md.tpl" => Some(include_str!("../templates/docs/worklog.md.tpl")),
        "templates/entrypoints/AGENTS.md.tpl" => {
            Some(include_str!("../templates/entrypoints/AGENTS.md.tpl"))
        }
        "templates/entrypoints/CLAUDE.md.tpl" => {
            Some(include_str!("../templates/entrypoints/CLAUDE.md.tpl"))
        }
        _ => None,
    }
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
