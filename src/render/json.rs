use std::io::{self, Write};
use std::path::Path;

use serde::{Serialize, Serializer};
use serde_json::ser::Formatter;
use serde_json::value::RawValue;

use crate::checks::{rule_group, CheckView};
use crate::context::ContextPacket;
use crate::doctor::DoctorReport;
use crate::fs_utils::absolute_display_path;
use crate::history::HistorySnapshot;
use crate::index::{DocRelation, IndexArtifact};
use crate::query::QueryCandidate;

// Only whitespace and numeric presentation differ from serde_json's formatters.
// Keep alpha.6's multiline empty arrays, inline lists and two-decimal confidence.
#[derive(Default)]
struct CliJsonFormatter {
    inline: bool,
    depth: usize,
    has_value: Vec<bool>,
}

impl CliJsonFormatter {
    fn indent<W: ?Sized + Write>(&self, writer: &mut W) -> io::Result<()> {
        for _ in 0..self.depth {
            writer.write_all(b"  ")?;
        }
        Ok(())
    }
}

impl Formatter for CliJsonFormatter {
    fn begin_array<W: ?Sized + Write>(&mut self, writer: &mut W) -> io::Result<()> {
        if self.inline {
            writer.write_all(b"[")
        } else {
            self.depth += 1;
            self.has_value.push(false);
            writer.write_all(b"[\n")
        }
    }

    fn end_array<W: ?Sized + Write>(&mut self, writer: &mut W) -> io::Result<()> {
        if !self.inline {
            self.depth -= 1;
            if self.has_value.pop().unwrap_or(false) {
                writer.write_all(b"\n")?;
            }
            self.indent(writer)?;
        }
        writer.write_all(b"]")
    }

    fn begin_array_value<W: ?Sized + Write>(
        &mut self,
        writer: &mut W,
        first: bool,
    ) -> io::Result<()> {
        if !first {
            writer.write_all(if self.inline { b", " } else { b",\n" })?;
        }
        if !self.inline {
            self.indent(writer)?;
        }
        Ok(())
    }

    fn end_array_value<W: ?Sized + Write>(&mut self, _writer: &mut W) -> io::Result<()> {
        if let Some(has_value) = self.has_value.last_mut() {
            *has_value = true;
        }
        Ok(())
    }

    fn begin_object<W: ?Sized + Write>(&mut self, writer: &mut W) -> io::Result<()> {
        if !self.inline {
            // Context's anchor historically has two extra spaces before its object.
            if self.depth == 1 {
                writer.write_all(b"  ")?;
            }
            self.depth += 1;
            self.has_value.push(false);
        }
        writer.write_all(b"{")
    }

    fn end_object<W: ?Sized + Write>(&mut self, writer: &mut W) -> io::Result<()> {
        if !self.inline {
            self.depth -= 1;
            if self.has_value.pop().unwrap_or(false) {
                writer.write_all(b"\n")?;
                self.indent(writer)?;
            }
        }
        writer.write_all(b"}")
    }

    fn begin_object_key<W: ?Sized + Write>(
        &mut self,
        writer: &mut W,
        first: bool,
    ) -> io::Result<()> {
        if !first {
            writer.write_all(b",")?;
        }
        if !self.inline {
            writer.write_all(b"\n")?;
            self.indent(writer)?;
        }
        Ok(())
    }

    fn begin_object_value<W: ?Sized + Write>(&mut self, writer: &mut W) -> io::Result<()> {
        writer.write_all(if self.inline { b":" } else { b": " })
    }

    fn end_object_value<W: ?Sized + Write>(&mut self, _writer: &mut W) -> io::Result<()> {
        if let Some(has_value) = self.has_value.last_mut() {
            *has_value = true;
        }
        Ok(())
    }

    fn write_f32<W: ?Sized + Write>(&mut self, writer: &mut W, value: f32) -> io::Result<()> {
        write!(writer, "{:.2}", value)
    }

    fn write_f64<W: ?Sized + Write>(&mut self, writer: &mut W, value: f64) -> io::Result<()> {
        write!(writer, "{:.2}", value)
    }
}

fn to_json<T: ?Sized + Serialize>(value: &T, inline: bool) -> String {
    let mut bytes = Vec::new();
    let formatter = CliJsonFormatter {
        inline,
        ..Default::default()
    };
    value
        .serialize(&mut serde_json::Serializer::with_formatter(
            &mut bytes, formatter,
        ))
        .expect("serialize CLI output");
    String::from_utf8(bytes).expect("JSON is UTF-8")
}

pub(crate) fn serialize_inline<T: ?Sized + Serialize, S: Serializer>(
    value: &T,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    RawValue::from_string(to_json(value, true))
        .map_err(serde::ser::Error::custom)?
        .serialize(serializer)
}

pub(crate) fn serialize_evidence<S: Serializer>(
    value: &str,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    match serde_json::from_str::<&RawValue>(value.trim()) {
        Ok(raw) => raw.serialize(serializer),
        Err(_) => value.serialize(serializer),
    }
}

