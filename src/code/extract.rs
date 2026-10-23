use std::collections::BTreeMap;
use std::path::Path;

use tree_sitter::{Language, Node, Parser, Query, QueryCursor};

use super::{file_language, CodeNode};
use crate::Result;

// Equivalent to Rust's tags query, with each method capturing its own function
// rather than the enclosing declaration_list. Include constants and signatures.
const RUST_TAGS: &str = r#"
    (struct_item name: (type_identifier) @name) @definition.class
    (enum_item name: (type_identifier) @name) @definition.class
    (union_item name: (type_identifier) @name) @definition.class
    (type_item name: (type_identifier) @name) @definition.type
    (trait_item name: (type_identifier) @name) @definition.interface
    (function_item name: (identifier) @name) @definition.function
    (function_signature_item name: (identifier) @name) @definition.function
    (mod_item name: (identifier) @name) @definition.module
    (macro_definition name: (identifier) @name) @definition.macro
    (const_item name: (identifier) @name) @definition.constant
    (static_item name: (identifier) @name) @definition.constant
"#;

const JS_CONSTANTS: &str = r#"
    (lexical_declaration kind: "const"
      (variable_declarator name: (identifier) @name) @definition.constant)
"#;

#[derive(Default)]
pub(super) struct Extractor {
    // Compile each grammar/query only once per scan, including a separate TSX parser.
    languages: BTreeMap<String, (Parser, Query)>,
}

impl Extractor {
    pub(super) fn extract(&mut self, path: &str, text: &str) -> Result<Vec<CodeNode>> {
        let lang = file_language(path);
        let grammar_key = if Path::new(path)
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.eq_ignore_ascii_case("tsx"))
            .unwrap_or(false)
        {
            "tsx"
        } else {
            &lang
        };
        let mut file = CodeNode {
            id: format!("file:{path}"),
            kind: "file",
            path: path.to_string(),
            name: String::new(),
            span_start: 1,
            span_end: text.lines().count().max(1),
            fingerprint: blake3::hash(text.as_bytes()).to_hex()[..12].to_string(),
            lang: lang.clone(),
            is_definition: false,
            is_test: false,
            signature: String::new(),
            doc_comment: String::new(),
        };
        if !self.languages.contains_key(grammar_key) {
            let Some((language, tags)) = grammar(grammar_key) else {
                return Ok(vec![file]);
            };
            let mut parser = Parser::new();
            parser.set_language(&language)?;
            let query = Query::new(&language, &tags)?;
            self.languages
                .insert(grammar_key.to_string(), (parser, query));
        }
        let (parser, query) = self.languages.get_mut(grammar_key).unwrap();
        let tree = parser
            .parse(text, None)
            .ok_or("tree-sitter parsing cancelled")?;
        file.fingerprint = fingerprint(tree.root_node(), text, &lang);
        let mut nodes = vec![file];
        let mut definitions = BTreeMap::new();
        let mut cursor = QueryCursor::new();
        for found in cursor.matches(query, tree.root_node(), text.as_bytes()) {
            let mut name = None;
            let mut definition = None;
            for capture in found.captures {
                let capture_name = query.capture_names()[capture.index as usize];
                if capture_name == "name" {
                    name = Some(capture.node);
                } else if capture_name.starts_with("definition.") {
                    definition = Some(capture.node);
                }
            }
            if let (Some(name), Some(definition)) = (name, definition) {
                let name = qualified_name(definition, &text[name.byte_range()], text);
                definitions.insert(
                    (definition.start_byte(), definition.end_byte(), name),
                    definition,
                );
            }
        }
        let mut occurrences = BTreeMap::new();
        for ((_, _, name), definition) in definitions {
            let occurrence = occurrences.entry(name.clone()).or_insert(0);
            *occurrence += 1;
            nodes.push(CodeNode {
                id: format!("symbol:{path}#{name}:{}", occurrence),
                kind: "symbol",
                path: path.to_string(),
                name,
                span_start: definition.start_position().row + 1,
                span_end: definition.end_position().row + 1,
                fingerprint: fingerprint(definition, text, &lang),
                lang: lang.clone(),
                is_definition: match definition.kind() {
                    "mod_item" | "module" | "internal_module" => {
                        definition.child_by_field_name("body").is_some()
                    }
                    "package_clause" => false,
                    _ => true,
                },
                signature: signature(definition, text),
                doc_comment: doc_comment(definition, text, &lang),
                is_test: lang == "rust" && rust_test_definition(definition, text),
            });
        }
        Ok(nodes)
    }
}

