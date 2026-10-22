use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::fs_utils::normalize_path;

pub(crate) struct ParsedDoc {
    pub(crate) frontmatter: BTreeMap<String, String>,
    pub(crate) title: Option<String>,
    pub(crate) summary: String,
    pub(crate) body_text: String,
    pub(crate) headings: Vec<String>,
    pub(crate) links: Vec<String>,
    pub(crate) path_mentions: Vec<String>,
    pub(crate) scope_paths: Vec<String>,
    pub(crate) supersedes: Vec<String>,
}

pub(crate) fn parse_doc_text(path: &str, text: &str) -> ParsedDoc {
    let normalized_text;
    let text = if text.contains("\r\n") {
        normalized_text = text.replace("\r\n", "\n");
        normalized_text.as_str()
    } else {
        text
    };
    let (frontmatter, body) = split_frontmatter(text);
    let frontmatter_map = frontmatter.map(parse_frontmatter).unwrap_or_default();
    let scope_paths = frontmatter.map(parse_scope_paths).unwrap_or_else(Vec::new);
    let supersedes = frontmatter.map(parse_supersedes).unwrap_or_else(Vec::new);

    let mut title = None;
    let mut headings = Vec::new();
    let mut links = Vec::new();
    let path_mentions = extract_inline_code_path_mentions(body);
    let mut in_fence = false;
    for line in body.lines() {
        let is_fence_line = is_backtick_fence_line(line);
        let line_in_fence = in_fence;
        if is_fence_line {
            in_fence = !in_fence;
        }
        if !line_in_fence && !is_fence_line {
            if let Some((level, heading)) = parse_heading(line) {
                if level == 1 && title.is_none() {
                    title = Some(heading.clone());
                }
                headings.push(heading);
            }
            for (_text, target) in extract_markdown_links(line) {
                if let Some(normalized) = normalize_link(path, &target) {
                    links.push(normalized);
                }
            }
        }
    }

    ParsedDoc {
        frontmatter: frontmatter_map,
        title,
        summary: extract_summary(body),
        body_text: body.to_string(),
        headings,
        links,
        path_mentions,
        scope_paths,
        supersedes,
    }
}

fn split_frontmatter(text: &str) -> (Option<&str>, &str) {
    if !text.starts_with("---\n") {
        return (None, text);
    }
    if let Some(end) = text[4..].find("\n---") {
        let fm_end = 4 + end;
        let after_marker = fm_end + 4;
        if after_marker == text.len() {
            (Some(&text[4..fm_end]), "")
        } else if text[after_marker..].starts_with('\n') {
            (Some(&text[4..fm_end]), &text[after_marker + 1..])
        } else {
            (None, text)
        }
    } else {
        (None, text)
    }
}

fn parse_frontmatter(raw: &str) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with("- ") {
            continue;
        }
        if let Some((key, value)) = trimmed.split_once(':') {
            let value = value.trim().trim_matches('"').trim_matches('\'');
            if !value.is_empty() {
                map.insert(key.trim().to_string(), value.to_string());
            }
        }
    }
    map
}

fn parse_scope_paths(raw: &str) -> Vec<String> {
    parse_frontmatter_list(raw, &["scope_paths", "source_paths"])
}

fn parse_supersedes(raw: &str) -> Vec<String> {
    let mut values = parse_frontmatter_list(raw, &["supersedes"]);
    for value in &mut values {
        let normalized = normalize_path(Path::new(value));
        *value = normalized;
    }
    values.sort();
    values.dedup();
    values
}

fn parse_frontmatter_list(raw: &str, keys: &[&str]) -> Vec<String> {
    let mut values = Vec::new();
    let mut active_key: Option<String> = None;

    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = trimmed.split_once(':') {
            let key = key.trim();
            if keys.contains(&key) {
                active_key = Some(key.to_string());
                let value = value.trim();
                if value.starts_with('[') && value.ends_with(']') {
                    for item in value.trim_matches(&['[', ']'][..]).split(',') {
                        push_list_value(&mut values, item);
                    }
                } else if !value.is_empty() {
                    push_list_value(&mut values, value);
                }
                continue;
            }
            active_key = None;
        } else if trimmed.starts_with("- ") && active_key.is_some() {
            push_list_value(&mut values, trimmed.trim_start_matches("- "));
        }
    }

    values.sort();
    values.dedup();
    values
}

fn push_list_value(values: &mut Vec<String>, raw: &str) {
    let value = raw
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
        .trim_end_matches('/')
        .to_string();
    if !value.is_empty() {
        values.push(value);
    }
}

