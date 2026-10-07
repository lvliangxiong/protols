use async_lsp::lsp_types::{LocationLink, Position, Range, Url};

use super::ProtoLanguageState;
use crate::config::Config;

const DEFINITIONS: &str = r#"syntax = "proto3";
package example.api;
message Outer {
  message Header { string value = 1; }
}
"#;

fn range_of(source: &str, needle: &str) -> Range {
    let offset = source.find(needle).unwrap();
    let before = &source[..offset];
    let line = u32::try_from(before.bytes().filter(|&b| b == b'\n').count()).unwrap();
    let column = u32::try_from(before.rsplit('\n').next().unwrap().len()).unwrap();
    Range::new(
        Position::new(line, column),
        Position::new(line, column + u32::try_from(needle.len()).unwrap()),
    )
}

fn state_with_usage(usage: &str) -> (ProtoLanguageState, Url, Url) {
    let definitions = Url::parse("file:///navigation/definitions.proto").unwrap();
    let document = Url::parse("file:///navigation/usage.proto").unwrap();
    let mut state = ProtoLanguageState::new();
    state.upsert_file(&definitions, DEFINITIONS, &[], 1, &Config::default(), false);
    state.upsert_file(&document, usage, &[], 1, &Config::default(), false);
    (state, document, definitions)
}

#[test]
fn definition_links_cover_complete_type_references() {
    for (field, type_name) in [
        (
            "example.api.Outer.Header header = 1;",
            "example.api.Outer.Header",
        ),
        (
            ".example.api.Outer.Header header = 1;",
            ".example.api.Outer.Header",
        ),
        (
            "map<string, example.api.Outer.Header> headers = 1;",
            "example.api.Outer.Header",
        ),
        (
            "oneof choice { example.api.Outer.Header header = 1; }",
            "example.api.Outer.Header",
        ),
    ] {
        let usage =
            format!("syntax = \"proto3\";\npackage example.client;\nmessage Request {{ {field} }}");
        let (state, document, definitions) = state_with_usage(&usage);
        let origin = range_of(&usage, type_name);
        for character in origin.start.character..origin.end.character {
            let links =
                state.definition(&document, Position::new(origin.start.line, character), &[]);
            assert_eq!(links.len(), 1, "{type_name} at {character}");
            let link = &links[0];
            assert_eq!(link.origin_selection_range, Some(origin));
            assert_eq!(link.target_uri, definitions);
            assert_eq!(link.target_selection_range, range_of(DEFINITIONS, "Header"));
            assert_eq!(
                link.target_range,
                range_of(DEFINITIONS, "message Header { string value = 1; }")
            );
        }
        assert!(state.definition(&document, origin.end, &[]).is_empty());
        // A field name stays a separate symbol, rather than becoming part of its type link.
        let field_name = range_of(&usage, "header");
        let links = state.definition(&document, field_name.start, &[]);
        assert_eq!(
            links[0].origin_selection_range,
            Some(if field.contains("headers") {
                range_of(&usage, "headers")
            } else {
                field_name
            })
        );
        assert_eq!(links[0].target_uri, document);
    }
}

#[test]
fn definition_links_cover_rpc_types_and_declarations() {
    let usage = "syntax = \"proto3\";\npackage example.client;\nservice Api { rpc Send(example.api.Outer.Header) returns (.example.api.Outer.Header); }";
    let (state, document, definitions) = state_with_usage(usage);
    for name in ["example.api.Outer.Header", ".example.api.Outer.Header"] {
        let origin = range_of(usage, name);
        let links = state.definition(&document, origin.start, &[]);
        assert_eq!(links[0].origin_selection_range, Some(origin));
        assert_eq!(links[0].target_uri, definitions);
        assert_eq!(
            links[0].target_selection_range,
            range_of(DEFINITIONS, "Header")
        );
    }
    let name = range_of(DEFINITIONS, "Header");
    let links = state.definition(&definitions, name.start, &[]);
    assert_eq!(links[0].origin_selection_range, Some(name));
    assert_eq!(links[0].target_selection_range, name);
    assert!(
        state
            .definition(&document, Position::new(0, 0), &[])
            .is_empty()
    );
    let missing = "syntax = \"proto3\"; message Test { missing.Type value = 1; }";
    let (state, document, _) = state_with_usage(missing);
    assert!(
        state
            .definition(&document, range_of(missing, "missing.Type").start, &[])
            .is_empty()
    );
}

#[test]
fn definition_links_preserve_multiple_matching_declarations() {
    let usage = "syntax = \"proto3\"; message Request { example.api.Outer.Header header = 1; }";
    let (mut state, document, definitions) = state_with_usage(usage);
    let duplicate = Url::parse("file:///navigation/duplicate.proto").unwrap();
    state.upsert_file(&duplicate, DEFINITIONS, &[], 1, &Config::default(), false);
    for (uri, origin) in [
        (&document, range_of(usage, "example.api.Outer.Header")),
        (&definitions, range_of(DEFINITIONS, "Header")),
    ] {
        let links = state.definition(uri, origin.start, &[]);
        assert_eq!(links.len(), 2);
        for target in [&definitions, &duplicate] {
            assert!(links.iter().any(|link| {
                link.target_uri == *target
                    && link.origin_selection_range == Some(origin)
                    && link.target_selection_range == range_of(DEFINITIONS, "Header")
            }));
        }
    }
}

