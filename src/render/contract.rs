//! Shared W5b output contract primitives.
//!
//! Renderers build content lines first and attach metadata only after the
//! content is selected.  Keeping budgeting, cursors and badges here makes the
//! text and JSON entry points agree without making the CLI's data model carry
//! presentation state.

use std::collections::BTreeSet;

use crate::query::query_tokens_for_render;

pub(crate) const DEFAULT_BUDGET: usize = 1500;
const CURSOR_PREFIX: &str = "hk1";

#[derive(Clone, Debug)]
pub(crate) struct RenderOptions {
    pub(crate) budget: usize,
    pub(crate) expand: Option<usize>,
    pub(crate) page: usize,
    pub(crate) cursor: Option<String>,
    pub(crate) full: bool,
    pub(crate) verbose: bool,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self {
            budget: DEFAULT_BUDGET,
            expand: None,
            page: 1,
            cursor: None,
            full: false,
            verbose: false,
        }
    }
}

impl RenderOptions {
    pub(crate) fn from_values(
        budget: Option<u32>,
        expand: Option<u32>,
        page: Option<u32>,
        cursor: Option<String>,
        full: bool,
        verbose: bool,
    ) -> Self {
        Self {
            budget: budget.map(|value| value as usize).unwrap_or(DEFAULT_BUDGET),
            expand: expand.map(|value| value as usize),
            page: page.map(|value| value as usize).unwrap_or(1).max(1),
            cursor,
            full,
            verbose,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct RenderLine {
    pub(crate) text: String,
    /// Lower values are filled first when a caller supplies separate groups.
    pub(crate) priority: u8,
}

impl RenderLine {
    pub(crate) fn content(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            priority: 0,
        }
    }

    pub(crate) fn related(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            priority: 1,
        }
    }

    pub(crate) fn hint(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            priority: 2,
        }
    }
}

#[allow(dead_code)]
#[derive(Clone, Debug, Default)]
pub(crate) struct RenderedPage {
    pub(crate) lines: Vec<String>,
    pub(crate) next_cursor: Option<String>,
    pub(crate) truncated: usize,
}

pub(crate) fn token_cost(text: &str) -> usize {
    if text.is_empty() {
        1
    } else {
        // The contract treats CJK characters as one token and approximates
        // Latin text as four characters per token.
        let cjk = text.chars().filter(|ch| is_cjk(*ch)).count();
        cjk + (text.chars().count().saturating_sub(cjk) + 3) / 4
    }
}

pub(crate) fn paginate_lines(
    mut lines: Vec<RenderLine>,
    options: &RenderOptions,
    command: &str,
) -> RenderedPage {
    // Callers append sections in priority order. Stable sorting keeps a caller
    // supplied main/related/hint order while allowing future renderers to mix
    // section builders safely.
    lines.sort_by_key(|line| line.priority);
    let offset = options
        .cursor
        .as_deref()
        .and_then(decode_cursor)
        .unwrap_or_else(|| page_offset(&lines, options.page, options.budget));
    let offset = offset.min(lines.len());
    let mut selected = Vec::new();
    let mut used = 0usize;
    let mut index = offset;
    while index < lines.len() {
        let line = &lines[index].text;
        let cost = token_cost(line);
        if !selected.is_empty() && used + cost > options.budget {
            break;
        }
        let text = if selected.is_empty() && cost > options.budget {
            truncate_to_budget(line, options.budget)
        } else {
            line.clone()
        };
        selected.push(text);
        used += token_cost(selected.last().expect("selected line"));
        index += 1;
    }
    let omitted = lines.len().saturating_sub(index);
    let next_cursor = (index < lines.len()).then(|| encode_cursor(index, command));
    if omitted > 0 {
        let page = options.page.saturating_add(1);
        let next = next_cursor
            .clone()
            .unwrap_or_else(|| encode_cursor(index, command));
        selected.push(format!(
            "… {} more (harnesskit {} --page {} --cursor {})",
            omitted, command, page, next
        ));
    }
    RenderedPage {
        lines: selected,
        next_cursor,
        truncated: omitted,
    }
}

fn page_offset(lines: &[RenderLine], page: usize, budget: usize) -> usize {
    let mut offset = 0usize;
    for _ in 1..page {
        let mut used = 0usize;
        while offset < lines.len() {
            let cost = token_cost(&lines[offset].text);
            if used > 0 && used + cost > budget {
                break;
            }
            used += cost;
            offset += 1;
        }
    }
    offset
}

fn encode_cursor(offset: usize, command: &str) -> String {
    let digest = blake3::hash(command.as_bytes()).to_hex().to_string();
    format!("{CURSOR_PREFIX}-{}-{}", &digest[..10], offset)
}

fn decode_cursor(cursor: &str) -> Option<usize> {
    let mut parts = cursor.split('-');
    if parts.next()? != CURSOR_PREFIX {
        return None;
    }
    let _digest = parts.next()?;
    parts.next()?.parse().ok()
}

