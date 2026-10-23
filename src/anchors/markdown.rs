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
}

pub(super) struct Document {
    pub(super) sections: Vec<Section>,
    pub(super) spans: Vec<InlineCode>,
}

// CommonMark supplies real inline-code events and source offsets. In particular,
// fenced/indented examples, escaped backticks and HTML are not code anchors.
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
    for (event, range) in Parser::new(body).into_offset_iter() {
        let offset = body_offset + range.start;
        let line = line_offsets.partition_point(|start| *start <= offset);
        match event {
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
        .map(|(index, line, column, text)| InlineCode {
            src: index
                .map(|index| sections[index].id.clone())
                .unwrap_or_else(|| format!("doc:{path}")),
            section: index
                .map(|index| sections[index].name.clone())
                .unwrap_or_default(),
            line,
            column,
            text,
        })
        .collect();
    Document { sections, spans }
}

fn fingerprint(text: &str) -> String {
    blake3::hash(text.as_bytes()).to_hex()[..12].to_string()
}
