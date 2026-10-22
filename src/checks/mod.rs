mod contamination;
mod coverage;
mod duplicate;
mod frontmatter;
mod history;
mod lifecycle;
mod manifest;
mod stale;

#[cfg(test)]
pub(crate) use stale::newest_existing_path_mtime;

use std::path::Path;

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

#[derive(Clone, serde::Serialize)]
pub(crate) struct IndexCheck {
    pub(crate) rule_id: String,
    pub(crate) severity: String,
    pub(crate) subject_path: String,
    pub(crate) message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) hint: Option<String>,
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
    add_history_checks(schema, repo_root, &mut checks);
    lifecycle::add_lifecycle_checks(schema, repo_root, docs, &mut checks)?;
    coverage::add_coverage_checks(schema, repo_root, docs, &mut checks)?;

    Ok(checks)
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
        evidence_json: evidence_object_json(evidence),
    });
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
                hint: hint_from_evidence(cell(row, 4)),
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
        "duplicate-content" | "overlap-heading-fingerprint" => "duplicate".to_string(),
        "canonical-links-generated" | "canonical-links-reference" => "contamination".to_string(),
        "frontmatter-field-not-allowed" => "frontmatter".to_string(),
        "entrypoint-too-long"
        | "duplicated-current-pointer"
        | "active-but-historical"
        | "superseded-not-marked"
        | "stale-active-plan" => "lifecycle".to_string(),
        "host-git-exclude-missing"
        | "managed-file-not-excluded"
        | "history-missing"
        | "history-dirty-since-snapshot"
        | "history-schema-not-snapshotted" => "history".to_string(),
        other => other.to_string(),
    }
}
