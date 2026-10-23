use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use crate::fs_utils::normalize_path;
use crate::schema::{
    configured_architecture_path, core_file_rel_path, glob_matcher, parse_schema,
    DocCollectionSpec, Schema, BUNDLED_SCHEMA_TEXT,
};
use crate::Result;

pub(crate) fn project_hint(target_dir: &Path, schema: &Schema) -> Option<String> {
    if has_project_docs(target_dir, schema) {
        None
    } else {
        Some(format!(
            "No HarnessKit docs or entrypoints found at {:?}; run `harnesskit init --preview` in that directory.",
            target_dir
        ))
    }
}

fn has_project_docs(target_dir: &Path, schema: &Schema) -> bool {
    (if schema.inferred_root.is_some() {
        let mut paths = BTreeSet::new();
        collect_markdown_files(target_dir, target_dir, &mut paths).is_ok() && !paths.is_empty()
    } else {
        target_dir.join(&schema.managed_root).is_dir()
    }) || [
        &schema.entrypoints.agents,
        &schema.entrypoints.claude,
        &schema.entrypoints.architecture,
        &schema.entrypoints.manifest,
        &schema.entrypoints.project_map,
    ]
    .into_iter()
    .flatten()
    .any(|path| target_dir.join(path).is_file())
}

pub(crate) fn orphan_state_hints(target_dir: &Path, schema: &Schema) -> Result<Vec<String>> {
    let default_schema = parse_schema(BUNDLED_SCHEMA_TEXT)?;
    let mut pending = vec![target_dir.to_path_buf()];
    let mut orphan_paths = Vec::new();
    while let Some(dir) = pending.pop() {
        let state_root = dir.join(".harnesskit");
        if state_root.is_dir() {
            // A nested project may use a different docs root and entrypoints.
            let local_schema = if dir != target_dir {
                fs::read_to_string(state_root.join("schema.yaml"))
                    .ok()
                    .and_then(|text| parse_schema(&text).ok())
            } else {
                None
            };
            let candidate_schema = if dir == target_dir {
                schema
            } else {
                local_schema.as_ref().unwrap_or(&default_schema)
            };
            if !has_project_docs(&dir, candidate_schema) {
                orphan_paths.push(normalize_path(state_root.strip_prefix(target_dir)?));
            }
        }
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            // Never follow symlinks or inspect VCS/state, hidden, build or dependency trees.
            if entry.file_type()?.is_dir()
                && !name.starts_with('.')
                && !matches!(name.as_ref(), "target" | "node_modules")
            {
                pending.push(entry.path());
            }
        }
    }
    orphan_paths.sort();
    Ok(orphan_paths
        .into_iter()
        .map(|path| {
            format!(
                "Possible orphan HarnessKit state at {:?}: its directory has no configured docs root or entrypoints. Verify target_dir and rerun with the intended project root; preserve .harnesskit/history before any cleanup.",
                path
            )
        })
        .collect())
}

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Coverage {
    pub(crate) markdown_total: usize,
    pub(crate) managed: usize,
    pub(crate) unmanaged: usize,
    pub(crate) excluded: usize,
    pub(crate) by_dir: Vec<DirectoryCoverage>,
    #[serde(serialize_with = "serialize_ratio")]
    pub(crate) ratio: f64,
    pub(crate) scan_root: String,
    #[serde(default)]
    pub(crate) root_reason: Option<String>,
    pub(crate) skipped: Vec<String>,
    pub(crate) mappings: Vec<DocMapping>,
}

