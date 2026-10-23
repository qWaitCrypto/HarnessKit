use super::{freshness_badge, paginate_lines, short_hint, RenderLine, RenderOptions, RenderedPage};
use crate::code::CodeRegion;
use crate::context::{CodeChangesSummary, CodeContext, DiffContext};
use crate::discover::Coverage;
use std::collections::BTreeMap;

fn label(region: &CodeRegion) -> String {
    if region.name.is_empty() {
        region.path.clone()
    } else {
        format!("{}#{}", region.path, region.name)
    }
}

pub(crate) fn print_code_context_with_options(
    packet: &CodeContext,
    coverage: &Coverage,
    hints: &[String],
    options: &RenderOptions,
) -> RenderedPage {
    let mut lines = vec![RenderLine::content(format!(
        "Context --for {}:",
        packet.input
    ))];
    if let Some(message) = packet.message {
        lines.push(RenderLine::content(message));
    } else {
        let mut seen_docs = std::collections::BTreeSet::new();
        for doc in &packet.documents {
            let badge = if doc.lifecycle == "historical" {
                " superseded"
            } else {
                ""
            };
            let mut excerpt = doc.summary.clone();
            for target in &packet.targets {
                excerpt = super::local_anchor_names(&excerpt, &target.path);
            }
            let summary = if excerpt.is_empty() {
                String::new()
            } else {
                format!(" :: {}", excerpt)
            };
            lines.push(RenderLine::content(format!(
                "- {} §{}{}{}",
                if seen_docs.insert(&doc.path) {
                    doc.path.as_str()
                } else {
                    ""
                },
                if doc.heading.is_empty() {
                    &doc.title
                } else {
                    &doc.heading
                },
                badge,
                summary
            )));
        }
    }
    if !packet.cochanged_files.is_empty() {
        lines.push(RenderLine::related("Co-changed files:"));
        for file in &packet.cochanged_files {
            lines.push(RenderLine::related(format!("- {} ~", file.path)));
        }
    }
    for state in packet
        .anchor_status
        .iter()
        .filter(|state| state.status != "current")
    {
        let badge = freshness_badge(&state.status);
        lines.push(RenderLine::related(format!(
            "- {} `{}` {}",
            state.doc,
            state.anchor,
            if badge.is_empty() {
                state.status.as_str()
            } else {
                badge
            }
        )));
    }
    if !packet.truncated.is_empty() {
        lines.push(RenderLine::related(human_truncation(
            &packet.truncated,
            options,
            &format!("context --for {}", packet.input),
        )));
    }
    if options.verbose {
        lines.push(RenderLine::related(coverage.summary()));
    }
    if let Some(hint) = hints.first() {
        lines.push(RenderLine::hint(format!("Hint: {}", short_hint(hint))));
    }
    let page = paginate_lines(lines, options, "context");
    for line in &page.lines {
        println!("{}", line);
    }
    page
}

