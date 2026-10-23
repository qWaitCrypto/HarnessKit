use crate::code::CodeRegion;
use crate::context::{CodeChangesSummary, CodeContext, DiffContext};
use crate::discover::Coverage;
use super::{freshness_badge, paginate_lines, short_hint, RenderLine, RenderOptions, RenderedPage};

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
    let mut lines = vec![RenderLine::content(format!("Context --for {}:", packet.input))];
    if let Some(message) = packet.message {
        lines.push(RenderLine::content(message));
    } else {
        for doc in &packet.documents {
            let badge = if doc.lifecycle == "historical" {
                " superseded"
            } else {
                ""
            };
            let summary = if doc.summary.is_empty() {
                String::new()
            } else {
                format!(" :: {}", doc.summary)
            };
            lines.push(RenderLine::content(format!(
                "- {} §{}{}{}",
                doc.path,
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
            state.doc, state.anchor, if badge.is_empty() { state.status.as_str() } else { badge }
        )));
    }
    if !packet.truncated.is_empty() {
        lines.push(RenderLine::related(format!("Truncated: {:?}", packet.truncated)));
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
            "Context --diff {} + working tree:",
            packet.revision.as_deref().unwrap_or("HEAD")
        )),
        RenderLine::content(format!(
            "Changed code: {} files, {} regions (covered {}, uncovered {})",
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
            .map(|doc| format!("{} §{} [{}]", doc.path, doc.heading, doc.lifecycle))
            .collect::<Vec<_>>();
        lines.push(RenderLine::related(format!(
            "  → {}",
            if docs.is_empty() {
                "no related documents".to_string()
            } else {
                docs.join(", ")
            }
        )));
        if !file.decisions.is_empty() {
            lines.push(RenderLine::related(format!("  Decision constraints: {}", file.decisions.join(", "))));
        }
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
            "- {} `{}` {}",
            state.doc, state.anchor, if badge.is_empty() { state.status.as_str() } else { badge }
        )));
    }
    if packet.totals.uncovered > 0 {
        lines.push(RenderLine::related(format!("Uncovered code regions: {}", packet.totals.uncovered)));
    }
    for region in packet.uncovered_regions.iter().take(5) {
        lines.push(RenderLine::related(format!(
            "- {} (L{}–L{})",
            label(region),
            region.span_start,
            region.span_end
        )));
    }
    if !packet.truncated.is_empty() {
        lines.push(RenderLine::related(format!("Truncated: {:?}", packet.truncated)));
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
