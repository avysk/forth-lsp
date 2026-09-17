#[allow(unused_imports)]
use crate::prelude::*;
use crate::{
    utils::{
        HashMapGetForLSPParams,
        definition_index::DefinitionIndex,
        handlers::send_response,
        ropey::{get_ix::GetIx, word_on_or_before::WordOnOrBefore},
        word_lookup::find_builtin_word,
    },
    words::{Word, Words},
};

use std::collections::HashMap;

use lsp_server::{Connection, Request};
use lsp_types::{Hover, request::HoverRequest};
use ropey::Rope;

use super::cast;
use crate::config::Config;

// Extract the hover logic for testing
pub fn get_hover_result(
    word: &str,
    data: &Words,
    def_index: Option<&DefinitionIndex>,
    files: Option<&HashMap<String, Rope>>,
    doc_comments: bool,
) -> Option<Hover> {
    if !word.is_empty() {
        // Check if word is user-defined (overrides built-in docs)
        if let Some(index) = def_index {
            let details = index.find_definition_details(word);
            if !details.is_empty() {
                if doc_comments && details.iter().any(|d| d.doc_comment.is_some()) {
                    let mut sections = Vec::new();
                    for def in &details {
                        if let Some(ref doc) = def.doc_comment {
                            let file_name = def
                                .file_path_or_uri
                                .split(['/', '\\'])
                                .next_back()
                                .unwrap_or("unknown");
                            let display_name = if let Some(ref val) = def.constant_value {
                                format!("{word} = {val}")
                            } else {
                                word.to_string()
                            };
                            let header = format!(
                                "{}: defined at {}:{}",
                                display_name,
                                file_name,
                                def.range.start.line + 1
                            );
                            let mut lines = vec![header];
                            if let Some(ref stack) = def.stack_effect {
                                lines.push(stack.clone());
                            }
                            lines.push(doc.clone());
                            let section = lines.join("\n");
                            if !sections.contains(&section) {
                                sections.push(section);
                            }
                        }
                    }
                    if !sections.is_empty() {
                        return Some(Hover {
                            contents: lsp_types::HoverContents::Markup(lsp_types::MarkupContent {
                                kind: lsp_types::MarkupKind::Markdown,
                                value: sections.join("\n\n---\n\n"),
                            }),
                            range: None,
                        });
                    }
                }

                let defs = index.find_definitions(word);
                // User-defined word - show definition source code
                let display_name =
                    if let Some(val) = details.iter().find_map(|d| d.constant_value.as_deref()) {
                        format!("{word} = {val}")
                    } else {
                        word.to_string()
                    };
                let mut hover_text = format!("### `{}`\n\n", display_name);

                // Show each definition location and source code
                let mut shown_locations = std::collections::HashSet::new();
                let mut first = true;
                for def in &defs {
                    // Add location info
                    let file_name = def
                        .uri
                        .path()
                        .as_str()
                        .split('/')
                        .next_back()
                        .unwrap_or("unknown");
                    let loc_key = (
                        file_name.to_string(),
                        def.range.start.line,
                        def.range.start.character,
                    );
                    if !shown_locations.insert(loc_key) {
                        continue;
                    }
                    if !first {
                        hover_text.push_str("\n---\n\n");
                    }
                    first = false;

                    hover_text.push_str(&format!(
                        "**Defined in:** `{}:{}:{}`\n\n",
                        file_name,
                        def.range.start.line + 1,
                        def.range.start.character + 1
                    ));

                    let is_colon_def = details
                        .iter()
                        .find(|d| {
                            d.range == def.range
                                && (crate::utils::uri_helpers::path_str_to_uri(&d.file_path_or_uri)
                                    .as_ref()
                                    == Some(&def.uri)
                                    || d.file_path_or_uri == def.uri.as_str())
                        })
                        .or_else(|| details.iter().find(|d| d.range == def.range))
                        .map(|d| d.is_colon_definition)
                        .unwrap_or(false);

                    // Try to extract source code if files are available
                    if let Some(files_map) = files {
                        // Use URI string directly (files HashMap keys are URIs, not paths)
                        let rope = files_map.get(&def.uri.to_string()).or_else(|| {
                            let def_uri_str = def.uri.to_string();
                            let norm_target = def_uri_str.replace('\\', "/");
                            files_map.iter().find_map(|(k, v)| {
                                if k.replace('\\', "/") == norm_target {
                                    Some(v)
                                } else {
                                    None
                                }
                            })
                        });
                        if let Some(rope) = rope {
                            let start_line = def.range.start.line as usize;
                            let end_line = def.range.end.line as usize;

                            if !is_colon_def {
                                // For defining words (VARIABLE, CREATE, CONSTANT, etc.), show only the definition line
                                if let Some(line) = rope.get_line(start_line) {
                                    let line_str = line.to_string();
                                    let trimmed = line_str.trim();
                                    if !trimmed.is_empty() {
                                        hover_text.push_str("```forth\n");
                                        hover_text.push_str(trimmed);
                                        hover_text.push_str("\n```\n");
                                    }
                                }
                            } else {
                                // For colon definitions, expand to show the full definition up to ';'
                                let (display_start, display_end) = if start_line == end_line {
                                    let expanded_end =
                                        (end_line + 20).min(rope.len_lines().saturating_sub(1));
                                    (start_line, expanded_end)
                                } else {
                                    (start_line, end_line)
                                };

                                let mut source_lines = Vec::new();
                                for line_idx in display_start..=display_end.min(display_start + 20)
                                {
                                    if let Some(line) = rope.get_line(line_idx) {
                                        let line_str = line.to_string();
                                        source_lines.push(line_str.trim_end().to_string());
                                        if line_str.trim_end().ends_with(';') {
                                            break;
                                        }
                                    }
                                }

                                if !source_lines.is_empty() {
                                    hover_text.push_str("```forth\n");
                                    hover_text.push_str(&source_lines.join("\n"));
                                    hover_text.push_str("\n```\n");
                                }
                            }
                        }
                    }
                }

                return Some(Hover {
                    contents: lsp_types::HoverContents::Markup(lsp_types::MarkupContent {
                        kind: lsp_types::MarkupKind::Markdown,
                        value: hover_text,
                    }),
                    range: None,
                });
            }
        }

        // Fall back to built-in documentation
        let default_info = &Word::default();
        let info = find_builtin_word(word, data).unwrap_or(default_info);
        Some(Hover {
            contents: lsp_types::HoverContents::Markup(lsp_types::MarkupContent {
                kind: lsp_types::MarkupKind::Markdown,
                value: info.documentation(),
            }),
            range: None,
        })
    } else {
        None
    }
}