pub(crate) fn print_diff_context_with_options(
    packet: &DiffContext,
    coverage: &Coverage,
    hints: &[String],
    options: &RenderOptions,
) -> RenderedPage {
    let mut lines = vec![
        RenderLine::content(format!(
            "Context --diff {} + working tree: Changed code: {} files, {} regions (covered {}, uncovered {})",
            packet.revision.as_deref().unwrap_or("HEAD"),
            packet.totals.files,
            packet.totals.regions,
            packet.totals.covered,
            packet.totals.uncovered
        )),
    ];
    for file in &packet.files {
        lines.push(RenderLine::content(format!(
            "- {} [{}; {}]: {}",
            file.path,
            file.change,
            file.coverage,
            file.regions
                .iter()
                .map(|impact| if impact.region.name.is_empty() {
                    "file region"
                } else {
                    &impact.region.name
                })
                .collect::<Vec<_>>()
                .join(", ")
        )));
        let docs = file
            .documents
            .iter()
            .take(3)
            .map(|doc| {
                let badge = freshness_badge(&doc.lifecycle);
                let heading = if doc.heading.is_empty() {
                    String::new()
                } else {
                    format!(" §{}", doc.heading)
                };
                format!(
                    "{}{}{} :: {}",
                    doc.path,
                    heading,
                    if badge.is_empty() {
                        String::new()
                    } else {
                        format!(" {badge}")
                    },
                    super::local_anchor_names(&doc.summary, &file.path)
                )
            })
            .collect::<Vec<_>>();
        lines.push(RenderLine::related(format!(
            "  → {}",
            if docs.is_empty() {
                "no related documents".to_string()
            } else {
                docs.join(", ")
            }
        )));
    }
    if packet.totals.drifting_anchors > 0 {
        lines.push(RenderLine::related(format!(
            "Predicted anchor issues: {}",
            packet.totals.drifting_anchors
        )));
    }
    for state in packet.predicted_drift.iter().take(5) {
        let badge = freshness_badge(&state.status);
        lines.push(RenderLine::related(format!(
            "- {} `{}` {} — code changed since review; confirm the document constraints.",
            state.doc,
            state
                .anchor
                .split_once('#')
                .map(|(_, name)| name)
                .unwrap_or(&state.anchor),
            if badge.is_empty() {
                state.status.as_str()
            } else {
                badge
            }
        )));
    }
    let remaining = packet
        .uncovered_regions
        .iter()
        .filter(|region| {
            !packet.files.iter().any(|file| {
                file.regions
                    .iter()
                    .any(|impact| impact.region.id == region.id)
            })
        })
        .collect::<Vec<_>>();
    if !remaining.is_empty() {
        lines.push(RenderLine::related(format!(
            "Additional uncovered code regions: {}",
            remaining.len()
        )));
    }
    for region in remaining.into_iter().take(5) {
        lines.push(RenderLine::related(format!(
            "- {} (L{}–L{})",
            label(region),
            region.span_start,
            region.span_end
        )));
    }
    if !packet.truncated.is_empty() {
        lines.push(RenderLine::related(human_truncation(
            &packet.truncated,
            options,
            "context --diff",
        )));
    }
    if options.verbose {
        lines.push(RenderLine::related(coverage.summary()));
    }
    if let Some(hint) = hints.first() {
        lines.push(RenderLine::hint(format!("Hint: {}", short_hint(hint))));
    }
    let page = paginate_lines(lines, options, "context");
    for line in &page.lines {
        println!("{}", line);
    }
    page
}

fn human_truncation(
    truncated: &BTreeMap<String, usize>,
    options: &RenderOptions,
    command: &str,
) -> String {
    let mut parts = Vec::new();
    if let Some(count) = truncated.get("files").copied().filter(|count| *count > 0) {
        parts.push(format!("{count} more files"));
    }
    if let Some(count) = truncated
        .get("uncovered_regions")
        .copied()
        .filter(|count| *count > 0)
    {
        parts.push(format!("{count} uncovered regions"));
    }
    if parts.is_empty() {
        let regions = truncated
            .iter()
            .filter(|(key, count)| key.ends_with(":regions") && **count > 0)
            .map(|(_, count)| *count)
            .sum::<usize>();
        if regions > 0 {
            parts.push(format!("{regions} more regions"));
        }
    }
    if parts.is_empty() {
        for (key, count) in truncated {
            if *count > 0 {
                parts.push(format!("{count} more {key}"));
            }
        }
    }
    format!(
        "… {} (harnesskit {} --page {})",
        parts.join(", "),
        command,
        options.page.saturating_add(1)
    )
}

pub(super) fn code_changes_lines(summary: &CodeChangesSummary) -> Vec<RenderLine> {
    if !summary.available {
        return Vec::new();
    }
    let mut lines = vec![RenderLine::content(
        "Code changes since snapshot: covered / uncovered / drifted",
    )];
    {
        lines.push(RenderLine::content(format!(
            "- Covered {} / uncovered {} / drifted {}",
            summary.covered, summary.uncovered, summary.drifted
        )));
        for area in summary.areas.iter().take(4) {
            lines.push(RenderLine::related(format!(
                "- {}{} [{}]",
                super::short_line(&area.path, 140),
                if area.name.is_empty() {
                    String::new()
                } else {
                    format!(":{}", super::short_line(&area.name, 100))
                },
                area.status
            )));
        }
        let omitted = summary.truncated + summary.areas.len().saturating_sub(4);
        if omitted > 0 {
            lines.push(RenderLine::related(format!("- {} more regions", omitted)));
        }
    }
    lines
}
