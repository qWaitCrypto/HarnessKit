mod baseline;
mod contamination;
mod coverage;
mod duplicate;
mod frontmatter;
mod history;
mod hotspot;
mod lifecycle;
mod manifest;
mod stale;

#[cfg(test)]
pub(crate) use stale::newest_existing_path_mtime;

use std::path::Path;

pub(crate) use baseline::{check_baseline, BaselineDelta, CheckSummary};

use crate::cli::CommandOptions;
use crate::index::{cell, DocRelation, IndexedDoc};
use crate::schema::Schema;
use crate::sqlite::QueryResult;

use self::contamination::add_contamination_checks;
use self::duplicate::add_duplicate_checks;
use self::frontmatter::add_frontmatter_constraint_checks;
use self::history::add_history_checks;
use self::manifest::build_manifest_checks;
use self::stale::add_stale_anchor_checks;

#[derive(Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct IndexCheck {
    pub(crate) rule_id: String,
    pub(crate) severity: String,
    pub(crate) subject_path: String,
    pub(crate) message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) hint: Option<String>,
    pub(crate) fixable: bool,
    #[serde(skip_serializing)]
    pub(crate) evidence_json: String,
}

#[derive(Clone)]
pub(crate) struct CheckView {
    pub(crate) severity: String,
    pub(crate) rule_id: String,
    pub(crate) subject_path: String,
    pub(crate) message: String,
    pub(crate) hint: Option<String>,
    pub(crate) fixable: bool,
    pub(crate) evidence_json: String,
    pub(crate) suppressed: bool,
    pub(crate) suppression_reason: Option<String>,
}

pub(crate) fn build_index_checks(
    schema: &Schema,
    repo_root: &Path,
    docs: &[IndexedDoc],
    relations: &[DocRelation],
) -> crate::Result<Vec<IndexCheck>> {
    let mut checks = build_manifest_checks(schema, docs, relations);

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
    frontmatter::add_frontmatter_parse_checks(repo_root, docs, &mut checks)?;
    add_history_checks(schema, repo_root, docs, &mut checks)?;
    lifecycle::add_lifecycle_checks(schema, repo_root, docs, &mut checks)?;
    coverage::add_coverage_checks(schema, repo_root, docs, &mut checks)?;

    for check in &mut checks {
        complete_check(check);
    }

    Ok(checks)
}

pub(crate) fn add_undocumented_hotspot_checks(
    db: &crate::sqlite::SqliteConnection,
    schema: &Schema,
    checks: &mut Vec<IndexCheck>,
) -> crate::Result<()> {
    hotspot::add_undocumented_hotspot_checks(db, schema, checks)
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
        hint: evidence
            .iter()
            .find(|(key, _)| *key == "hint")
            .map(|(_, value)| value.to_string()),
        fixable: false,
        evidence_json: evidence_object_json(evidence),
    });
}

pub(crate) fn complete_check(check: &mut IndexCheck) {
    let hint = check
        .hint
        .clone()
        .or_else(|| hint_from_evidence(&check.evidence_json))
        .unwrap_or_else(|| repair_hint(&check.rule_id, &check.subject_path, &check.evidence_json));
    let mut evidence: serde_json::Value =
        serde_json::from_str(&check.evidence_json).unwrap_or_else(|_| serde_json::json!({}));
    evidence["hint"] = hint.clone().into();
    evidence["fixable"] = check.fixable.into();
    check.hint = Some(hint);
    check.evidence_json = evidence.to_string();
}

fn repair_hint(rule: &str, path: &str, evidence: &str) -> String {
    let evidence: serde_json::Value = serde_json::from_str(evidence).unwrap_or_default();
    let target = evidence
        .get("target")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    match rule {
        "missing-entrypoint-doc" | "missing-required-doc" | "missing-collection-index" =>
            format!("Restore {}, or change its declaration in .harnesskit/schema.yaml if this layout is intentional.", path),
        "entrypoint-manifest-link" => format!("Add a link in {} to the configured manifest {}.", path, evidence["manifest"].as_str().unwrap_or("")),
        "manifest-missing-doc" => format!("Add a link to {} in {}.", path, evidence["manifest"].as_str().unwrap_or("")),
        "broken-link" => format!("Correct or remove the link to {} in {}, or restore the missing target.", target, path),
        "wrong-docs-navigation-target" => format!("Point navigation in {} to a Markdown document instead of {}.", path, target),
        "orphan-managed-doc" => format!("Link the documents in {} from an entrypoint or a nearby index.", path),
        "stale-explicit-anchor" => format!("Review the newer scoped code against {} and update the document if its contract changed.", path),
        "code-drift" => format!("Review the changed code, then update {} or run `harnesskit check --ack {}` when the document remains correct.", path, path),
        "anchor-moved" => format!("Run `harnesskit check --fix` to rewrite the moved anchor in {}.", path),
        "anchor-broken" => format!("Restore the target or update/remove the broken anchor in {}.", path),
        "missing-scope-path" => format!("Correct or remove the missing scope_paths entry in {}.", path),
        "duplicate-content" | "overlap-heading-fingerprint" => format!("Compare {} with the related paths in evidence; consolidate repeated content or clarify each document's scope.", path),
        "canonical-links-generated" | "canonical-links-reference" => format!("Review {}: link to the source contract for {}, and label supporting evidence as such.", path, target),
        "frontmatter-field-not-allowed" => format!("Remove the unsupported field from {}, or add it to the collection's allowed_frontmatter in .harnesskit/schema.yaml.", path),
        "frontmatter-parse-error" => format!("Repair the YAML frontmatter and its closing --- delimiter in {}.", path),
        "schema-parse-error" => format!("Repair the schema syntax in {} and rerun check.", path),
        "managed-file-not-excluded" | "host-git-exclude-missing" => format!("For local-only docs, add {} to .gitignore or .git/info/exclude; for shared docs, track them with git.", path),
        "history-missing" | "history-dirty-since-snapshot" | "history-schema-not-snapshotted" =>
            "Review the document changes, then run harnesskit history snapshot -m \"Reviewed docs\" in this project.".to_string(),
        _ => format!("Review {} and the evidence for {}; update the affected document or its schema declaration.", path, rule),
    }
}