#[test]
fn import_links_exclude_keywords_quotes_and_semicolons() {
    let temporary = tempfile::tempdir().unwrap();
    std::fs::create_dir(temporary.path().join("sub")).unwrap();
    let target = temporary.path().join("sub/shared.proto");
    std::fs::write(&target, "syntax = \"proto3\";").unwrap();
    let target_uri = Url::from_file_path(&target).unwrap();
    let document = Url::from_file_path(temporary.path().join("source.proto")).unwrap();
    let includes = vec![temporary.path().to_path_buf()];
    for statement in [
        "import \"sub/shared.proto\";",
        "import public \"sub/shared.proto\";",
        "import weak 'sub/shared.proto';",
    ] {
        let content = format!("syntax = \"proto3\";\n{statement}");
        let mut state = ProtoLanguageState::new();
        state.upsert_file(&document, &content, &includes, 1, &Config::default(), false);
        let origin = range_of(&content, "sub/shared.proto");
        for character in origin.start.character..origin.end.character {
            assert_eq!(
                state.definition(&document, Position::new(1, character), &includes),
                vec![LocationLink {
                    origin_selection_range: Some(origin),
                    target_uri: target_uri.clone(),
                    target_range: Range::default(),
                    target_selection_range: Range::default(),
                }]
            );
        }
        for position in [
            Position::new(1, 0),
            Position::new(1, origin.start.character - 1),
            origin.end,
            Position::new(1, origin.end.character + 1),
        ] {
            assert!(
                state.definition(&document, position, &includes).is_empty(),
                "{statement}: {position:?}"
            );
        }
    }
}

#[test]
fn references_honor_include_declaration_without_dropping_uses() {
    let usage = "syntax = \"proto3\";\nmessage Request { example.api.Outer.Header header = 1; }";
    let (mut state, document, definitions) = state_with_usage(usage);
    let fqn = "example.api.Outer.Header";
    let uses = state.references_for_fqn(fqn, false);
    assert_eq!(uses.len(), 1);
    assert_eq!(uses[0].uri, document);
    assert_eq!(uses[0].range, range_of(usage, fqn));
    let all = state.references_for_fqn(fqn, true);
    assert_eq!(all.len(), 2);
    assert!(
        all.iter()
            .any(|loc| loc.uri == definitions && loc.range == range_of(DEFINITIONS, "Header"))
    );
    assert_eq!(state.references_for_fqn("example.api.Outer", false), vec![]);
    let second = Url::parse("file:///navigation/second.proto").unwrap();
    state.upsert_file(&second, usage, &[], 1, &Config::default(), false);
    assert_eq!(state.references_for_fqn(fqn, false).len(), 2);
    assert_eq!(state.references_for_fqn(fqn, true).len(), 3);
    assert!(state.references_for_fqn("missing.Type", true).is_empty());
}

#[test]
fn references_distinguish_matching_names_across_scopes() {
    let target = Url::parse("file:///navigation/target.proto").unwrap();
    let other = Url::parse("file:///navigation/other.proto").unwrap();
    let target_source = r#"syntax = "proto3";
package example.target;
message Header {}
message Request {
  Header local_header = 1;
  .example.other.Header other_header = 2;
  string unrelated = 3;
}
"#;
    let other_source = r#"syntax = "proto3";
package example.other;
message Header {}
message Request {
  Header local_header = 1;
  .example.target.Header target_header = 2;
  repeated example.target.Header target_headers = 3;
  message Nested {
    message Header {}
    Header nested_header = 1;
  }
}
"#;
    let mut state = ProtoLanguageState::new();
    for (uri, content) in [(&target, target_source), (&other, other_source)] {
        state.upsert_file(uri, content, &[], 1, &Config::default(), false);
    }
    let references = state.references_for_fqn("example.target.Header", false);
    assert_eq!(references.len(), 3);
    for (uri, source, token) in [
        (&target, target_source, "Header local_header"),
        (&other, other_source, ".example.target.Header"),
        (&other, other_source, "repeated example.target.Header"),
    ] {
        let mut range = range_of(source, token);
        if token.starts_with("repeated ") {
            range.start.character += 9;
        } else if token == "Header local_header" {
            range.end.character = range.start.character + 6;
        }
        assert!(
            references
                .iter()
                .any(|loc| loc.uri == *uri && loc.range == range)
        );
    }
    assert_eq!(
        state
            .references_for_fqn("example.target.Header", true)
            .len(),
        4
    );
    assert_eq!(
        state
            .references_for_fqn("example.other.Header", false)
            .len(),
        2
    );
    assert_eq!(
        state
            .references_for_fqn("example.other.Request.Nested.Header", false)
            .len(),
        1
    );
}