fn serialize_ratio<S: serde::Serializer>(
    ratio: &f64,
    serializer: S,
) -> std::result::Result<S::Ok, S::Error> {
    // The legacy CLI formatter rounds relation confidence to two decimals.
    // Coverage must retain its numeric denominator precision.
    use serde::Serialize;
    serde_json::value::RawValue::from_string(serde_json::to_string(ratio).unwrap())
        .map_err(serde::ser::Error::custom)?
        .serialize(serializer)
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct DirectoryCoverage {
    pub(crate) dir: String,
    pub(crate) count: usize,
    pub(crate) status: String,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct DocMapping {
    pub(crate) path: String,
    pub(crate) status: String,
    pub(crate) collection: Option<String>,
    pub(crate) role: Option<String>,
    pub(crate) reason: String,
}

impl Coverage {
    pub(crate) fn managed_paths(&self) -> Vec<String> {
        self.mappings
            .iter()
            .filter(|item| item.status == "managed")
            .map(|item| item.path.clone())
            .collect()
    }

    pub(crate) fn hints(&self) -> Vec<String> {
        let mut hints = self.explanation_hints();
        for dir in self.by_dir.iter().filter(|item| item.status == "unmanaged") {
            hints.push(format!("{} unmanaged Markdown file(s) in {}. Merge into .harnesskit/schema.yaml (globs are repository-relative):\n{}", dir.count, dir.dir, include_hint(&dir.dir)));
        }
        hints
    }

    pub(crate) fn explanation_hints(&self) -> Vec<String> {
        let mut hints = vec![format!(
            "Coverage scans Markdown under {:?}, configured core/entrypoint files and explicit include matches. Templates, hidden directories, symlinks, target and node_modules are skipped. Nested projects (subdirectories with AGENTS.md, CLAUDE.md or .harnesskit) and fixture directories (tests/fixtures, testdata, fixture/fixtures under examples) are excluded by default, alongside schema ignore matches. Ratio = managed / (managed + unmanaged); an empty scope has ratio 0. Use --json (coverage.mappings) or doctor --layout for each classification; include overrides default exclusions and inference, ignore takes precedence.", self.scan_root
        )];
        if self.root_reason.is_some() {
            hints.push("No default docs/ directory or explicit schema/docs-root was supplied: inferred managed_root '.' and supporting notes for otherwise unclassified repository Markdown. Use doctor --layout to review, --write-schema to persist, or include/ignore to refine the scope.".to_string());
        }
        hints
    }

    pub(crate) fn summary(&self) -> String {
        format!("Coverage: {}/{} eligible Markdown managed ({:.1}%), {} total, {} unmanaged, {} excluded; scan root {}",
            self.managed, self.managed + self.unmanaged, self.ratio * 100.0,
            self.markdown_total, self.unmanaged, self.excluded, self.scan_root)
    }
}

pub(crate) fn include_hint(dir: &str) -> String {
    let pattern = if dir == "." {
        "*".to_string()
    } else {
        format!("{}/*", globset::escape(dir))
    };
    format!(
        "include:\n  {}: notes",
        serde_json::to_string(&pattern).unwrap()
    )
}

pub(crate) fn discover_docs(repo_root: &Path, schema: &Schema) -> Result<Coverage> {
    let mut paths = BTreeSet::new();
    let includes = schema
        .include
        .iter()
        .map(|item| glob_matcher(&item.pattern))
        .collect::<Result<Vec<_>>>()?;
    let root = repo_root.join(&schema.managed_root);
    if root.is_dir() {
        collect_markdown_files(repo_root, &root, &mut paths)?;
    }
    // Includes may explicitly opt in Markdown outside managed_root.
    if !schema.include.is_empty() {
        let mut candidates = BTreeSet::new();
        collect_markdown_files(repo_root, repo_root, &mut candidates)?;
        paths.extend(
            candidates
                .into_iter()
                .filter(|path| includes.iter().any(|matcher| matcher.is_match(path))),
        );
    }
    for path in [
        &schema.entrypoints.agents,
        &schema.entrypoints.claude,
        &schema.entrypoints.architecture,
        &schema.entrypoints.manifest,
        &schema.entrypoints.project_map,
    ]
    .into_iter()
    .flatten()
    .cloned()
    .chain(
        schema
            .core_files
            .iter()
            .map(|spec| core_file_rel_path(schema, spec)),
    ) {
        let path = normalize_path(Path::new(&path));
        if !skipped_path(&path) && regular_file(repo_root, &path) && is_markdown(&path) {
            paths.insert(path);
        }
    }
    let ignores = schema
        .ignore
        .iter()
        .map(|pattern| Ok((pattern, glob_matcher(pattern)?)))
        .collect::<Result<Vec<_>>>()?;
    let mut mappings = Vec::new();
    let mut counts = BTreeMap::<(String, String), usize>::new();
    for path in paths {
        let (collection, reason) = classify_path(schema, &path);
        let default_exclusion = if includes.iter().any(|matcher| matcher.is_match(&path)) {
            None
        } else {
            default_exclusion_reason(repo_root, &path)
        };
        let (status, reason) = if let Some((pattern, _)) =
            ignores.iter().find(|(_, matcher)| matcher.is_match(&path))
        {
            ("excluded", format!("schema.ignore: {}", pattern))
        } else if let Some(reason) = default_exclusion {
            ("excluded", reason.to_string())
        } else if collection.is_some() || is_configured_doc(schema, &path) {
            ("managed", reason)
        } else {
            (
                "unmanaged",
                "No collection, alias, filename heuristic or flat-doc default matched".to_string(),
            )
        };
        let dir = Path::new(&path)
            .parent()
            .map(normalize_path)
            .filter(|dir| !dir.is_empty())
            .unwrap_or_else(|| ".".to_string());
        *counts.entry((dir, status.to_string())).or_default() += 1;
        mappings.push(DocMapping {
            role: (status == "managed")
                .then(|| infer_index_role(schema, &path, collection.as_deref())),
            path,
            status: status.to_string(),
            collection,
            reason,
        });
    }
    let managed = mappings
        .iter()
        .filter(|item| item.status == "managed")
        .count();
    let unmanaged = mappings
        .iter()
        .filter(|item| item.status == "unmanaged")
        .count();
    Ok(Coverage {
        markdown_total: mappings.len(),
        managed,
        unmanaged,
        excluded: mappings
            .iter()
            .filter(|item| item.status == "excluded")
            .count(),
        by_dir: counts
            .into_iter()
            .map(|((dir, status), count)| DirectoryCoverage { dir, count, status })
            .collect(),
        ratio: if managed + unmanaged == 0 {
            0.0
        } else {
            managed as f64 / (managed + unmanaged) as f64
        },
        scan_root: schema.managed_root.clone(),
        root_reason: schema.inferred_root.as_ref().map(|root| {
            format!(
                "Default {}/ absent and no explicit schema/docs-root; infer repository root",
                root
            )
        }),
        skipped: vec![
            "templates/ (at any depth)".to_string(),
            "hidden directories (including .harnesskit)".to_string(),
            "symlinks".to_string(),
            "target/".to_string(),
            "node_modules/".to_string(),
        ],
        mappings,
    })
}

pub(crate) fn collect_managed_doc_paths(repo_root: &Path, schema: &Schema) -> Result<Vec<String>> {
    Ok(discover_docs(repo_root, schema)?.managed_paths())
}

fn default_exclusion_reason(repo_root: &Path, path: &str) -> Option<&'static str> {
    let parent = Path::new(path).parent()?;
    let directories = parent.iter().collect::<Vec<_>>();
    // Fixture locations take precedence when a fixture is also a nested project.
    if directories.iter().any(|name| *name == "testdata")
        || directories
            .windows(2)
            .any(|parts| parts[0] == "tests" && parts[1] == "fixtures")
        || directories.iter().enumerate().any(|(index, name)| {
            *name == "examples"
                && directories[index + 1..]
                    .iter()
                    .any(|name| *name == "fixture" || *name == "fixtures")
        })
    {
        return Some("fixture-dir");
    }
    // Keep scanning excluded trees so coverage can count and explain their files.
    // The target repository's own entrypoints/state never make it a nested project.
    for ancestor in parent.ancestors().filter(|dir| !dir.as_os_str().is_empty()) {
        let dir = repo_root.join(ancestor);
        if dir.join("AGENTS.md").is_file()
            || dir.join("CLAUDE.md").is_file()
            || dir.join(".harnesskit").is_dir()
        {
            return Some("nested-project");
        }
    }
    None
}

fn skipped_path(path: &str) -> bool {
    Path::new(path).components().any(|part| match part {
        std::path::Component::Normal(name) => {
            let name = name.to_string_lossy();
            name.starts_with('.')
                || matches!(name.as_ref(), "templates" | "target" | "node_modules")
        }
        std::path::Component::ParentDir | std::path::Component::RootDir => true,
        _ => false,
    })
}

fn regular_file(root: &Path, relative: &str) -> bool {
    let mut current = root.to_path_buf();
    for part in Path::new(relative).components() {
        current.push(part);
        if fs::symlink_metadata(&current)
            .map(|metadata| metadata.file_type().is_symlink())
            .unwrap_or(true)
        {
            return false;
        }
    }
    current.is_file()
}

pub(crate) fn is_ignored(schema: &Schema, path: &str) -> bool {
    schema.ignore.iter().any(|pattern| {
        glob_matcher(pattern)
            .expect("validated schema glob")
            .is_match(path)
    })
}

pub(crate) fn is_markdown(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.eq_ignore_ascii_case("md") || ext.eq_ignore_ascii_case("markdown"))
        .unwrap_or(false)
}