pub(crate) fn parse_heading(line: &str) -> Option<(u32, String)> {
    let trimmed = line.trim_start();
    let hashes = trimmed.chars().take_while(|c| *c == '#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let rest = trimmed[hashes..].trim();
    if rest.is_empty() {
        None
    } else {
        Some((hashes as u32, rest.trim_matches('#').trim().to_string()))
    }
}

fn leading_backtick_run(line: &str) -> Option<usize> {
    let trimmed = line.trim_start();
    let run = trimmed.chars().take_while(|ch| *ch == '`').count();
    if run > 0 {
        Some(run)
    } else {
        None
    }
}

fn is_backtick_fence_line(line: &str) -> bool {
    leading_backtick_run(line)
        .map(|run| run >= 3)
        .unwrap_or(false)
}

fn extract_markdown_links(line: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'[' {
            if let Some(close) = line[i + 1..].find(']') {
                let close_idx = i + 1 + close;
                if line[close_idx..].starts_with("](") {
                    if let Some(end) = line[close_idx + 2..].find(')') {
                        let text = &line[i + 1..close_idx];
                        let target = &line[close_idx + 2..close_idx + 2 + end];
                        out.push((text.to_string(), target.to_string()));
                        i = close_idx + 3 + end;
                        continue;
                    }
                }
            }
        }
        i += 1;
    }
    out
}

fn normalize_link(src_doc: &str, raw: &str) -> Option<String> {
    if raw.starts_with("http://")
        || raw.starts_with("https://")
        || raw.starts_with("mailto:")
        || raw.starts_with('#')
    {
        return None;
    }
    let target = raw.split('#').next().unwrap_or(raw).trim();
    if target.is_empty() {
        return None;
    }
    let joined = if target.starts_with('/') {
        PathBuf::from(target.trim_start_matches('/'))
    } else {
        Path::new(src_doc)
            .parent()
            .unwrap_or_else(|| Path::new(""))
            .join(target)
    };
    Some(normalize_path(&joined))
}

fn extract_summary(body: &str) -> String {
    let mut paragraph = Vec::new();
    let mut in_fence = false;
    for line in body.lines() {
        let line_in_fence = in_fence;
        let is_fence_line = is_backtick_fence_line(line);
        if is_fence_line {
            in_fence = !in_fence;
        }
        if line_in_fence || is_fence_line {
            if !paragraph.is_empty() {
                break;
            }
            continue;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if !paragraph.is_empty() {
                break;
            }
            continue;
        }
        if trimmed.starts_with('#') || trimmed.starts_with("- ") {
            if !paragraph.is_empty() {
                break;
            }
            continue;
        }
        paragraph.push(trimmed);
    }
    paragraph.join(" ")
}

pub(crate) fn extract_inline_code_path_mentions(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut mentions = Vec::new();
    let mut seen = BTreeSet::new();
    let mut current = String::new();
    let mut in_inline_code = false;
    let mut in_fence = false;
    let mut index = 0;

    while index < chars.len() {
        if chars[index] == '`' {
            let mut run = 1;
            while index + run < chars.len() && chars[index + run] == '`' {
                run += 1;
            }

            if run >= 3 && is_line_leading_backtick_run(&chars, index) {
                if !in_inline_code {
                    in_fence = !in_fence;
                } else {
                    for _ in 0..run {
                        current.push('`');
                    }
                }
                index += run;
                continue;
            }

            if run == 1 && !in_fence {
                if in_inline_code {
                    for candidate in extract_path_candidates(&current) {
                        if seen.insert(candidate.clone()) {
                            mentions.push(candidate);
                        }
                    }
                    current.clear();
                    in_inline_code = false;
                } else {
                    in_inline_code = true;
                }
                index += 1;
                continue;
            }

            if in_inline_code {
                for _ in 0..run {
                    current.push('`');
                }
            }
            index += run;
            continue;
        }

        if in_inline_code {
            current.push(chars[index]);
        }
        index += 1;
    }

    mentions
}

fn is_line_leading_backtick_run(chars: &[char], index: usize) -> bool {
    let line_start = chars[..index]
        .iter()
        .rposition(|ch| *ch == '\n')
        .map(|position| position + 1)
        .unwrap_or(0);
    chars[line_start..index].iter().all(|ch| ch.is_whitespace())
}

fn extract_path_candidates(text: &str) -> Vec<String> {
    let prefixes = [
        "docs/",
        "src/",
        "tests/",
        "test/",
        "crates/",
        "packages/",
        "AGENTS.md",
        "CLAUDE.md",
        "ARCHITECTURE.md",
        "README.md",
    ];
    let mut paths = Vec::new();
    let mut seen = BTreeSet::new();
    for token in text.split(|c: char| {
        c.is_whitespace() || matches!(c, '`' | '"' | '\'' | '(' | ')' | '[' | ']' | ',' | ';')
    }) {
        let normalized = token
            .trim()
            .trim_matches(|c: char| matches!(c, '.' | ':' | ',' | ')' | '(' | '"' | '\'' | '`'))
            .trim_end_matches('/')
            .trim_start_matches("./")
            .to_string();
        if normalized.len() < 3 {
            continue;
        }
        if prefixes.iter().any(|prefix| normalized.starts_with(prefix))
            && seen.insert(normalized.clone())
        {
            paths.push(normalized);
        }
    }
    paths
}
