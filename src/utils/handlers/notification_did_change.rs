#[allow(unused_imports)]
use crate::prelude::*;
use crate::utils::definition_index::DefinitionIndex;
use crate::utils::diagnostics::{get_diagnostics_from_tokens, publish_diagnostics};
use crate::words::Words;

use std::collections::HashMap;

use forth_lexer::parser::Lexer;
use lsp_server::{Connection, Notification};
use ropey::Rope;

use super::cast_notification;

use std::path::PathBuf;

use crate::config::WorkspaceConfig;
use crate::utils::include_resolver::resolve_and_load_dependencies;
use crate::utils::uri_helpers::uri_to_path;

pub fn handle_did_change_text_document(
    notification: &Notification,
    connection: &Connection,
    files: &mut HashMap<String, Rope>,
    def_index: &mut DefinitionIndex,
    builtin_words: &Words,
    include_dirs: &[PathBuf],
    workspace: &WorkspaceConfig,
) -> Result<()> {
    match cast_notification::<lsp_types::notification::DidChangeTextDocument>(notification.clone())
    {
        Ok(params) => {
            let file_uri = params.text_document.uri.to_string();
            log_debug!("DidChange for file: {}", file_uri);
            let source = {
                let rope = files
                    .get_mut(&file_uri)
                    .expect("Must be able to get rope for lang");
                log_debug!(
                    "Processing {} content changes",
                    params.content_changes.len()
                );
                for change in params.content_changes {
                    log_debug!(
                        "Change range: {:?}, text length: {}",
                        change.range,
                        change.text.len()
                    );
                    if let Some(range) = change.range {
                        // Incremental change - update specific range
                        let start_line = range.start.line as usize;
                        let end_line = range.end.line as usize;
                        if start_line >= rope.len_lines() || end_line >= rope.len_lines() {
                            // Out-of-bounds range, fall back to full document sync
                            *rope = Rope::from_str(change.text.as_str());
                            continue;
                        }
                        let start = rope.line_to_char(start_line) + range.start.character as usize;
                        let end = rope.line_to_char(end_line) + range.end.character as usize;
                        rope.remove(start..end);
                        rope.insert(start, change.text.as_str());
                    } else {
                        // Full document sync - replace entire rope
                        *rope = Rope::from_str(change.text.as_str());
                    }
                }
                log_debug!("After changes, rope has {} chars", rope.len_chars());
                rope.to_string()
            };

            // Parse once and reuse tokens for both indexing and diagnostics
            let tokens = Lexer::new(&source).parse();

            let file_path = uri_to_path(&params.text_document.uri);
            resolve_and_load_dependencies(
                &tokens,
                file_path.as_deref(),
                include_dirs,
                files,
                def_index,
                workspace,
            );

            log_debug!("Updating definition index for: {}", file_uri);
            let rope = files
                .get(&file_uri)
                .expect("Must be able to get rope for lang");
            def_index.update_file_from_tokens(&file_uri, &tokens, rope);

            let diagnostics =
                get_diagnostics_from_tokens(&tokens, &source, rope, def_index, builtin_words);
            publish_diagnostics(
                connection,
                params.text_document.uri.clone(),
                diagnostics,
                params.text_document.version,
            )?;

            Ok(())
        }
        Err(err) => {
            log_handler_error!("Did change notification", err);
            Err(err)
        }
    }
}