fn collect_markdown_files(repo_root: &Path, dir: &Path, out: &mut BTreeSet<String>) -> Result<()> {
    if fs::symlink_metadata(dir)?.file_type().is_symlink() {
        return Ok(());
    }
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let relative = normalize_path(path.strip_prefix(repo_root)?);
        if skipped_path(&relative) {
            continue;
        }
        let kind = entry.file_type()?;
        if kind.is_dir() {
            collect_markdown_files(repo_root, &path, out)?;
        } else if kind.is_file() && is_markdown(&relative) {
            out.insert(relative);
        }
    }
    Ok(())
}

pub(crate) fn collection_lookup(schema: &Schema) -> BTreeMap<String, &DocCollectionSpec> {
    schema
        .doc_collections
        .iter()
        .map(|spec| (spec.name.clone(), spec))
        .collect()
}

fn is_configured_doc(schema: &Schema, path: &str) -> bool {
    is_entrypoint_path(schema, path)
        || schema.entrypoints.manifest.as_deref() == Some(path)
        || schema.entrypoints.project_map.as_deref() == Some(path)
        || schema
            .core_files
            .iter()
            .any(|spec| core_file_rel_path(schema, spec) == path)
}

fn classify_path(schema: &Schema, path: &str) -> (Option<String>, String) {
    for item in &schema.include {
        if glob_matcher(&item.pattern)
            .expect("validated schema glob")
            .is_match(path)
        {
            return (
                Some(item.collection.clone()),
                format!("schema.include: {} -> {}", item.pattern, item.collection),
            );
        }
    }
    if is_configured_doc(schema, path) {
        return (None, "schema core/entrypoint".to_string());
    }
    if let Some(spec) = schema
        .doc_collections
        .iter()
        .filter(|spec| {
            path.starts_with(&format!(
                "{}/",
                normalize_path(&Path::new(&schema.managed_root).join(&spec.path))
            ))
        })
        .max_by_key(|spec| spec.path.len())
    {
        return (
            Some(spec.name.clone()),
            format!("schema.doc_collections: {}", spec.name),
        );
    }
    let root = normalize_path(Path::new(&schema.managed_root));
    let relative = if root.is_empty() {
        Some(path)
    } else {
        path.strip_prefix(&format!("{}/", root))
    };
    if let Some(relative) = relative {
        let directory = relative.split('/').next().unwrap_or("");
        let alias = match directory {
            "reference" => Some("references"),
            "adr" | "adrs" => Some("decisions"),
            "plans" | "exec-plans" => Some("exec_plans"),
            "design" => Some("design_docs"),
            "specs" => Some("product_specs"),
            "runbooks" => Some("operations"),
            _ => None,
        };
        if let Some(collection) = alias.filter(|_| relative.contains('/')) {
            return (
                Some(collection.to_string()),
                format!("directory alias: {} -> {}", directory, collection),
            );
        }
        let filename = Path::new(path)
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_lowercase();
        let inferred = if filename.contains("decision") {
            Some(("decisions", "*decision*"))
        } else if filename.contains("plan") {
            Some(("exec_plans", "*plan*"))
        } else if filename.starts_with("architecture") {
            Some(("design_docs", "architecture*"))
        } else {
            None
        };
        if let Some((collection, pattern)) = inferred {
            return (
                Some(collection.to_string()),
                format!("filename heuristic: {} -> {}", pattern, collection),
            );
        }
        if schema.inferred_root.is_some() {
            return (
                Some("notes".to_string()),
                "repository fallback: supporting notes (default docs/ absent)".to_string(),
            );
        }
        if !relative.contains('/') {
            return (
                Some("notes".to_string()),
                "flat Markdown under managed_root -> supporting notes".to_string(),
            );
        }
    }
    (None, String::new())
}

