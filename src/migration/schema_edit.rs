//! Edits for the small schema grammar supported by `parse_schema`.
//!
//! Keep source bytes outside the targeted values/keys and inserted lines:
//! serializing a Schema would discard comments, quoting and field order.

use std::collections::BTreeMap;
use std::ops::Range;

use crate::schema::{parse_schema, strip_schema_comment, Schema};
use crate::Result;

use super::{missing_frontmatter_fields, CURRENT_SCHEMA_VERSION};

struct Line<'a> {
    start: usize,
    end: usize,
    code: &'a str,
    indent: usize,
}

impl Line<'_> {
    fn key(&self) -> Option<&str> {
        self.code
            .trim_start()
            .split_once(':')
            .map(|(key, _)| key.trim())
    }

    fn value_start(&self) -> usize {
        let colon = self.code.find(':').expect("schema key/value");
        colon + 1 + self.code[colon + 1..].len() - self.code[colon + 1..].trim_start().len()
    }
}

fn block(lines: &[Line<'_>], header: usize) -> Range<usize> {
    let end = (header + 1..lines.len())
        .find(|index| {
            !lines[*index].code.is_empty() && lines[*index].indent <= lines[header].indent
        })
        .unwrap_or(lines.len());
    header + 1..end
}

fn insertion_offset(lines: &[Line<'_>], header: usize) -> usize {
    block(lines, header)
        .rev()
        .find(|index| !lines[*index].code.is_empty())
        .map(|index| lines[index].end)
        .unwrap_or(lines[header].end)
}

pub(super) fn edit_schema(
    raw: &str,
    schema: &Schema,
    upgrade: bool,
    patterns: &[(String, String)],
) -> Result<String> {
    let newline = if raw.contains("\r\n") { "\r\n" } else { "\n" };
    let mut offset = 0;
    let lines = raw
        .split_inclusive('\n')
        .map(|text| {
            let start = offset;
            offset += text.len();
            let code = strip_schema_comment(text.trim_end_matches(['\r', '\n'])).trim_end();
            Line {
                start,
                end: offset,
                code,
                indent: code.len() - code.trim_start_matches(' ').len(),
            }
        })
        .collect::<Vec<_>>();
    let section = |name: &str| {
        lines
            .iter()
            .position(|line| line.indent == 0 && line.code == format!("{}:", name))
    };
    let mut edits = Vec::<(Range<usize>, String)>::new();
    let mut additions = BTreeMap::<usize, Vec<String>>::new();

    if upgrade {
        if let Some(line) = lines
            .iter()
            .find(|line| line.indent == 0 && line.key() == Some("schema_version"))
        {
            if schema.schema_version < CURRENT_SCHEMA_VERSION {
                edits.push((
                    line.start + line.value_start()..line.start + line.code.len(),
                    CURRENT_SCHEMA_VERSION.to_string(),
                ));
            }
        } else {
            additions
                .entry(0)
                .or_default()
                .push(format!("schema_version: {}", CURRENT_SCHEMA_VERSION));
        }

        let mut rules = Vec::new();
        if schema.rules.entrypoint_max_lines.is_none() {
            rules.push("  entrypoint_max_lines: 120".to_string());
        }
        if schema.rules.stale_active_plan_days.is_none() {
            rules.push("  stale_active_plan_days: 30".to_string());
        }
        if !rules.is_empty() {
            if let Some(header) = section("rules") {
                additions
                    .entry(insertion_offset(&lines, header))
                    .or_default()
                    .extend(rules);
            } else {
                let added = additions.entry(raw.len()).or_default();
                added.push("rules:".to_string());
                added.extend(rules);
            }
        }

        if let Some(header) = section("suppressions") {
            for index in block(&lines, header) {
                let line = &lines[index];
                let code = line
                    .code
                    .trim_start()
                    .strip_prefix("- ")
                    .unwrap_or(line.code.trim_start());
                if code.split_once(':').map(|(key, _)| key.trim()) == Some("rule_id") {
                    let start =
                        line.start + line.code.find("rule_id").expect("legacy suppression key");
                    edits.push((start..start + "rule_id".len(), "rule".to_string()));
                }
            }
        }

        if let Some(header) = section("doc_collections") {
            for index in block(&lines, header).filter(|index| lines[*index].indent == 2) {
                let Some(collection) = schema
                    .doc_collections
                    .iter()
                    .find(|collection| lines[index].code.trim() == format!("{}:", collection.name))
                else {
                    continue;
                };
                let missing = missing_frontmatter_fields(collection);
                if missing.is_empty() {
                    continue;
                }
                if let Some(field) = block(&lines, index)
                    .find(|field| lines[*field].key() == Some("allowed_frontmatter"))
                {
                    let line = &lines[field];
                    let value_start = line.value_start();
                    let value = &line.code[value_start..];
                    if value.starts_with('[') && value.ends_with(']') {
                        let before_close = line.code[..line.code.len() - 1].trim_end();
                        let separator = if before_close.ends_with('[') {
                            ""
                        } else if before_close.ends_with(',') {
                            " "
                        } else {
                            ", "
                        };
                        let start = line.start + before_close.len();
                        edits.push((start..start, format!("{}{}", separator, missing.join(", "))));
                    } else if value.is_empty() {
                        let start = line.start + value_start;
                        edits.push((start..start, format!(" [{}]", missing.join(", "))));
                    } else {
                        let start = line.start + line.code.len();
                        edits.push((start..start, format!(", {}", missing.join(", "))));
                    }
                } else {
                    additions
                        .entry(insertion_offset(&lines, index))
                        .or_default()
                        .push(format!("    allowed_frontmatter: [{}]", missing.join(", ")));
                }
            }
        }
    }

    let mappings = patterns
        .iter()
        .filter(|(pattern, _)| !schema.include.iter().any(|item| item.pattern == *pattern))
        .map(|(pattern, collection)| {
            format!(
                "  {}: {}",
                serde_json::to_string(pattern).expect("schema glob"),
                collection
            )
        })
        .collect::<Vec<_>>();
    if !mappings.is_empty() {
        let header = section("include");
        let added = additions
            .entry(
                header
                    .map(|header| insertion_offset(&lines, header))
                    .unwrap_or(raw.len()),
            )
            .or_default();
        if header.is_none() {
            added.push("include:".to_string());
        }
        added.extend(mappings);
    }

    for (offset, added) in additions {
        let prefix = if offset > 0 && !raw[..offset].ends_with('\n') {
            newline
        } else {
            ""
        };
        let suffix = if offset == raw.len() && !raw.ends_with('\n') {
            ""
        } else {
            newline
        };
        edits.push((
            offset..offset,
            format!("{}{}{}", prefix, added.join(newline), suffix),
        ));
    }
    edits.sort_by_key(|(range, _)| std::cmp::Reverse(range.start));
    let mut text = raw.to_string();
    for (range, replacement) in edits {
        text.replace_range(range, &replacement);
    }
    // Validate before writing any changed schema bytes.
    parse_schema(&text)?;
    Ok(text)
}
