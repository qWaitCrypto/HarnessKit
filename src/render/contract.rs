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
        cjk + text.chars().count().saturating_sub(cjk).div_ceil(4)
    }
}

pub(crate) fn paginate_lines(
    lines: Vec<RenderLine>,
    options: &RenderOptions,
    command: &str,
) -> RenderedPage {
    // A related line belongs to the nearest preceding content line.  Keeping
    // these entries together is important for both readability and paging:
    // sorting all priority-1 lines after the content lines would detach a
    // document's evidence from its parent (especially in context --diff).
    let entries = group_lines(lines);
    let offset = options
        .cursor
        .as_deref()
        .and_then(decode_cursor)
        .unwrap_or_else(|| page_offset(&entries, options.page, options.budget));
    let offset = offset.min(entries.len());
    let mut selected = Vec::new();
    let mut used = 0usize;
    let mut index = offset;
    while index < entries.len() {
        let entry = &entries[index];
        let cost = entry
            .iter()
            .map(|line| token_cost(&line.text))
            .sum::<usize>();
        if !selected.is_empty() && used + cost > options.budget {
            break;
        }
        // Do not split an entry across pages.  An individual long entry may
        // exceed the approximate budget, but its parent and evidence remain
        // an intelligible unit.
        selected.extend(entry.iter().map(|line| line.text.clone()));
        used += cost;
        index += 1;
    }
    let omitted = entries.iter().skip(index).map(Vec::len).sum::<usize>();
    let next_cursor = (index < entries.len()).then(|| encode_cursor(index, command));
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

fn group_lines(lines: Vec<RenderLine>) -> Vec<Vec<RenderLine>> {
    let mut entries = Vec::<Vec<RenderLine>>::new();
    for line in lines {
        let continuation = line.priority == 1 || line.text.starts_with(char::is_whitespace);
        if entries.is_empty() || !continuation {
            entries.push(vec![line]);
        } else if let Some(entry) = entries.last_mut() {
            entry.push(line);
        }
    }
    entries
}

fn page_offset(entries: &[Vec<RenderLine>], page: usize, budget: usize) -> usize {
    let mut offset = 0usize;
    for _ in 1..page {
        let mut used = 0usize;
        while offset < entries.len() {
            let cost = entries[offset]
                .iter()
                .map(|line| token_cost(&line.text))
                .sum::<usize>();
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
                || term_lower.chars().count() >= 4
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
        let Some(prefix) = text.get(cursor..start) else {
            continue;
        };
        let Some(hit) = text.get(start..end) else {
            continue;
        };
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
    let fallback = if text.trim().is_empty() { fallback } else { "" };
    highlight(
        &preferred_excerpt(text, query, &[], fallback, max_chars),
        query,
    )
}

/// Prefer the actual rule or code reference within the selected section. Query
/// coverage still leads; headings are useful only when the prose has no hit or
/// constraint. Anchors are supplied by reverse context and related-code links.
pub(crate) fn preferred_excerpt(
    text: &str,
    query: &str,
    anchors: &[&str],
    fallback: &str,
    max_chars: usize,
) -> String {
    let terms = query_tokens_for_render(query);
    let mut best = None::<(String, (bool, usize, usize, usize))>;
    for sentence in sentences(text).into_iter().flat_map(fenced_candidates) {
        let lower = sentence.to_lowercase();
        let prose = !sentence.trim_start().starts_with('#');
        if !prose && !fallback.is_empty() {
            continue;
        }
        let count = terms
            .iter()
            .filter(|term| crate::query::matching_offset(&lower, term).is_some())
            .count();
        let constraints = if prose { constraint_score(&lower) } else { 0 };
        let anchored = usize::from(
            prose
                && (anchors
                    .iter()
                    .any(|anchor| !anchor.is_empty() && sentence.contains(anchor))
                    || sentence.split_whitespace().any(|word| {
                        word.contains('/')
                            && !word.contains("://")
                            && (word.contains('#')
                                || word.rsplit('/').next().unwrap_or("").contains('.'))
                    })),
        );
        let score = (
            prose && count > 0,
            if prose { count } else { 0 },
            constraints,
            anchored,
        );
        if (count > 0 || constraints > 0 || anchored > 0)
            && best
                .as_ref()
                .map(|(_, previous)| score > *previous)
                .unwrap_or(true)
        {
            best = Some((sentence, score));
        }
    }
    let chosen = best.map(|(sentence, _)| sentence).unwrap_or_else(|| {
        if !fallback.is_empty() {
            return fallback.to_string();
        }
        text.lines()
            .map(str::trim)
            .find(|line| !line.is_empty() && !line.starts_with("```"))
            .unwrap_or(fallback)
            .to_string()
    });
    let chars = chosen.chars().collect::<Vec<_>>();
    let first = terms
        .iter()
        .filter_map(|term| crate::query::matching_offset(&chosen.to_lowercase(), term))
        .min()
        .and_then(|offset| chosen.get(..offset))
        .map(|prefix| prefix.chars().count())
        .unwrap_or(0);
    let mut start = if first >= max_chars {
        first.saturating_sub(max_chars / 3)
    } else {
        0
    };
    while start > 0 && !chars[start - 1].is_whitespace() && chars[start - 1] != '`' {
        start -= 1;
    }
    let mut excerpt = compact(&chars[start..].iter().collect::<String>(), max_chars);
    if excerpt.matches('`').count() % 2 == 1 {
        if let Some(open) = excerpt.rfind('`') {
            // Do not leave a path or inline identifier dangling at the budget
            // boundary. A long inline span can still be shown with closed ticks.
            if open > 0 {
                excerpt.truncate(open);
                excerpt = format!("{}…", excerpt.trim_end());
            } else {
                excerpt = excerpt.trim_end_matches('…').to_string() + "`…";
            }
        }
    }
    if start > 0 {
        excerpt.insert(0, '…');
    }
    excerpt
}

pub(crate) fn constraint_score(text: &str) -> usize {
    let words = text
        .split(|ch: char| !ch.is_alphabetic())
        .collect::<BTreeSet<_>>();
    ["must", "never", "only", "not", "cannot", "shall"]
        .iter()
        .filter(|word| words.contains(**word))
        .count()
        + usize::from(words.contains("returns") && words.contains("caller"))
        + ["必须", "不得", "禁止", "只能"]
            .iter()
            .filter(|word| text.contains(**word))
            .count()
}

fn fenced_candidates(sentence: &str) -> impl Iterator<Item = String> + '_ {
    let lines = sentence.lines().map(str::trim).collect::<Vec<_>>();
    if lines.iter().any(|line| line.starts_with("```")) {
        let candidates = lines
            .into_iter()
            .filter(|line| !line.is_empty() && !line.starts_with("```"))
            .map(str::to_string)
            .collect::<Vec<_>>();
        candidates.into_iter()
    } else {
        std::iter::once(sentence.trim().to_string())
            .collect::<Vec<_>>()
            .into_iter()
    }
}

fn sentences(text: &str) -> Vec<&str> {
    let mut start = 0;
    let mut backticks = 0;
    let mut chars = text.char_indices().peekable();
    let mut sentences = Vec::new();
    while let Some((offset, ch)) = chars.next() {
        if ch == '`' {
            let mut run = 1;
            while chars.peek().map(|(_, ch)| *ch == '`').unwrap_or(false) {
                chars.next();
                run += 1;
            }
            if backticks == 0 {
                backticks = run;
            } else if backticks == run {
                backticks = 0;
            }
            continue;
        }
        let newline_boundary = ch == '\n' && {
            let before = text[..offset].rsplit('\n').next().unwrap_or("").trim();
            let after = text[offset + 1..].lines().next().unwrap_or("").trim();
            before.is_empty()
                || after.is_empty()
                || before.starts_with('#')
                || after.starts_with('#')
                || after.starts_with("- ")
                || after.starts_with("* ")
                || after.starts_with("+ ")
                || after.starts_with("```")
        };
        let boundary = backticks == 0
            && (newline_boundary
                || matches!(ch, '。' | '！' | '？')
                || (matches!(ch, '.' | '!' | '?')
                    && chars
                        .peek()
                        .map(|(_, next)| next.is_whitespace())
                        .unwrap_or(true)));
        if boundary {
            let end = offset + ch.len_utf8();
            sentences.push(&text[start..end]);
            start = end;
        }
    }
    if start < text.len() {
        sentences.push(&text[start..]);
    }
    sentences
}

pub(crate) fn edge_badge(confidence: &str) -> &'static str {
    if confidence == "INFERRED" {
        "~"
    } else {
        ""
    }
}

/// The surrounding result already identifies this file; keep the actual
/// symbol name in prose without repeating its full anchor path.
pub(crate) fn local_anchor_names(text: &str, path: &str) -> String {
    text.split('`')
        .enumerate()
        .map(|(index, part)| {
            let part = if index % 2 == 1 {
                part.split_once('#')
                    .filter(|(anchor_path, _)| *anchor_path == path)
                    .map(|(_, name)| name)
                    .unwrap_or(part)
            } else {
                part
            };
            if index == 0 {
                part.to_string()
            } else {
                format!("`{part}")
            }
        })
        .collect()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kwic_keeps_paths_decimal_points_and_inline_code_inside_sentences() {
        let text = "Usage log writes `scripts/x.sh` and `src/usage.rs#record` using version 1.2. It then returns.";
        let excerpt = kwic_excerpt(text, "usage log", "", 160);
        let plain = excerpt.replace('«', "").replace('»', "");
        assert!(
            plain.contains("`scripts/x.sh` and `src/usage.rs#record` using version 1.2."),
            "{excerpt}"
        );
        assert!(!excerpt.contains("It then returns"));
    }

    #[test]
    fn kwic_keeps_a_late_hit_and_closes_or_omits_truncated_inline_spans() {
        let text = format!(
            "{} usage log records the operation before returning.",
            "Long preamble ".repeat(30)
        );
        let excerpt = kwic_excerpt(&text, "usage log", "", 160);
        assert!(excerpt.contains("«usage» «log»"), "{excerpt}");
        for text in [
            format!("Read `src/{}.rs` before logging.", "long".repeat(70)),
            format!("`src/{}.rs` records logs.", "long".repeat(70)),
        ] {
            let excerpt = kwic_excerpt(&text, "Read", "", 160);
            assert_eq!(excerpt.matches('`').count() % 2, 0, "{excerpt}");
        }
    }

    #[test]
    fn kwic_joins_soft_wrapping_and_uses_the_same_word_boundaries_as_retrieval() {
        let text = "Usage logic chooses a branch.\n\nThe usage log writes through\n`scripts/x.sh` before returning. Another sentence follows.";
        let excerpt = kwic_excerpt(text, "usage log", "", 160);
        assert!(
            excerpt.contains("«usage» «log» writes through `scripts/x.sh` before returning."),
            "{excerpt}"
        );
        assert!(
            !excerpt.contains("logic") && !excerpt.contains("Another sentence"),
            "{excerpt}"
        );
    }

    #[test]
    fn kwic_skips_fenced_markers_and_keeps_only_the_matching_code_line() {
        let text = "## Local usage\n\n```bash\nexport HARNESSKIT_USAGE_LOG=off # Disable logging\nharnesskit doctor --usage --days 30\n```\n";
        let excerpt = kwic_excerpt(text, "usage log", "", 160);
        assert_eq!(
            excerpt,
            "export HARNESSKIT_«USAGE»_«LOG»=off # Disable logging"
        );
        assert!(!excerpt.contains("```"));
        assert!(!excerpt.contains("harnesskit doctor"));
    }

    #[test]
    fn pagination_keeps_related_lines_with_their_parent_on_every_page() {
        let options = RenderOptions {
            budget: 5,
            ..RenderOptions::default()
        };
        let first = paginate_lines(
            vec![
                RenderLine::content("- first file"),
                RenderLine::related("  → first document"),
                RenderLine::content("- second file"),
                RenderLine::related("  → second document"),
            ],
            &options,
            "context --diff",
        );
        assert_eq!(first.lines[0], "- first file");
        assert_eq!(first.lines[1], "  → first document");
        assert!(first.lines.iter().all(|line| !line.contains("second")));

        let second = paginate_lines(
            vec![
                RenderLine::content("- first file"),
                RenderLine::related("  → first document"),
                RenderLine::content("- second file"),
                RenderLine::related("  → second document"),
            ],
            &RenderOptions {
                budget: 5,
                page: 2,
                ..RenderOptions::default()
            },
            "context --diff",
        );
        assert_eq!(second.lines[0], "- second file");
        assert_eq!(second.lines[1], "  → second document");
    }
}