fn rust_test_definition(mut node: Node<'_>, text: &str) -> bool {
    loop {
        if node.kind() == "mod_item"
            && node
                .child_by_field_name("name")
                .map(|name| matches!(&text[name.byte_range()], "tests" | "test"))
                .unwrap_or(false)
        {
            return true;
        }
        let mut previous = node.prev_named_sibling();
        while let Some(attribute) = previous {
            if attribute.kind().contains("comment") {
                previous = attribute.prev_named_sibling();
                continue;
            }
            if attribute.kind() != "attribute_item" {
                break;
            }
            let value = text[attribute.byte_range()]
                .split_whitespace()
                .collect::<String>();
            if value == "#[test]" || value == "#[cfg(test)]" || value.ends_with("::test]") {
                return true;
            }
            previous = attribute.prev_named_sibling();
        }
        let Some(parent) = node.parent() else {
            return false;
        };
        node = parent;
    }
}

fn signature(definition: Node<'_>, text: &str) -> String {
    let end = definition
        .child_by_field_name("body")
        .or_else(|| {
            definition
                .child_by_field_name("value")
                .and_then(|value| value.child_by_field_name("body"))
        })
        .map(|body| body.start_byte())
        .unwrap_or_else(|| definition.end_byte());
    text[definition.start_byte()..end]
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(1024)
        .collect()
}

fn doc_comment(definition: Node<'_>, text: &str, lang: &str) -> String {
    // Exports and decorators wrap the definition in the JS/Python grammars.
    let attachment = definition
        .parent()
        .filter(|parent| matches!(parent.kind(), "export_statement" | "decorated_definition"))
        .unwrap_or(definition);
    let mut comments = Vec::new();
    let mut start = attachment.start_byte();
    let mut previous = attachment.prev_named_sibling();
    while let Some(node) = previous {
        let gap = &text[node.end_byte()..start];
        if !gap.trim().is_empty() || gap.bytes().filter(|byte| *byte == b'\n').count() > 1 {
            break;
        }
        if node.kind().contains("comment") {
            let comment = text[node.byte_range()].trim();
            if lang != "rust" || comment.starts_with("///") || comment.starts_with("/**") {
                comments.push(comment.to_string());
            }
        } else if node.kind() != "attribute_item" {
            break;
        }
        start = node.start_byte();
        previous = node.prev_named_sibling();
    }
    comments.reverse();
    if lang == "python" {
        if let Some(string) = definition
            .child_by_field_name("body")
            .and_then(|body| body.named_child(0))
            .filter(|statement| statement.kind() == "expression_statement")
            .and_then(|statement| statement.named_child(0))
            .filter(|expression| expression.kind() == "string")
        {
            comments.push(text[string.byte_range()].to_string());
        }
    }
    comments.join("\n")
}

