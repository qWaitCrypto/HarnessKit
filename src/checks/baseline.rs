use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::checks::CheckView;
use crate::cli::CommandOptions;
use crate::Result;

const BASELINE_PATH: &str = ".harnesskit/state/check-baseline.json";

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct BaselineFinding {
    pub(crate) rule_id: String,
    pub(crate) severity: String,
    pub(crate) subject_path: String,
    pub(crate) message: String,
    pub(crate) hint: Option<String>,
    pub(crate) fixable: bool,
    // Keep identity independent of mutable wording, counts and timestamps.
    identity: String,
}

impl BaselineFinding {
    fn from_check(check: &CheckView) -> Self {
        let evidence: serde_json::Value =
            serde_json::from_str(&check.evidence_json).unwrap_or_default();
        let discriminator = match check.rule_id.as_str() {
            "broken-link"
            | "canonical-links-reference"
            | "canonical-links-generated"
            | "external-ref-changed" => evidence.get("target"),
            "frontmatter-field-not-allowed" => evidence.get("field"),
            "missing-scope-path" => evidence.get("scope_path"),
            _ => None,
        };
        Self {
            rule_id: check.rule_id.clone(),
            severity: check.severity.clone(),
            subject_path: check.subject_path.clone(),
            message: check.message.clone(),
            hint: check.hint.clone(),
            fixable: check.fixable,
            identity: serde_json::to_string(&(
                &check.rule_id,
                &check.subject_path,
                &check.severity,
                discriminator,
            ))
            .expect("serialize finding identity"),
        }
    }
}

#[derive(Serialize, Deserialize)]
struct Baseline {
    version: u32,
    findings: Vec<BaselineFinding>,
}

#[derive(Serialize)]
pub(crate) struct BaselineDelta {
    pub(crate) path: &'static str,
    pub(crate) available: bool,
    pub(crate) created: bool,
    pub(crate) updated: bool,
    pub(crate) new_count: usize,
    pub(crate) resolved_count: usize,
    pub(crate) new: Vec<BaselineFinding>,
    pub(crate) resolved: Vec<BaselineFinding>,
}

impl Default for BaselineDelta {
    fn default() -> Self {
        Self {
            path: BASELINE_PATH,
            available: false,
            created: false,
            updated: false,
            new_count: 0,
            resolved_count: 0,
            new: Vec::new(),
            resolved: Vec::new(),
        }
    }
}

pub(crate) fn check_baseline(
    root: &Path,
    checks: &[CheckView],
    options: &CommandOptions,
    update: bool,
) -> Result<BaselineDelta> {
    let current = checks
        .iter()
        .filter(|check| !check.suppressed)
        .map(BaselineFinding::from_check)
        .map(|finding| (finding.identity.clone(), finding))
        .collect::<BTreeMap<_, _>>();
    let path = root.join(BASELINE_PATH);
    let previous = match fs::read(&path) {
        Ok(bytes) => serde_json::from_slice::<Baseline>(&bytes)
            .ok()
            .filter(|baseline| baseline.version == 1),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    let mut delta = BaselineDelta {
        available: true,
        created: previous.is_none(),
        updated: update,
        ..BaselineDelta::default()
    };
    if let Some(previous) = previous {
        let previous_ids = previous
            .findings
            .iter()
            .map(|item| item.identity.as_str())
            .collect::<BTreeSet<_>>();
        delta.new = current
            .values()
            .filter(|item| !previous_ids.contains(item.identity.as_str()))
            .cloned()
            .collect();
        delta.resolved = previous
            .findings
            .into_iter()
            .filter(|item| !current.contains_key(&item.identity))
            .collect();
    }
    // Even filtered checks establish/update the complete baseline. A narrow
    // --rules invocation must never erase findings from another rule.
    if delta.created || update {
        fs::create_dir_all(
            path.parent()
                .ok_or("check baseline has no parent directory")?,
        )?;
        let baseline = Baseline {
            version: 1,
            findings: current.into_values().collect(),
        };
        let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
        fs::write(&temporary, serde_json::to_vec_pretty(&baseline)?)?;
        fs::rename(temporary, path)?;
    }
    let matches_rule = |finding: &BaselineFinding| {
        options
            .rules
            .as_ref()
            .map(|rules| {
                rules.contains(&finding.rule_id)
                    || rules.contains(&crate::checks::rule_group(&finding.rule_id))
            })
            .unwrap_or(true)
    };
    delta.new.retain(matches_rule);
    delta.resolved.retain(matches_rule);
    delta.new_count = delta.new.len();
    delta.resolved_count = delta.resolved.len();
    Ok(delta)
}

#[derive(Default, Serialize)]
pub(crate) struct RuleSummary {
    pub(crate) rule_id: String,
    pub(crate) error: usize,
    pub(crate) warning: usize,
    pub(crate) info: usize,
    pub(crate) suppressed: usize,
}

#[derive(Default, Serialize)]
pub(crate) struct CheckSummary {
    pub(crate) error: usize,
    pub(crate) warning: usize,
    pub(crate) info: usize,
    pub(crate) suppressed: usize,
    pub(crate) by_rule: Vec<RuleSummary>,
}

impl CheckSummary {
    pub(crate) fn from_checks(checks: &[CheckView]) -> Self {
        let mut summary = Self::default();
        let mut rules = BTreeMap::<String, RuleSummary>::new();
        for check in checks {
            let rule = rules
                .entry(check.rule_id.clone())
                .or_insert_with(|| RuleSummary {
                    rule_id: check.rule_id.clone(),
                    ..RuleSummary::default()
                });
            if check.suppressed {
                summary.suppressed += 1;
                rule.suppressed += 1;
            } else {
                match check.severity.as_str() {
                    "error" => {
                        summary.error += 1;
                        rule.error += 1;
                    }
                    "warning" => {
                        summary.warning += 1;
                        rule.warning += 1;
                    }
                    _ => {
                        summary.info += 1;
                        rule.info += 1;
                    }
                }
            }
        }
        summary.by_rule = rules.into_values().collect();
        summary
    }
}