fn evidence_object_json(items: &[(&str, &str)]) -> String {
    struct Evidence<'a>(&'a [(&'a str, &'a str)]);
    impl serde::Serialize for Evidence<'_> {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            serializer.collect_map(self.0.iter().copied())
        }
    }
    serde_json::to_string(&Evidence(items)).expect("serialize check evidence")
}

pub(crate) fn hint_from_evidence(evidence: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(evidence)
        .ok()?
        .get("hint")?
        .as_str()
        .map(str::to_string)
}

pub(crate) fn filter_and_suppress_checks(
    result: &QueryResult,
    schema: &Schema,
    options: &CommandOptions,
) -> Vec<CheckView> {
    let suppressions = schema
        .suppressions
        .iter()
        .map(|item| {
            (
                item,
                item.path_glob.then(|| {
                    crate::schema::glob_matcher(&item.path).expect("validated suppression glob")
                }),
            )
        })
        .collect::<Vec<_>>();
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
            let suppression = suppressions
                .iter()
                .find(|(item, matcher)| {
                    item.rule_id == rule_id
                        && (item.path == subject_path
                            || matcher
                                .as_ref()
                                .map(|matcher| matcher.is_match(&subject_path))
                                .unwrap_or(false))
                })
                .map(|(item, _)| *item);
            let evidence: serde_json::Value = serde_json::from_str(cell(row, 4)).unwrap_or_default();
            Some(CheckView {
                severity: cell(row, 0).to_string(),
                rule_id,
                subject_path,
                message: cell(row, 3).to_string(),
                hint: Some(
                    hint_from_evidence(cell(row, 4))
                        .unwrap_or_else(|| repair_hint(cell(row, 1), cell(row, 2), cell(row, 4))),
                ),
                fixable: evidence
                    .get("fixable")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false),
                evidence_json: cell(row, 4).to_string(),
                suppressed: suppression.is_some(),
                suppression_reason: suppression.map(|item| item.reason.clone()),
            })
        })
        .collect()
}

pub(crate) fn rule_group(rule_id: &str) -> String {
    match rule_id {
        "missing-entrypoint-doc"
        | "missing-required-doc"
        | "missing-collection-index"
        | "entrypoint-manifest-link"
        | "manifest-missing-doc"
        | "broken-link"
        | "unmanaged-docs"
        | "wrong-docs-navigation-target"
        | "orphan-managed-doc" => "manifest".to_string(),
        "stale-explicit-anchor" | "missing-scope-path" => "stale".to_string(),
        "anchor-unresolved"
        | "anchor-ambiguous"
        | "anchor-line-number"
        | "code-drift"
        | "anchor-moved"
        | "anchor-broken" => "anchors".to_string(),
        "duplicate-content" | "overlap-heading-fingerprint" => "duplicate".to_string(),
        "canonical-links-generated" | "canonical-links-reference" => "contamination".to_string(),
        "frontmatter-field-not-allowed" | "frontmatter-parse-error" => "frontmatter".to_string(),
        "entrypoint-too-long"
        | "duplicated-current-pointer"
        | "active-but-historical"
        | "superseded-not-marked"
        | "stale-active-plan" => "lifecycle".to_string(),
        "undocumented-hotspot" => "weave".to_string(),
        "host-git-exclude-missing"
        | "managed-file-not-excluded"
        | "history-missing"
        | "history-dirty-since-snapshot"
        | "history-schema-not-snapshotted" => "history".to_string(),
        other => other.to_string(),
    }
}