fn grammar(lang: &str) -> Option<(Language, String)> {
    Some(match lang {
        "rust" => (tree_sitter_rust::language(), RUST_TAGS.to_string()),
        "javascript" => (
            tree_sitter_javascript::language(),
            format!("{}\n{}", tree_sitter_javascript::TAGS_QUERY, JS_CONSTANTS),
        ),
        "typescript" | "tsx" => (
            if lang == "tsx" {
                tree_sitter_typescript::language_tsx()
            } else {
                tree_sitter_typescript::language_typescript()
            },
            // TypeScript's tags only add TS-specific constructs; JS supplies functions/classes.
            format!(
                "{}\n{}\n{}\n{}",
                tree_sitter_javascript::TAGS_QUERY,
                tree_sitter_typescript::TAGS_QUERY,
                JS_CONSTANTS,
                r#"
                    (type_alias_declaration name: (type_identifier) @name) @definition.type
                    (enum_declaration name: (identifier) @name) @definition.type
                    (internal_module name: (_) @name) @definition.module
                "#,
            ),
        ),
        "python" => (
            tree_sitter_python::language(),
            tree_sitter_python::TAGS_QUERY.to_string(),
        ),
        "go" => (
            tree_sitter_go::language(),
            format!(
                "{}\n{}",
                tree_sitter_go::TAGS_QUERY,
                r#"
                (const_spec name: (identifier) @name) @definition.constant
                (package_clause (package_identifier) @name) @definition.module
            "#
            ),
        ),
        _ => return None,
    })
}

fn type_name<'a>(node: Node<'_>, text: &'a str) -> &'a str {
    match node.kind() {
        "generic_type" => node
            .child_by_field_name("type")
            .map(|node| type_name(node, text)),
        "pointer_type" | "reference_type" => node.named_child(0).map(|node| type_name(node, text)),
        _ => None,
    }
    .unwrap_or(&text[node.byte_range()])
}

fn qualified_name(definition: Node<'_>, name: &str, text: &str) -> String {
    let mut scopes = Vec::new();
    let mut parent = definition.parent();
    while let Some(node) = parent {
        let scope = match node.kind() {
            "impl_item" => node.child_by_field_name("type"),
            "mod_item"
            | "trait_item"
            | "class_definition"
            | "class_declaration"
            | "class"
            | "abstract_class_declaration"
            | "interface_declaration"
            | "internal_module"
            | "module"
            | "function_definition"
            | "function_item"
            | "function_declaration" => node.child_by_field_name("name"),
            _ => None,
        };
        if let Some(scope) = scope {
            scopes.push(type_name(scope, text));
        }
        parent = node.parent();
    }
    scopes.reverse();
    if definition.kind() == "method_declaration" {
        if let Some(receiver) = definition.child_by_field_name("receiver") {
            if let Some(receiver_type) = receiver
                .named_child(0)
                .and_then(|node| node.child_by_field_name("type"))
            {
                scopes.push(type_name(receiver_type, text));
            }
        }
    }
    scopes.push(name);
    scopes.join("::")
}

fn hash_token(hasher: &mut blake3::Hasher, token: &[u8]) {
    // Length prefixes preserve token boundaries: `a b` differs from `ab`.
    hasher.update(&(token.len() as u64).to_le_bytes());
    hasher.update(token);
}