pub(crate) fn render_history_snapshot_json(snapshot: &HistorySnapshot) -> String {
    to_json(snapshot, false) + "\n"
}

pub(crate) fn render_index_json(artifact: &IndexArtifact) -> String {
    to_json(artifact, false) + "\n"
}

pub(crate) fn render_doctor_json(report: &DoctorReport) -> String {
    #[derive(Serialize)]
    struct Report<'a> {
        #[serde(flatten)]
        report: &'a DoctorReport,
        coverage: &'a str,
        notes: [&'a str; 2],
    }
    to_json(&Report {
        report,
        coverage: "Local environment and possible orphan HarnessKit state; the orphan scan skips symlinks, hidden directories, target and node_modules. Use check for document health.",
        notes: [
            ".harnesskit/state is derived and rebuildable; .harnesskit/history stores local doc checkpoints.",
            "If diagnostics do not point at the intended writable project root, pass the project root explicitly.",
        ],
    }, false)
}

pub(crate) fn render_check_json(
    target_dir: &Path,
    docs_count: usize,
    relation_count: usize,
    checks: &[CheckView],
    hints: &[String],
) -> String {
    #[derive(Serialize)]
    struct Check<'a> {
        severity: &'a str,
        rule_id: &'a str,
        rule_group: String,
        subject_path: &'a str,
        message: &'a str,
        #[serde(serialize_with = "serialize_evidence")]
        evidence_json: &'a str,
        suppressed: bool,
        suppression_reason: Option<&'a str>,
    }
    #[derive(Serialize)]
    struct Report<'a> {
        target_dir: String,
        docs_count: usize,
        relation_count: usize,
        checks: Vec<Check<'a>>,
        coverage: &'a str,
        hints: &'a [String],
    }
    let checks = checks
        .iter()
        .map(|check| Check {
            severity: &check.severity,
            rule_id: &check.rule_id,
            rule_group: rule_group(&check.rule_id),
            subject_path: &check.subject_path,
            message: &check.message,
            evidence_json: &check.evidence_json,
            suppressed: check.suppressed,
            suppression_reason: check.suppression_reason.as_deref(),
        })
        .collect();
    to_json(
        &Report {
            target_dir: absolute_display_path(target_dir),
            docs_count,
            relation_count,
            checks,
            coverage: "Schema-managed documents only; other Markdown and external files are not checked. The orphan state scan skips symlinks, hidden directories, target and node_modules.",
            hints,
        },
        false,
    ) + "\n"
}

pub(crate) fn render_query_json(
    query_terms: &str,
    candidates: &[QueryCandidate],
    hints: &[String],
) -> String {
    #[derive(Serialize)]
    struct Report<'a> {
        query: &'a str,
        candidates: &'a [QueryCandidate],
        coverage: &'a str,
        hints: &'a [String],
    }
    to_json(
        &Report {
            query: query_terms,
            candidates,
            coverage: "Schema-managed documents in the fact store; other Markdown and external files are not searched.",
            hints,
        },
        false,
    ) + "\n"
}

pub(crate) fn render_context_packet_json(
    kind: &str,
    input: &str,
    packet: &ContextPacket,
    hints: &[String],
) -> String {
    #[derive(Serialize)]
    struct Relation<'a> {
        src_path: &'a str,
        dst_path: &'a str,
        relation_type: &'a str,
        reason: &'a str,
        confidence: f32,
        explicit: bool,
    }
    impl<'a> From<&'a DocRelation> for Relation<'a> {
        fn from(relation: &'a DocRelation) -> Self {
            Self {
                src_path: &relation.src_path,
                dst_path: &relation.dst_path,
                relation_type: &relation.relation_type,
                reason: &relation.reason,
                confidence: relation.confidence,
                explicit: relation.explicit,
            }
        }
    }
    #[derive(Serialize)]
    struct Report<'a> {
        kind: &'a str,
        input: &'a str,
        anchor: &'a QueryCandidate,
        #[serde(serialize_with = "serialize_inline")]
        incoming: Vec<Relation<'a>>,
        #[serde(serialize_with = "serialize_inline")]
        outgoing: Vec<Relation<'a>>,
        #[serde(serialize_with = "serialize_inline")]
        collection_siblings: &'a [String],
        #[serde(serialize_with = "serialize_inline")]
        same_scope_docs: &'a [String],
        #[serde(serialize_with = "serialize_inline")]
        recommended_reading_order: &'a [String],
        coverage: &'a str,
        hints: &'a [String],
    }
    to_json(
        &Report {
            kind,
            input,
            anchor: &packet.anchor,
            incoming: packet.incoming.iter().map(Relation::from).collect(),
            outgoing: packet.outgoing.iter().map(Relation::from).collect(),
            collection_siblings: &packet.collection_siblings,
            same_scope_docs: &packet.same_scope_docs,
            recommended_reading_order: &packet.recommended_reading_order,
            coverage: "Schema-managed documents and their indexed relations; other Markdown and external files are not included in this context packet.",
            hints,
        },
        false,
    ) + "\n"
}
