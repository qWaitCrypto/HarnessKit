use crate::code::CodeRegion;
use crate::context::{CodeChangesSummary, CodeContext, DiffContext};
use crate::discover::Coverage;
use crate::output::println;

fn label(region: &CodeRegion) -> String {
    if region.name.is_empty() {
        region.path.clone()
    } else {
        format!("{}#{}", region.path, region.name)
    }
}

fn print_lines(mut lines: Vec<String>) {
    if lines.len() > 40 {
        let omitted = lines.len() - 39;
        lines.truncate(39);
        lines.push(format!(
            "… 另有 {omitted} 行未显示；使用 --json 查看结构化结果。"
        ));
    }
    for line in lines {
        println!("{}", super::short_line(&line, 360));
    }
}

pub(crate) fn print_code_context(packet: &CodeContext, coverage: &Coverage, hints: &[String]) {
    let mut lines = vec![format!("Context --for {}:", packet.input)];
    if !packet.targets.is_empty() {
        lines.push(format!(
            "代码: {}",
            packet
                .targets
                .iter()
                .map(label)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if let Some(message) = packet.message {
        lines.push(message.to_string());
    } else {
        lines.push("关联文档章节（决策与 active 计划优先）:".to_string());
        for doc in &packet.documents {
            let role = if doc.constrains {
                "决策约束"
            } else if doc.active_plan {
                "active 计划"
            } else {
                &doc.role
            };
            let kinds = doc
                .links
                .iter()
                .map(|link| link.kind.as_str())
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>()
                .join(",");
            lines.push(format!(
                "- {} §{} [{role}; status={}; lifecycle={}] ({kinds})",
                doc.path,
                if doc.heading.is_empty() {
                    &doc.title
                } else {
                    &doc.heading
                },
                if doc.status.is_empty() {
                    "default"
                } else {
                    &doc.status
                },
                doc.lifecycle
            ));
            if !doc.summary.is_empty() {
                lines.push(format!("  {}", doc.summary));
            }
        }
    }
    lines.push(format!(
        "常一起改的文件（top5）: {}",
        packet.cochanged_files.len()
    ));
    for file in &packet.cochanged_files {
        lines.push(format!(
            "- {} [INFERRED; {} 次; weight={:.4}]",
            file.path, file.count, file.weight
        ));
    }
    lines.push(format!(
        "锚点状态: {} 个正常，{} 个漂移/迁移/断链/歧义",
        packet.anchor_counts.get("current").copied().unwrap_or(0),
        packet
            .anchor_counts
            .iter()
            .filter(|(status, _)| status.as_str() != "current")
            .map(|(_, count)| count)
            .sum::<usize>()
    ));
    for state in packet
        .anchor_status
        .iter()
        .filter(|state| state.status != "current")
    {
        lines.push(format!(
            "- {} `{}` [{}]",
            state.doc, state.anchor, state.status
        ));
    }
    if !packet.truncated.is_empty() {
        lines.push(format!("省略计数: {:?}", packet.truncated));
    }
    lines.push(coverage.summary());
    lines.extend(hints.iter().map(|hint| format!("Hint: {hint}")));
    print_lines(lines);
}

pub(crate) fn print_diff_context(packet: &DiffContext, coverage: &Coverage, hints: &[String]) {
    let mut lines = vec![
        format!(
            "Context --diff {} + working tree:",
            packet.revision.as_deref().unwrap_or("HEAD")
        ),
        format!(
            "改动代码: {} 文件，{} 区域（已覆盖 {} / 未覆盖 {}）",
            packet.totals.files,
            packet.totals.regions,
            packet.totals.covered,
            packet.totals.uncovered
        ),
    ];
    for file in &packet.files {
        lines.push(format!(
            "- {} [{}; {}]: {}",
            file.path,
            file.change,
            file.coverage,
            file.regions
                .iter()
                .map(|impact| if impact.region.name.is_empty() {
                    "文件区域"
                } else {
                    &impact.region.name
                })
                .collect::<Vec<_>>()
                .join(", ")
        ));
        let docs = file
            .documents
            .iter()
            .take(3)
            .map(|doc| format!("{} §{} [{}]", doc.path, doc.heading, doc.lifecycle))
            .collect::<Vec<_>>();
        lines.push(format!(
            "  → {}",
            if docs.is_empty() {
                "无关联文档".to_string()
            } else {
                docs.join(", ")
            }
        ));
        if !file.decisions.is_empty() {
            lines.push(format!("  决策约束: {}", file.decisions.join(", ")));
        }
    }
    lines.push(format!(
        "锁预测漂移/迁移/断链: {}",
        packet.totals.drifting_anchors
    ));
    for state in packet.predicted_drift.iter().take(5) {
        lines.push(format!(
            "- {} `{}` [{}]",
            state.doc, state.anchor, state.status
        ));
    }
    lines.push(format!("无文档覆盖区域: {}", packet.totals.uncovered));
    for region in packet.uncovered_regions.iter().take(5) {
        lines.push(format!(
            "- {} (L{}–L{})",
            label(region),
            region.span_start,
            region.span_end
        ));
    }
    if !packet.truncated.is_empty() {
        lines.push(format!("省略计数: {:?}", packet.truncated));
    }
    lines.push(coverage.summary());
    lines.extend(hints.iter().map(|hint| format!("Hint: {hint}")));
    print_lines(lines);
}

pub(super) fn print_code_changes_summary(summary: &CodeChangesSummary) {
    println!("上个快照以来改动的代码区域：已覆盖 / 未覆盖 / 漂移");
    if summary.available {
        println!(
            "- 已覆盖 {} / 未覆盖 {} / 漂移 {}",
            summary.covered, summary.uncovered, summary.drifted
        );
        for area in summary.areas.iter().take(4) {
            println!(
                "- {}{} [{}]",
                super::short_line(&area.path, 140),
                if area.name.is_empty() {
                    String::new()
                } else {
                    format!("#{}", super::short_line(&area.name, 100))
                },
                area.status
            );
        }
        let omitted = summary.truncated + summary.areas.len().saturating_sub(4);
        if omitted > 0 {
            println!("- 另有 {} 个区域（计数已包含）", omitted);
        }
    }
    if let Some(hint) = summary.hint {
        println!("- {}", hint);
    }
}