fn fingerprint(root: Node<'_>, text: &str, lang: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    let mut pending = vec![(root, false)];
    while let Some((node, leaving_block)) = pending.pop() {
        if leaving_block {
            hash_token(&mut hasher, b"\0dedent");
            continue;
        }
        if node.kind().contains("comment")
            || matches!(node.kind(), "hash_bang_line" | "line_continuation")
        {
            continue;
        }
        // Python INDENT/DEDENT tokens carry meaning, unlike the number of
        // spaces. Tree-sitter represents them as block boundaries, not leaves.
        if lang == "python" && node.kind() == "block" {
            hash_token(&mut hasher, b"\0indent");
            pending.push((node, true));
        }
        if node.child_count() == 0 {
            let token = &text.as_bytes()[node.byte_range()];
            // Trivia is absent from the syntax tree. Literal contents (even a
            // string consisting only of spaces) must retain their exact bytes.
            let literal = node.kind().contains("string")
                || matches!(node.kind(), "jsx_text" | "template_chars" | "regex_pattern");
            if !token.is_empty() && (literal || !token.iter().all(u8::is_ascii_whitespace)) {
                hash_token(&mut hasher, token);
            }
        } else {
            let mut cursor = node.walk();
            let children = node.children(&mut cursor).collect::<Vec<_>>();
            pending.extend(children.into_iter().rev().map(|node| (node, false)));
        }
    }
    hasher.finalize().to_hex()[..12].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn symbols(path: &str, text: &str) -> BTreeMap<String, String> {
        Extractor::default()
            .extract(path, text)
            .unwrap()
            .into_iter()
            .filter(|node| node.kind == "symbol")
            .map(|node| (node.name, node.fingerprint))
            .collect()
    }

    #[test]
    fn tags_cover_functions_methods_types_modules_and_constants_in_each_language() {
        for (path, text, names) in [
            ("api.rs", "mod auth { pub struct Session; impl Session { pub fn renew(&self) {} } const TTL: u64 = 60; type Token = u64; }",
                vec!["auth", "auth::Session", "auth::Session::renew", "auth::TTL", "auth::Token"]),
            ("api.ts", "namespace auth { export type Token = number; export interface State { renew(): void; } export enum Mode { Live } export class Session { renew() {} } export function refresh() {} export const TTL = 60; export const arrow = () => 1; }",
                vec!["auth", "auth::Token", "auth::State", "auth::State::renew", "auth::Mode", "auth::Session", "auth::Session::renew", "auth::refresh", "auth::TTL", "auth::arrow"]),
            ("api.js", "class Session { renew() {} } function refresh() {} const TTL = 60; const arrow = () => 1;",
                vec!["Session", "Session::renew", "refresh", "TTL", "arrow"]),
            ("api.py", "TTL = 60\nclass Session:\n    def renew(self):\n        return TTL\ndef refresh():\n    return 1\n",
                vec!["TTL", "Session", "Session::renew", "refresh"]),
            ("api.go", "package auth\nconst TTL = 60\ntype Session struct {}\nfunc (s *Session) renew() {}\nfunc refresh() {}\n",
                vec!["auth", "TTL", "Session", "Session::renew", "refresh"]),
        ] {
            let found = symbols(path, text);
            for name in names {
                assert!(found.contains_key(name), "{path}: missing {name}, got {found:?}");
            }
        }
    }

    #[test]
    fn whitespace_and_comments_do_not_change_fingerprints_but_logic_does() {
        for (path, before, formatted, changed) in [
            (
                "a.rs",
                "fn f(x: i32) -> i32 { x + 60 }",
                "// doc\nfn f ( x : i32 ) -> i32 { /* comment */ x + 60\n}",
                "fn f(x: i32) -> i32 { x + 90 }",
            ),
            (
                "a.ts",
                "function f(x: number) { return x + 60; }",
                "// doc\nfunction f (x: number) {\n // comment\n return x + 60 ;\n}",
                "function f(x: number) { return x + 90; }",
            ),
            (
                "a.js",
                "function f(x) { return x + 60; }",
                "/* doc */\nfunction f ( x ) {\n return x + /* comment */ 60 ;\n}",
                "function f(x) { return x + 90; }",
            ),
            (
                "a.py",
                "def f(x):\n    return x + 60\n",
                "# doc\ndef f( x ):\n        # comment\n        return x + 60\n",
                "def f(x):\n    return x + 90\n",
            ),
            (
                "a.go",
                "package a\nfunc f(x int) int { return x + 60 }\n",
                "package a\n// doc\nfunc f (x int) int {\n // comment\n return x + 60\n}\n",
                "package a\nfunc f(x int) int { return x + 90 }\n",
            ),
        ] {
            let original = symbols(path, before);
            let hash = &original["f"];
            assert_eq!(hash.len(), 12);
            assert!(hash.bytes().all(|byte| byte.is_ascii_hexdigit()));
            assert_eq!(hash, &symbols(path, formatted)["f"], "{path}");
            assert_ne!(hash, &symbols(path, changed)["f"], "{path}");
        }
    }

    #[test]
    fn string_whitespace_and_comment_markers_remain_part_of_the_token_stream() {
        for (path, before, changed) in [
            (
                "a.rs",
                "fn f() { let s = r#\" /* a */ \"#; }",
                "fn f() { let s = r#\"/* a */\"#; }",
            ),
            (
                "a.ts",
                "function f() { return ' '; }",
                "function f() { return ''; }",
            ),
            (
                "a.js",
                "function f() { return `  ${1 + 2} // a`; }",
                "function f() { return ` ${1 + 2} // a`; }",
            ),
            (
                "a.js",
                "function f() { return / /; }",
                "function f() { return /  /; }",
            ),
            (
                "a.py",
                "def f():\n    return ' # a '\n",
                "def f():\n    return '# a'\n",
            ),
            (
                "a.go",
                "package a\nfunc f() string { return ` /* a */ ` }",
                "package a\nfunc f() string { return `/* a */` }",
            ),
        ] {
            assert_ne!(
                symbols(path, before)["f"],
                symbols(path, changed)["f"],
                "{path}"
            );
        }
    }

    #[test]
    fn python_continuations_are_trivia_but_indentation_keeps_its_meaning() {
        let before = "def f(x):\n    if x:\n        x += 60\n    return x\n";
        let formatted = "def f( x ):\n  if x:\n    x += \\\n      60\n  return x\n";
        let changed = "def f(x):\n    if x:\n        x += 60\n        return x\n";
        let original = symbols("a.py", before);
        assert_eq!(original["f"], symbols("a.py", formatted)["f"]);
        assert_ne!(original["f"], symbols("a.py", changed)["f"]);
    }

    #[test]
    fn method_spans_and_fingerprints_do_not_include_sibling_methods() {
        let before =
            "struct Session;\nimpl Session {\n fn renew() { one(); }\n fn close() { two(); }\n}";
        let after = before.replace("two()", "three()");
        let original = symbols("a.rs", before);
        let changed = symbols("a.rs", &after);
        assert_eq!(original["Session::renew"], changed["Session::renew"]);
        assert_ne!(original["Session::close"], changed["Session::close"]);
        let nodes = Extractor::default().extract("a.rs", before).unwrap();
        let method = nodes
            .iter()
            .find(|node| node.name == "Session::renew")
            .unwrap();
        assert_eq!((method.span_start, method.span_end), (3, 3));
    }

    #[test]
    fn duplicate_names_have_distinct_ids_and_formatting_preserves_ids() {
        let before = "function f() {}\nfunction f(x) { return x; }";
        let mut extractor = Extractor::default();
        let original = extractor.extract("a.js", before).unwrap();
        let changed = extractor
            .extract("a.js", &format!("// comment\n\n{before}"))
            .unwrap();
        let ids = original.iter().map(|node| &node.id).collect::<Vec<_>>();
        assert_eq!(ids.len(), 3);
        assert_ne!(ids[1], ids[2]);
        assert_eq!(ids, changed.iter().map(|node| &node.id).collect::<Vec<_>>());
    }

    #[test]
    fn jsx_tsx_and_module_extension_aliases_use_the_correct_grammars() {
        for path in [
            "a.jsx", "a.tsx", "a.TSX", "a.mjs", "a.cjs", "a.mts", "a.cts", "a.pyi",
        ] {
            let text = if path.ends_with("pyi") {
                "def f() -> str: ..."
            } else if path.to_ascii_lowercase().ends_with("sx") {
                "function f() { return <span> hello </span>; }"
            } else {
                "function f() { return 1; }"
            };
            assert!(symbols(path, text).contains_key("f"), "{path}");
        }
    }

    #[test]
    fn unsupported_languages_keep_only_a_file_node() {
        let nodes = Extractor::default()
            .extract("Main.java", "class Main {}\n")
            .unwrap();
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].kind, "file");
        assert_eq!(nodes[0].lang, "java");
        assert_eq!((nodes[0].span_start, nodes[0].span_end), (1, 1));
    }

    #[test]
    fn parser_recovers_symbols_from_incomplete_code_and_references_are_not_definitions() {
        let names = symbols("a.rs", "fn valid() { referenced(); }\nfn unfinished(");
        assert!(names.contains_key("valid"));
        assert!(!names.contains_key("referenced"));
    }
}