pub(crate) fn collection_name_for_path(schema: &Schema, path: &str) -> Option<String> {
    classify_path(schema, path).0
}

pub(crate) fn default_authority(collection: &str) -> Option<String> {
    match collection {
        "notes" | "exec_plans" | "operations" => Some("supporting".to_string()),
        "decisions" | "design_docs" | "product_specs" => Some("canonical".to_string()),
        "references" => Some("reference".to_string()),
        _ => None,
    }
}

pub(crate) fn infer_index_role(schema: &Schema, path: &str, collection: Option<&str>) -> String {
    if is_entrypoint_path(schema, path) {
        "entrypoint".to_string()
    } else if configured_architecture_path(schema).as_deref() == Some(path) {
        "architecture".to_string()
    } else if path == schema.entrypoints.manifest.as_deref().unwrap_or("") {
        "manifest".to_string()
    } else if collection.is_some() && collection != Some("notes") && path.ends_with("/index.md") {
        "collection-index".to_string()
    } else {
        collection.unwrap_or("doc").to_string()
    }
}

pub(crate) fn is_entrypoint_path(schema: &Schema, path: &str) -> bool {
    schema.entrypoints.agents.as_deref() == Some(path)
        || schema.entrypoints.claude.as_deref() == Some(path)
        || schema.entrypoints.architecture.as_deref() == Some(path)
}

pub(crate) fn is_anchor_candidate(schema: &Schema, path: &str) -> bool {
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