pub(crate) fn cursor_for_offset(offset: usize, command: &str) -> String {
    encode_cursor(offset, command)
}

pub(crate) fn cursor_offset(cursor: &str) -> Option<usize> {
    decode_cursor(cursor)
}

pub(crate) fn short_hint(text: &str) -> String {
    compact(text, 160)
}

pub(crate) fn compact(text: &str, max_chars: usize) -> String {
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = compact.chars();
    let value = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        format!("{value}…")
    } else {
        value
    }
}

pub(crate) fn truncate_to_budget(text: &str, budget: usize) -> String {
    if token_cost(text) <= budget {
        return text.to_string();
    }
    let max_chars = budget.saturating_mul(4).max(1);
    let value = text.chars().take(max_chars).collect::<String>();
    format!("{}…", value.trim_end())
}

pub(crate) fn is_cjk(ch: char) -> bool {
    matches!(
        ch as u32,
        0x2e80..=0x9fff
            | 0xac00..=0xd7af
            | 0xf900..=0xfaff
            | 0x20000..=0x323af
    )
}

pub(crate) fn highlight(text: &str, query: &str) -> String {
    let terms = query_tokens_for_render(query);
    highlight_terms(text, &terms)
}

pub(crate) fn highlight_terms(text: &str, terms: &[String]) -> String {
    if terms.is_empty() || text.is_empty() {
        return text.to_string();
    }
    let lower = text.to_lowercase();
    let mut matches = Vec::<(usize, usize)>::new();
    for term in terms {
        let term_lower = term.to_lowercase();
        if term_lower.is_empty() {
            continue;
        }
        for (start, _) in lower.match_indices(&term_lower) {
            let end = start + term_lower.len();
            let boundary = is_cjk(term_lower.chars().next().unwrap_or(' '))
                || (!lower[..start]
                    .chars()
                    .next_back()
                    .map(char::is_alphanumeric)
                    .unwrap_or(false)
                    && !lower[end..]
                        .chars()
                        .next()
                        .map(char::is_alphanumeric)
                        .unwrap_or(false));
            if !boundary {
                continue;
            }
            if !matches.iter().any(|(a, b)| start < *b && end > *a) {
                matches.push((start, end));
            }
        }
    }
    matches.sort_by_key(|(start, _)| *start);
    let mut out = String::new();
    let mut cursor = 0usize;
    for (start, end) in matches {
        if start < cursor {
            continue;
        }
        let Some(prefix) = text.get(cursor..start) else { continue };
        let Some(hit) = text.get(start..end) else { continue };
        out.push_str(prefix);
        out.push('«');
        out.push_str(hit);
        out.push('»');
        cursor = end;
    }
    out.push_str(text.get(cursor..).unwrap_or_default());
    out
}

pub(crate) fn kwic_excerpt(text: &str, query: &str, fallback: &str, max_chars: usize) -> String {
    let terms = query_tokens_for_render(query);
    let mut best = None::<(&str, usize)>;
    for sentence in text
        .split(|ch| matches!(ch, '\n' | '.' | '!' | '?' | '。' | '！' | '？'))
        .map(str::trim)
        .filter(|part| !part.is_empty())
    {
        let lower = sentence.to_lowercase();
        let count = terms
            .iter()
            .filter(|term| lower.contains(&term.to_lowercase()))
            .count();
        if count > 0 && best.map(|(_, score)| count > score).unwrap_or(true) {
            best = Some((sentence, count));
        }
    }
    let chosen = best.map(|(sentence, _)| sentence).unwrap_or_else(|| {
        text.lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or(fallback)
    });
    compact(&highlight(chosen, query), max_chars)
}

pub(crate) fn edge_badge(confidence: &str) -> &'static str {
    if confidence == "INFERRED" {
        "~"
    } else {
        ""
    }
}

pub(crate) fn freshness_badge(status: &str) -> &'static str {
    match status {
        "drifted" | "stale" => "⚠drift",
        "broken" => "✗broken",
        "moved" => "✗broken",
        "ambiguous" => "⚠drift",
        "historical" | "superseded" => "superseded",
        _ => "",
    }
}

pub(crate) fn merge_names(names: impl IntoIterator<Item = String>, limit: usize) -> String {
    let mut seen = BTreeSet::new();
    let mut values = names
        .into_iter()
        .filter(|name| !name.is_empty())
        .filter(|name| seen.insert(name.clone()))
        .collect::<Vec<_>>();
    values.sort();
    let omitted = values.len().saturating_sub(limit);
    values.truncate(limit);
    let mut out = values.join(", ");
    if omitted > 0 {
        if !out.is_empty() {
            out.push_str(", ");
        }
        out.push_str(&format!("… {} more", omitted));
    }
    out
}

