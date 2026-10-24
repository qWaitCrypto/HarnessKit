use pulldown_cmark::{Event, Parser, Tag, TagEnd};

use crate::parse::split_frontmatter;

pub(super) struct Section {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) span_start: usize,
    pub(super) span_end: usize,
    pub(super) fingerprint: String,
}

pub(super) struct InlineCode {
    pub(super) src: String,
    pub(super) section: String,
    pub(super) line: usize,
    pub(super) column: usize,
    pub(super) text: String,
    pub(super) prose: bool,
}

pub(super) struct Document {
    pub(super) sections: Vec<Section>,
    pub(super) spans: Vec<InlineCode>,
}

// CommonMark supplies inline-code and prose events with source offsets. Fenced
// or indented examples, HTML and link labels never become prose anchors.
pub(super) fn parse(path: &str, text: &str) -> Document {
    let text = text.replace("\r\n", "\n");
    let (_, body) = split_frontmatter(&text);
    let body_offset = text.len() - body.len();
    let line_offsets = std::iter::once(0)
        .chain(text.match_indices('\n').map(|(index, _)| index + 1))
        .collect::<Vec<_>>();
    let last_line = text.lines().count().max(1);
    let mut sections = Vec::<Section>::new();
    let mut open_sections = Vec::<(usize, usize, usize)>::new();
    let mut heading = None;
    let mut raw_spans = Vec::new();
    let mut in_code_block = false;
    let mut link_depth = 0;
    for (event, range) in Parser::new(body).into_offset_iter() {
        let offset = body_offset + range.start;
        let line = line_offsets.partition_point(|start| *start <= offset);
        match event {
            Event::Start(Tag::CodeBlock(_)) => in_code_block = true,
            Event::End(TagEnd::CodeBlock) => in_code_block = false,
            Event::Start(Tag::Link { .. } | Tag::Image { .. }) => link_depth += 1,
            Event::End(TagEnd::Link | TagEnd::Image) => link_depth -= 1,
            Event::Start(Tag::Heading { level, .. }) => {
                while open_sections
                    .last()
                    .map(|(_, open_level, _)| *open_level >= level as usize)
                    .unwrap_or(false)
                {
                    let (index, _, start) = open_sections.pop().unwrap();
                    sections[index].span_end =
                        line.saturating_sub(1).max(sections[index].span_start);
                    sections[index].fingerprint = fingerprint(&text[start..offset]);
                }
                let index = sections.len();
                sections.push(Section {
                    id: format!("section:{path}#L{line}"),
                    name: String::new(),
                    span_start: line,
                    span_end: last_line,
                    fingerprint: String::new(),
                });
                open_sections.push((index, level as usize, offset));
                heading = Some(index);
            }
            Event::End(TagEnd::Heading(_)) => heading = None,
            Event::Text(value) => {
                if let Some(index) = heading {
                    sections[index].name.push_str(&value);
                }
                if !in_code_block && link_depth == 0 {
                    let section = sections.len().checked_sub(1);
                    for (candidate, relative_offset) in prose_path_candidates(&body[range]) {
                        let candidate_offset = offset + relative_offset;
                        if text[..candidate_offset].ends_with(['`', '\\'])
                            || text[candidate_offset + candidate.len()..].starts_with(['`', '\\'])
                        {
                            continue;
                        }
                        let candidate_line = line_offsets
                            .partition_point(|start| *start <= candidate_offset);
                        let line_start = line_offsets[candidate_line.saturating_sub(1)];
                        let column = text[line_start..candidate_offset].chars().count() + 1;
                        raw_spans.push((section, candidate_line, column, candidate, true));
                    }
                }
            }
            Event::SoftBreak | Event::HardBreak => {
                if let Some(index) = heading {
                    sections[index].name.push(' ');
                }
            }
            Event::Code(value) => {
                if let Some(index) = heading {
                    sections[index].name.push_str(&value);
                }
                raw_spans.push((
                    sections.len().checked_sub(1),
                    line,
                    text[line_offsets[line - 1]..offset].chars().count() + 1,
                    value.into_string(),
                    false,
                ));
            }
            _ => {}
        }
    }
    for (index, _, start) in open_sections {
        sections[index].fingerprint = fingerprint(&text[start..]);
    }
    let spans = raw_spans
        .into_iter()
        .map(|(index, line, column, text, prose)| InlineCode {
            src: index
                .map(|index| sections[index].id.clone())
                .unwrap_or_else(|| format!("doc:{path}")),
            section: index
                .map(|index| sections[index].name.clone())
                .unwrap_or_default(),
            line,
            column,
            text,
            prose,
        })
        .collect();
    Document { sections, spans }
}

/// Resolution later requires an indexed file, so nonexistent paths and ordinary
/// dotted words stay prose. Keep URL/command tokens whole to avoid extracting a
/// valid repository path from inside one of them.
fn prose_path_candidates(text: &str) -> Vec<(String, usize)> {
    let mut candidates = Vec::new();
    let mut offset = 0;
    for token in text.split_inclusive(|ch: char| {
        ch.is_whitespace() || matches!(ch, '(' | ')' | '[' | ']' | '"' | '\'' | ',' | ';' | '。' | '，' | '；' | '：' | '！' | '？')
    }) {
        let candidate = token.trim().trim_end_matches(|ch: char| {
            matches!(ch, '(' | ')' | '[' | ']' | '"' | '\'' | ',' | ';' | ':' | '.' | '!' | '?' | '。' | '，' | '；' | '：' | '！' | '？')
        });
        let token_offset = offset;
        offset += token.len();
        if candidate.len() < 3
            || candidate.contains(['`', '\\', '=', '<', '>', '{', '}', '?', '@'])
            || candidate.contains("://") || candidate.starts_with('/')
        {
            continue;
        }
        let path = candidate.split_once('#').map(|(path, _)| path).unwrap_or(candidate);
        let filename = path.contains('/') || path.contains('.')
            || path.chars().next().map(char::is_uppercase).unwrap_or(false);
        if path.ends_with('/') || path.contains(':') || !filename {
            continue;
        }
        candidates.push((candidate.to_string(), token_offset));
    }
    candidates
}

fn fingerprint(text: &str) -> String {
    blake3::hash(text.as_bytes()).to_hex()[..12].to_string()
}