pub fn handle_hover(
    req: &Request,
    connection: &Connection,
    data: &Words,
    files: &mut HashMap<String, Rope>,
    def_index: &DefinitionIndex,
    config: &Config,
) -> Result<()> {
    match cast::<HoverRequest>(req.clone()) {
        Ok((id, params)) => {
            log_request!(id, params);
            let word = files
                .for_position_param(&params.text_document_position_params)
                .and_then(|rope| {
                    let ix = rope.get_ix(&params);
                    if ix >= rope.len_chars() {
                        return None;
                    }
                    Some(rope.word_on_or_before(ix).to_string())
                });
            let result = word.and_then(|w| {
                get_hover_result(&w, data, Some(def_index), Some(files), config.doc_comments)
            });
            send_response(connection, id, result)?;
            Ok(())
        }
        Err(Error::ExtractRequestError(req)) => Err(Error::ExtractRequestError(req)),
        Err(err) => panic!("{err:?}"),
        // Err(err @ ExtractError::JsonError { .. }) => panic!("{err:?}"),
        // Err(ExtractError::MethodMismatch(req)) => req,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lsp_types::MarkupKind;

    #[test]
    fn test_hover_finds_builtin_word() {
        let words = Words::default();
        let result = get_hover_result("DUP", &words, None, None, true);

        assert!(result.is_some());
        let hover = result.unwrap();
        if let lsp_types::HoverContents::Markup(content) = hover.contents {
            assert_eq!(content.kind, MarkupKind::Markdown);
            assert!(content.value.contains("DUP"));
            assert!(content.value.contains("( x -- x x )"));
        } else {
            panic!("Expected Markup hover contents");
        }
    }

    #[test]
    fn test_hover_case_insensitive() {
        let words = Words::default();
        let result = get_hover_result("dup", &words, None, None, true);

        assert!(result.is_some());
        let hover = result.unwrap();
        if let lsp_types::HoverContents::Markup(content) = hover.contents {
            assert!(content.value.contains("DUP"));
        } else {
            panic!("Expected Markup hover contents");
        }
    }

    #[test]
    fn test_hover_returns_none_for_unknown_word() {
        let words = Words::default();
        let result = get_hover_result("NONEXISTENT_WORD_12345", &words, None, None, true);

        // Unknown words return default Word, which still returns Some
        // This is the current behavior
        assert!(result.is_some());
    }

    #[test]
    fn test_hover_returns_none_for_empty_word() {
        let words = Words::default();
        let result = get_hover_result("", &words, None, None, true);

        assert!(result.is_none());
    }

    #[test]
    fn test_hover_stack_effect_operators() {
        let words = Words::default();
        let test_cases = vec![
            ("+", "( n1 | u1 n2 | u2 -- n3 | u3 )"),
            ("-", "( n1 | u1 n2 | u2 -- n3 | u3 )"),
            ("*", "( n1 | u1 n2 | u2 -- n3 | u3 )"),
            ("SWAP", "( x1 x2 -- x2 x1 )"),
        ];

        for (word, expected_stack) in test_cases {
            let result = get_hover_result(word, &words, None, None, true);
            assert!(result.is_some(), "Expected hover for word: {}", word);

            if let lsp_types::HoverContents::Markup(content) = result.unwrap().contents {
                assert!(
                    content.value.contains(expected_stack),
                    "Word '{}' should contain stack effect '{}'",
                    word,
                    expected_stack
                );
            }
        }
    }

    #[test]
    fn test_hover_user_defined_overrides_builtin() {
        use crate::utils::definition_index::DefinitionIndex;
        use ropey::Rope;
        use std::env;

        let words = Words::default();
        let mut index = DefinitionIndex::new();
        let temp_dir = env::temp_dir();
        let file_path = temp_dir.join("user.forth").to_string_lossy().to_string();
        let file_uri = crate::utils::uri_helpers::path_str_to_uri(&file_path)
            .unwrap()
            .to_string();

        // Define a word that exists in built-ins
        let rope = Rope::from_str(": DUP 1 + ;");
        index.update_file(&file_uri, &rope);

        let mut files = HashMap::new();
        files.insert(file_uri.clone(), rope);

        let result = get_hover_result("DUP", &words, Some(&index), Some(&files), true);

        assert!(result.is_some());
        let hover = result.unwrap();
        if let lsp_types::HoverContents::Markup(content) = hover.contents {
            // Should show user-defined info with source code, not built-in docs
            assert!(content.value.contains("DUP"));
            assert!(content.value.contains("Defined in:"));
            assert!(content.value.contains(": DUP 1 + ;"));
            assert!(!content.value.contains("( x -- x x )"));
        } else {
            panic!("Expected Markup hover contents");
        }
    }

    #[test]
    fn test_hover_user_defined_word_only() {
        use crate::utils::definition_index::DefinitionIndex;
        use ropey::Rope;
        use std::env;

        let words = Words::default();
        let mut index = DefinitionIndex::new();
        let temp_dir = env::temp_dir();
        let file_path = temp_dir.join("user.forth").to_string_lossy().to_string();
        let file_uri = crate::utils::uri_helpers::path_str_to_uri(&file_path)
            .unwrap()
            .to_string();

        // Define a word that doesn't exist in built-ins
        let rope = Rope::from_str(": myword 1 + ;");
        index.update_file(&file_uri, &rope);

        let mut files = HashMap::new();
        files.insert(file_uri.clone(), rope);

        let result = get_hover_result("myword", &words, Some(&index), Some(&files), true);

        assert!(result.is_some());
        let hover = result.unwrap();
        if let lsp_types::HoverContents::Markup(content) = hover.contents {
            assert!(content.value.contains("myword"));
            assert!(content.value.contains("Defined in:"));
            assert!(content.value.contains(": myword 1 + ;"));
        } else {
            panic!("Expected Markup hover contents");
        }
    }

    #[test]
    fn test_hover_user_defined_variable() {
        use crate::utils::definition_index::DefinitionIndex;
        use ropey::Rope;
        use std::env;

        let words = Words::default();
        let mut index = DefinitionIndex::new();
        let temp_dir = env::temp_dir();
        let file_path = temp_dir.join("user.forth").to_string_lossy().to_string();
        let file_uri = crate::utils::uri_helpers::path_str_to_uri(&file_path)
            .unwrap()
            .to_string();

        // Define a variable
        let rope = Rope::from_str("VARIABLE counter");
        index.update_file(&file_uri, &rope);

        let mut files = HashMap::new();
        files.insert(file_uri.clone(), rope);

        let result = get_hover_result("counter", &words, Some(&index), Some(&files), true);

        assert!(result.is_some());
        let hover = result.unwrap();
        if let lsp_types::HoverContents::Markup(content) = hover.contents {
            assert!(content.value.contains("counter"));
            assert!(content.value.contains("Defined in:"));
            assert!(content.value.contains("VARIABLE counter"));
        } else {
            panic!("Expected Markup hover contents");
        }
    }

    #[test]
    fn test_hover_user_defined_multiline() {
        use crate::utils::definition_index::DefinitionIndex;
        use ropey::Rope;
        use std::env;

        let words = Words::default();
        let mut index = DefinitionIndex::new();
        let temp_dir = env::temp_dir();
        let file_path = temp_dir.join("user.forth").to_string_lossy().to_string();
        let file_uri = crate::utils::uri_helpers::path_str_to_uri(&file_path)
            .unwrap()
            .to_string();

        // Define a multiline word
        let rope = Rope::from_str(
            ": factorial\n  dup 0= if\n    drop 1\n  else\n    dup 1- factorial *\n  then\n;",
        );
        index.update_file(&file_uri, &rope);

        let mut files = HashMap::new();
        files.insert(file_uri.clone(), rope);

        let result = get_hover_result("factorial", &words, Some(&index), Some(&files), true);

        assert!(result.is_some());
        let hover = result.unwrap();
        if let lsp_types::HoverContents::Markup(content) = hover.contents {
            assert!(content.value.contains("factorial"));
            assert!(content.value.contains("Defined in:"));
            assert!(content.value.contains(": factorial"));
            assert!(content.value.contains("dup 0= if"));
        } else {
            panic!("Expected Markup hover contents");
        }
    }

    #[test]
    fn test_hover_multibyte_utf8_file() {
        use crate::utils::definition_index::DefinitionIndex;
        use crate::utils::ropey::word_on_or_before::WordOnOrBefore;
        use ropey::Rope;

        // Simulate a Forth file with Italian comments (multi-byte UTF-8)
        let src = "\\ tabella è unica\r\n: SAVE_BATT ( -- ) ;\r\n\\ così il test\r\n";
        let rope = Rope::from_str(src);

        let mut index = DefinitionIndex::new();
        let file_uri = "file:///test/test.f".to_string();
        index.update_file(&file_uri, &rope);

        let mut files = HashMap::new();
        files.insert(file_uri, rope.clone());
        let words = Words::default();

        for line in 0..rope.len_lines() {
            let line_start = rope.line_to_char(line);
            let line_end = if line + 1 < rope.len_lines() {
                rope.line_to_char(line + 1)
            } else {
                rope.len_chars()
            };
            let line_len = line_end - line_start;
            for character in [0, line_len / 2, line_len.saturating_sub(1)] {
                let ix = line_start + character;
                if ix < rope.len_chars() {
                    let word = rope.word_on_or_before(ix);
                    let _ = get_hover_result(
                        &word.to_string(),
                        &words,
                        Some(&index),
                        Some(&files),
                        true,
                    );
                }
            }
        }
    }

    #[test]
    fn test_hover_doc_comments_with_stack_effect() {
        use crate::utils::definition_index::DefinitionIndex;
        use ropey::Rope;

        let words = Words::default();
        let mut index = DefinitionIndex::new();
        let file_uri = "file:///test/test.f".to_string();
        let src =
            "42 constant FOO\n\\ doc comment for my-word\n: my-word ( 0 -- 1 )\n  foo\n  bar ;";
        index.update_file(&file_uri, &Rope::from_str(src));

        let result = get_hover_result("my-word", &words, Some(&index), None, true);
        assert!(result.is_some());
        if let lsp_types::HoverContents::Markup(content) = result.unwrap().contents {
            assert_eq!(
                content.value,
                "my-word: defined at test.f:3\n( 0 -- 1 )\ndoc comment for my-word"
            );
        } else {
            panic!("Expected Markup hover contents");
        }
    }

    #[test]
    fn test_hover_doc_comments_without_stack_effect() {
        use crate::utils::definition_index::DefinitionIndex;
        use ropey::Rope;

        let words = Words::default();
        let mut index = DefinitionIndex::new();
        let file_uri = "file:///test/test.f".to_string();
        let src = ": prev-word\n  foo\n  bar ;\n\\ doc-comment for qux\nvariable qux";
        index.update_file(&file_uri, &Rope::from_str(src));

        let result = get_hover_result("qux", &words, Some(&index), None, true);
        assert!(result.is_some());
        if let lsp_types::HoverContents::Markup(content) = result.unwrap().contents {
            assert_eq!(
                content.value,
                "qux: defined at test.f:5\ndoc-comment for qux"
            );
        } else {
            panic!("Expected Markup hover contents");
        }
    }

    #[test]
    fn test_hover_doc_comments_multiline() {
        use crate::utils::definition_index::DefinitionIndex;
        use ropey::Rope;

        let words = Words::default();
        let mut index = DefinitionIndex::new();
        let file_uri = "file:///test/test.f".to_string();
        let src = "\\ here comes\n\\ some comment block\n\\ documenting word\n: word ( a b -- c )\n  ... ;";
        index.update_file(&file_uri, &Rope::from_str(src));

        let result = get_hover_result("word", &words, Some(&index), None, true);
        assert!(result.is_some());
        if let lsp_types::HoverContents::Markup(content) = result.unwrap().contents {
            assert_eq!(
                content.value,
                "word: defined at test.f:4\n( a b -- c )\nhere comes some comment block documenting word"
            );
        } else {
            panic!("Expected Markup hover contents");
        }
    }

    #[test]
    fn test_hover_doc_comments_disabled() {
        use crate::utils::definition_index::DefinitionIndex;
        use ropey::Rope;

        let words = Words::default();
        let mut index = DefinitionIndex::new();
        let file_uri = "file:///test/test.f".to_string();
        let rope = Rope::from_str("\\ here comes\n\\ doc comment\n: word ( a b -- c )\n  ... ;");
        index.update_file(&file_uri, &rope);

        let mut files = HashMap::new();
        files.insert(file_uri.clone(), rope);

        // With doc_comments = false, should fall back to existing format
        let result = get_hover_result("word", &words, Some(&index), Some(&files), false);
        assert!(result.is_some());
        if let lsp_types::HoverContents::Markup(content) = result.unwrap().contents {
            assert!(content.value.contains("### `word`"));
            assert!(content.value.contains("Defined in:"));
            assert!(content.value.contains("```forth"));
        } else {
            panic!("Expected Markup hover contents");
        }
    }

    #[test]
    fn test_hover_cross_file_doc_comments() {
        use crate::utils::definition_index::DefinitionIndex;
        use ropey::Rope;

        let words = Words::default();
        let mut index = DefinitionIndex::new();
        let lib_uri = "file:///lib/math.f".to_string();
        let src_lib = "\\ adds two numbers\n: add2 ( a b -- c ) + ;";
        index.update_file(&lib_uri, &Rope::from_str(src_lib));

        // Hover in another file (files map doesn't even have to have the other file loaded in editor)
        let result = get_hover_result("add2", &words, Some(&index), None, true);
        assert!(result.is_some());
        if let lsp_types::HoverContents::Markup(content) = result.unwrap().contents {
            assert_eq!(
                content.value,
                "add2: defined at math.f:2\n( a b -- c )\nadds two numbers"
            );
        } else {
            panic!("Expected Markup hover contents");
        }
    }

    #[test]
    fn test_hover_doc_comments_no_duplicates() {
        use crate::utils::definition_index::DefinitionIndex;
        use ropey::Rope;

        let words = Words::default();
        let mut index = DefinitionIndex::new();
        let file_path = "file://C:\\test\\test.f";
        let src = "\\ doc comment\n: my-word ( -- )\n  noop ;";
        let rope = Rope::from_str(src);
        index.update_file(file_path, &rope);
        // Simulate client didOpen updating the same file with a slightly different URI representation
        let file_uri = "file:///c%3A/test/test.f";
        index.update_file(file_uri, &rope);

        let result = get_hover_result("my-word", &words, Some(&index), None, true);
        assert!(result.is_some());
        if let lsp_types::HoverContents::Markup(content) = result.unwrap().contents {
            assert_eq!(
                content.value,
                "my-word: defined at test.f:2\n( -- )\ndoc comment"
            );
            // Must NOT contain duplicate sections joined by "---"
            assert!(!content.value.contains("---"));
        } else {
            panic!("Expected Markup hover contents");
        }
    }

    #[test]
    fn test_hover_constant_with_doc_comment() {
        use crate::utils::definition_index::DefinitionIndex;
        use ropey::Rope;

        let words = Words::default();
        let mut index = DefinitionIndex::new();
        let file_uri = "file:///test/test.f".to_string();
        let src = "\\ Left arrow key\n8 constant LEFT\n21 constant RIGHT";
        index.update_file(&file_uri, &Rope::from_str(src));

        let result = get_hover_result("LEFT", &words, Some(&index), None, true);
        assert!(result.is_some());
        if let lsp_types::HoverContents::Markup(content) = result.unwrap().contents {
            assert_eq!(
                content.value,
                "LEFT = 8: defined at test.f:2\nLeft arrow key"
            );
        } else {
            panic!("Expected Markup hover contents");
        }
    }

    #[test]
    fn test_hover_constant_without_doc_comment() {
        use crate::utils::definition_index::DefinitionIndex;
        use ropey::Rope;

        let words = Words::default();
        let mut index = DefinitionIndex::new();
        let file_uri = "file:///test/test.f".to_string();
        let src = "8 constant LEFT\n21 constant RIGHT";
        let rope = Rope::from_str(src);
        index.update_file(&file_uri, &rope);

        let mut files = HashMap::new();
        files.insert(file_uri.clone(), rope);

        let result = get_hover_result("LEFT", &words, Some(&index), Some(&files), true);
        assert!(result.is_some());
        if let lsp_types::HoverContents::Markup(content) = result.unwrap().contents {
            assert!(content.value.contains("### `LEFT = 8`"));
            assert!(content.value.contains("Defined in:"));
            assert!(content.value.contains("```forth\n8 constant LEFT\n```"));
            // Must NOT contain the next line (RIGHT)
            assert!(!content.value.contains("RIGHT"));
        } else {
            panic!("Expected Markup hover contents");
        }
    }

    #[test]
    fn test_hover_create_and_variable_without_doc_comment() {
        use crate::utils::definition_index::DefinitionIndex;
        use ropey::Rope;

        let words = Words::default();
        let mut index = DefinitionIndex::new();
        let file_uri = "file:///test/test.f".to_string();
        let src = "create (vdu-buf) 512 allot\n\nvariable (vdu-fid)\n\n: (emit-buffer) ( addr len -- )\n  bounds do\n    i c@ emit\n  loop ;\n";
        let rope = Rope::from_str(src);
        index.update_file(&file_uri, &rope);

        let mut files = HashMap::new();
        files.insert(file_uri.clone(), rope);

        // Hover for (vdu-buf) should show only the create line
        let result_buf = get_hover_result("(vdu-buf)", &words, Some(&index), Some(&files), true);
        assert!(result_buf.is_some());
        if let lsp_types::HoverContents::Markup(content) = result_buf.unwrap().contents {
            assert!(
                content
                    .value
                    .contains("```forth\ncreate (vdu-buf) 512 allot\n```")
            );
            assert!(!content.value.contains("variable"));
            assert!(!content.value.contains("(vdu-fid)"));
            assert!(!content.value.contains("(emit-buffer)"));
            assert!(!content.value.contains("loop ;"));
        } else {
            panic!("Expected Markup hover contents");
        }

        // Hover for (vdu-fid) should show only the variable line
        let result_fid = get_hover_result("(vdu-fid)", &words, Some(&index), Some(&files), true);
        assert!(result_fid.is_some());
        if let lsp_types::HoverContents::Markup(content) = result_fid.unwrap().contents {
            assert!(content.value.contains("```forth\nvariable (vdu-fid)\n```"));
            assert!(!content.value.contains("create"));
            assert!(!content.value.contains("(vdu-buf)"));
            assert!(!content.value.contains("(emit-buffer)"));
            assert!(!content.value.contains("loop ;"));
        } else {
            panic!("Expected Markup hover contents");
        }

        // Hover for (emit-buffer) should show the full colon definition up to ';'
        let result_emit =
            get_hover_result("(emit-buffer)", &words, Some(&index), Some(&files), true);
        assert!(result_emit.is_some());
        if let lsp_types::HoverContents::Markup(content) = result_emit.unwrap().contents {
            assert!(content.value.contains(": (emit-buffer) ( addr len -- )"));
            assert!(content.value.contains("bounds do"));
            assert!(content.value.contains("loop ;"));
            assert!(!content.value.contains("create"));
            assert!(!content.value.contains("variable"));
        } else {
            panic!("Expected Markup hover contents");
        }
    }
}
