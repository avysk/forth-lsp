use std::collections::HashMap;

use forth_lexer::{
    parser::Lexer,
    token::{Data, Token},
};
use lsp_types::{Location, Range};
use ropey::Rope;

use super::{
    data_to_position::{ToPosition, data_range_from_to},
    definition_helpers::find_colon_definitions,
    token_utils::extract_word_name_with_range,
};

/// Details of a word definition in the index
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefinitionDetails {
    pub file_path_or_uri: String,
    pub range: Range,
    pub stack_effect: Option<String>,
    pub doc_comment: Option<String>,
    pub constant_value: Option<String>,
    pub is_colon_definition: bool,
}

/// Index of all word definitions and references across the workspace
pub struct DefinitionIndex {
    /// Maps lowercase word names to their definition details
    /// Key: word name (lowercase for case-insensitive lookup)
    definitions: HashMap<String, Vec<DefinitionDetails>>,
    /// Maps lowercase word names to their reference (usage) locations
    /// Key: word name (lowercase for case-insensitive lookup)
    /// Value: list of (file_path, range) tuples
    references: HashMap<String, Vec<(String, Range)>>,
}

impl DefinitionIndex {
    pub fn new() -> Self {
        Self {
            definitions: HashMap::new(),
            references: HashMap::new(),
        }
    }

    /// Update the index for a specific file
    /// This removes all old definitions and references from the file and adds new ones
    pub fn update_file(&mut self, file_path: &str, rope: &Rope) {
        let progn = rope.to_string();
        let mut lexer = Lexer::new(progn.as_str());
        let tokens = lexer.parse();
        self.update_file_from_tokens(file_path, &tokens, rope);
    }

    /// Update the index for a specific file using pre-parsed tokens
    /// This avoids re-parsing when tokens are already available
    pub fn update_file_from_tokens(&mut self, file_path: &str, tokens: &[Token], rope: &Rope) {
        // First, remove all definitions and references from this file
        self.remove_file(file_path);

        // Track which token indices are part of word definitions (to exclude from references)
        let mut definition_token_indices = std::collections::HashSet::new();

        // First pass: collect colon definitions
        for result in find_colon_definitions(tokens) {
            if result.len() >= 2 {
                // Mark the word name token(s) as part of definition
                if let Token::Number(num_data) = &result[1] {
                    definition_token_indices.insert(num_data.start);
                } else if let Token::Word(data) = &result[1] {
                    definition_token_indices.insert(data.start);
                }

                let Some((name, selection_start, selection_end)) =
                    extract_word_name_with_range(result, 1)
                else {
                    continue;
                };

                // Use the name's range, not the entire definition block
                let name_data = Data::new(selection_start, selection_end, "");
                let range = data_range_from_to(&name_data, &name_data, rope);

                // Extract doc comments directly preceding ':'
                let colon_data = result[0].get_data();
                let colon_line = rope.char_to_line(colon_data.start);
                let doc_comment = extract_doc_comment_block(colon_line, rope);

                // Immediate stack effect following the word name immediately (same or next line)
                let word_line = rope.char_to_line(selection_start);
                let stack_effect = if let Some(Token::StackComment(stack_data)) = result.get(2) {
                    let comment_line = rope.char_to_line(stack_data.start);
                    if comment_line == word_line || comment_line == word_line + 1 {
                        Some(stack_data.value.trim().to_string())
                    } else {
                        None
                    }
                } else {
                    None
                };

                // Store with lowercase key for case-insensitive lookup
                self.definitions
                    .entry(name.to_lowercase())
                    .or_default()
                    .push(DefinitionDetails {
                        file_path_or_uri: file_path.to_string(),
                        range,
                        stack_effect,
                        doc_comment,
                        constant_value: None,
                        is_colon_definition: true,
                    });
            }
        }

        // Second pass: collect defining word definitions (VARIABLE, CONSTANT, CREATE, etc.)
        // These are patterns like: VARIABLE name, CONSTANT name, CREATE name
        let defining_words = [
            "VARIABLE",
            "CONSTANT",
            "CREATE",
            "VALUE",
            "2VARIABLE",
            "2CONSTANT",
            "2VALUE",
            "FVARIABLE",
            "FCONSTANT",
            "DEFER",
            "BUFFER:",
            "CODE",
        ];

        for i in 0..tokens.len().saturating_sub(1) {
            if let Token::Word(defining_word_data) = &tokens[i] {
                // Check if this is a defining word
                if defining_words
                    .iter()
                    .any(|&dw| dw.eq_ignore_ascii_case(defining_word_data.value))
                {
                    // Look for the next word token (skip numbers/comments)
                    if let Some(Token::Word(name_data)) = tokens.get(i + 1) {
                        // Mark the name token as part of a definition
                        definition_token_indices.insert(name_data.start);

                        let name = name_data.value.to_string();

                        // Use only the name's range, not including the defining word
                        let range = name_data.to_range(rope);

                        // Extract doc comments directly preceding the definition
                        let def_line = rope.char_to_line(defining_word_data.start);
                        let doc_comment = extract_doc_comment_block(def_line, rope);

                        let is_constant = ["CONSTANT", "2CONSTANT", "FCONSTANT", "VALUE", "2VALUE"]
                            .iter()
                            .any(|&cw| cw.eq_ignore_ascii_case(defining_word_data.value));

                        let constant_value = if is_constant {
                            extract_constant_value(i, tokens, rope, &definition_token_indices)
                        } else {
                            None
                        };

                        // Immediate stack effect following the word name immediately (same or next line)
                        let word_line = rope.char_to_line(name_data.start);
                        let stack_effect =
                            if let Some(Token::StackComment(stack_data)) = tokens.get(i + 2) {
                                let comment_line = rope.char_to_line(stack_data.start);
                                if comment_line == word_line || comment_line == word_line + 1 {
                                    Some(stack_data.value.trim().to_string())
                                } else {
                                    None
                                }
                            } else {
                                None
                            };

                        // Store with lowercase key for case-insensitive lookup
                        self.definitions
                            .entry(name.to_lowercase())
                            .or_default()
                            .push(DefinitionDetails {
                                file_path_or_uri: file_path.to_string(),
                                range,
                                stack_effect,
                                doc_comment,
                                constant_value,
                                is_colon_definition: false,
                            });
                    }
                }
            }
        }

        // Second pass: collect references (word usages)
        for token in tokens {
            match token {
                Token::Word(data) => {
                    if definition_token_indices.contains(&data.start) {
                        continue;
                    }

                    let range = data.to_range(rope);

                    self.references
                        .entry(data.value.to_lowercase())
                        .or_default()
                        .push((file_path.to_string(), range));
                }
                Token::Number(data) => {
                    if definition_token_indices.contains(&data.start) {
                        continue;
                    }

                    let name = data.value.to_lowercase();

                    // Only add referene if first pass picked up number literal redefine
                    if self.definitions.contains_key(&name) {
                        let range = data.to_range(rope);

                        self.references
                            .entry(data.value.to_lowercase())
                            .or_default()
                            .push((file_path.to_string(), range));
                    }
                }
                _ => {}
            }
        }
    }

    /// Remove all definitions and references from a specific file
    pub fn remove_file(&mut self, file_path: &str) {
        // Remove all definitions that reference this file
        for locations in self.definitions.values_mut() {
            locations.retain(|def| def.file_path_or_uri != file_path);
        }
        self.definitions
            .retain(|_, locations| !locations.is_empty());

        // Remove all references that reference this file
        for locations in self.references.values_mut() {
            locations.retain(|(path, _)| path != file_path);
        }
        self.references.retain(|_, locations| !locations.is_empty());
    }

    /// Find all definitions of a word (case-insensitive)
    pub fn find_definitions(&self, word: &str) -> Vec<Location> {
        let mut locations = Vec::new();

        if let Some(defs) = self.definitions.get(&word.to_lowercase()) {
            let mut seen = std::collections::HashSet::new();
            for def in defs {
                let file_name = def
                    .file_path_or_uri
                    .split(['/', '\\'])
                    .next_back()
                    .unwrap_or("")
                    .to_string();
                if !seen.insert((file_name, def.range.start.line, def.range.start.character)) {
                    continue;
                }
                if let Some(uri) = crate::utils::uri_helpers::path_str_to_uri(&def.file_path_or_uri)
                {
                    locations.push(Location {
                        uri,
                        range: def.range,
                    });
                }
            }
        }

        locations
    }

    /// Find definition details for a word (case-insensitive)
    pub fn find_definition_details(&self, word: &str) -> Vec<DefinitionDetails> {
        let mut defs = self
            .definitions
            .get(&word.to_lowercase())
            .cloned()
            .unwrap_or_default();
        let mut seen = std::collections::HashSet::new();
        defs.retain(|d| {
            let file_name = d
                .file_path_or_uri
                .split(['/', '\\'])
                .next_back()
                .unwrap_or("")
                .to_string();
            seen.insert((file_name, d.range.start.line, d.range.start.character))
        });
        defs
    }

    /// Find the stack effect for the first definition of a word (case-insensitive)
    pub fn find_stack_effect(&self, word: &str) -> Option<String> {
        self.definitions
            .get(&word.to_lowercase())?
            .iter()
            .find_map(|def| def.stack_effect.clone())
    }

    /// Find all references (usages) of a word (case-insensitive)
    pub fn find_references(&self, word: &str) -> Vec<Location> {
        let mut locations = Vec::new();

        if let Some(refs) = self.references.get(&word.to_lowercase()) {
            for (file_path_or_uri, range) in refs {
                if let Some(uri) = crate::utils::uri_helpers::path_str_to_uri(file_path_or_uri) {
                    locations.push(Location { uri, range: *range });
                }
            }
        }

        locations
    }

    /// Find all references to a word, including its definition (case-insensitive)
    /// This is useful for the LSP "Find References" command which typically includes the definition
    pub fn find_all_references(&self, word: &str, include_declaration: bool) -> Vec<Location> {
        let mut locations = Vec::new();

        // Add definition if requested
        if include_declaration {
            locations.extend(self.find_definitions(word));
        }

        // Add all usages
        locations.extend(self.find_references(word));

        locations
    }

    /// Get all word names in the index (useful for completion)
    pub fn all_words(&self) -> Vec<String> {
        self.definitions.keys().cloned().collect()
    }
}

impl Default for DefinitionIndex {
    fn default() -> Self {
        Self::new()
    }
}

/// Extracts the value expression preceding a constant or value definition.
fn extract_constant_value(
    i: usize,
    tokens: &[Token],
    rope: &Rope,
    definition_token_indices: &std::collections::HashSet<usize>,
) -> Option<String> {
    if i == 0 {
        return None;
    }
    let def_line = rope.char_to_line(tokens[i].get_data().start);
    let mut start_idx = i;
    for k in (0..i).rev() {
        let tok = &tokens[k];
        let tok_data = tok.get_data();
        let tok_line = rope.char_to_line(tok_data.start);
        if definition_token_indices.contains(&tok_data.start)
            || matches!(tok, Token::Semicolon(_) | Token::Colon(_))
        {
            break;
        }
        if tok_line != def_line && start_idx != i {
            break;
        }
        start_idx = k;
        if tok_line != def_line {
            break;
        }
    }
    if start_idx < i {
        let first_start = tokens[start_idx].get_data().start;
        let last_end = tokens[i - 1].get_data().end;
        if first_start < last_end && last_end <= rope.len_chars() {
            let slice = rope.slice(first_start..last_end).to_string();
            let trimmed = slice.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

/// Extracts the doc-comment block directly preceding `def_line`.
/// Scans upward line-by-line starting at `def_line - 1`.
/// Stops as soon as a line does not start with `\` (e.g. previous definition, blank line, code).
/// Strips leading `\ ` (or `\` if empty), trims trailing `\r` and whitespace,
/// and joins the lines with space `" "`.
fn extract_doc_comment_block(def_line: usize, rope: &Rope) -> Option<String> {
    if def_line == 0 {
        return None;
    }
    let mut comment_lines = Vec::new();
    let mut line_idx = def_line - 1;
    loop {
        let line = rope.line(line_idx);
        let line_str = line.to_string();
        let trimmed = line_str.trim();
        if trimmed.starts_with('\\') {
            let content = if let Some(stripped) = trimmed.strip_prefix(r"\ ") {
                stripped
            } else if trimmed == "\\" {
                ""
            } else if let Some(stripped) = trimmed.strip_prefix('\\') {
                stripped
            } else {
                trimmed
            };
            comment_lines.push(content.to_string());
            if line_idx == 0 {
                break;
            }
            line_idx -= 1;
        } else {
            break;
        }
    }
    if comment_lines.is_empty() {
        None
    } else {
        comment_lines.reverse();
        Some(comment_lines.join(" "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    #[test]
    fn test_index_single_file() {
        let mut index = DefinitionIndex::new();
        let temp_dir = env::temp_dir();
        let file_path = temp_dir.join("test.forth").to_string_lossy().to_string();

        let rope = Rope::from_str(": add1 1 + ;\n: double 2 * ;");
        index.update_file(&file_path, &rope);

        let locations = index.find_definitions("add1");
        assert_eq!(locations.len(), 1);
        assert!(locations[0].uri.to_string().contains("test.forth"));
    }

    #[test]
    fn test_index_case_insensitive() {
        let mut index = DefinitionIndex::new();
        let temp_dir = env::temp_dir();
        let file_path = temp_dir.join("test.forth").to_string_lossy().to_string();

        let rope = Rope::from_str(": MyWord 1 + ;");
        index.update_file(&file_path, &rope);

        let upper = index.find_definitions("MYWORD");
        let lower = index.find_definitions("myword");
        let mixed = index.find_definitions("MyWord");

        assert_eq!(upper.len(), 1);
        assert_eq!(lower.len(), 1);
        assert_eq!(mixed.len(), 1);
        assert_eq!(upper[0].range, lower[0].range);
        assert_eq!(upper[0].range, mixed[0].range);
    }

    #[test]
    fn test_index_multiple_files() {
        let mut index = DefinitionIndex::new();
        let temp_dir = env::temp_dir();
        let file1 = temp_dir.join("file1.forth").to_string_lossy().to_string();
        let file2 = temp_dir.join("file2.forth").to_string_lossy().to_string();

        index.update_file(&file1, &Rope::from_str(": add1 1 + ;"));
        index.update_file(&file2, &Rope::from_str(": double 2 * ;"));

        let add1_locs = index.find_definitions("add1");
        let double_locs = index.find_definitions("double");

        assert_eq!(add1_locs.len(), 1);
        assert_eq!(double_locs.len(), 1);
        assert!(add1_locs[0].uri.to_string().contains("file1.forth"));
        assert!(double_locs[0].uri.to_string().contains("file2.forth"));
    }

    #[test]
    fn test_index_duplicate_definitions() {
        let mut index = DefinitionIndex::new();
        let temp_dir = env::temp_dir();
        let file1 = temp_dir.join("file1.forth").to_string_lossy().to_string();
        let file2 = temp_dir.join("file2.forth").to_string_lossy().to_string();

        index.update_file(&file1, &Rope::from_str(": test 1 + ;"));
        index.update_file(&file2, &Rope::from_str(": test 2 * ;"));

        let locations = index.find_definitions("test");
        assert_eq!(locations.len(), 2);
    }

    #[test]
    fn test_index_update_file() {
        let mut index = DefinitionIndex::new();
        let temp_dir = env::temp_dir();
        let file_path = temp_dir.join("test.forth").to_string_lossy().to_string();

        // Initial content
        index.update_file(&file_path, &Rope::from_str(": old 1 + ;"));
        assert_eq!(index.find_definitions("old").len(), 1);
        assert_eq!(index.find_definitions("new").len(), 0);

        // Update content
        index.update_file(&file_path, &Rope::from_str(": new 2 * ;"));
        assert_eq!(index.find_definitions("old").len(), 0);
        assert_eq!(index.find_definitions("new").len(), 1);
    }

    #[test]
    fn test_index_remove_file() {
        let mut index = DefinitionIndex::new();
        let temp_dir = env::temp_dir();
        let file1 = temp_dir.join("file1.forth").to_string_lossy().to_string();
        let file2 = temp_dir.join("file2.forth").to_string_lossy().to_string();

        index.update_file(&file1, &Rope::from_str(": word1 1 + ;"));
        index.update_file(&file2, &Rope::from_str(": word2 2 * ;"));

        assert_eq!(index.find_definitions("word1").len(), 1);
        assert_eq!(index.find_definitions("word2").len(), 1);

        index.remove_file(&file1);

        assert_eq!(index.find_definitions("word1").len(), 0);
        assert_eq!(index.find_definitions("word2").len(), 1);
    }

    #[test]
    fn test_index_number_prefix_words() {
        let mut index = DefinitionIndex::new();
        let temp_dir = env::temp_dir();
        let file_path = temp_dir.join("test.forth").to_string_lossy().to_string();

        let rope = Rope::from_str(": 2swap ( a b c d -- c d a b ) rot >r rot r> ;");
        index.update_file(&file_path, &rope);

        let locations = index.find_definitions("2swap");
        assert_eq!(locations.len(), 1);
    }

    #[test]
    fn test_index_not_found() {
        let index = DefinitionIndex::new();
        let locations = index.find_definitions("nonexistent");
        assert_eq!(locations.len(), 0);
    }

    #[test]
    fn test_all_words() {
        let mut index = DefinitionIndex::new();
        let temp_dir = env::temp_dir();
        let file_path = temp_dir.join("test.forth").to_string_lossy().to_string();

        index.update_file(&file_path, &Rope::from_str(": add1 1 + ;\n: double 2 * ;"));

        let words = index.all_words();
        assert_eq!(words.len(), 2);
        assert!(words.contains(&"add1".to_string()));
        assert!(words.contains(&"double".to_string()));
    }

    #[test]
    fn test_find_references_simple() {
        let mut index = DefinitionIndex::new();
        let temp_dir = env::temp_dir();
        let file_path = temp_dir.join("test.forth").to_string_lossy().to_string();

        // Define a word and use it
        index.update_file(
            &file_path,
            &Rope::from_str(": add1 1 + ;\n: test add1 add1 ;"),
        );

        let refs = index.find_references("add1");
        // Should find 2 references (both calls in test word)
        assert_eq!(refs.len(), 2);
    }

    #[test]
    fn test_find_references_excludes_definition() {
        let mut index = DefinitionIndex::new();
        let temp_dir = env::temp_dir();
        let file_path = temp_dir.join("test.forth").to_string_lossy().to_string();

        index.update_file(&file_path, &Rope::from_str(": myword 1 + ;"));

        let refs = index.find_references("myword");
        // Should find 0 references (definition is not a reference)
        assert_eq!(refs.len(), 0);
    }

    #[test]
    fn test_find_references_case_insensitive() {
        let mut index = DefinitionIndex::new();
        let temp_dir = env::temp_dir();
        let file_path = temp_dir.join("test.forth").to_string_lossy().to_string();

        index.update_file(&file_path, &Rope::from_str(": add1 1 + ;\n: test ADD1 ;"));

        let refs = index.find_references("add1");
        assert_eq!(refs.len(), 1);

        let refs_upper = index.find_references("ADD1");
        assert_eq!(refs_upper.len(), 1);
    }

    #[test]
    fn test_find_references_multiple_files() {
        let mut index = DefinitionIndex::new();
        let temp_dir = env::temp_dir();
        let file1 = temp_dir.join("file1.forth").to_string_lossy().to_string();
        let file2 = temp_dir.join("file2.forth").to_string_lossy().to_string();

        index.update_file(&file1, &Rope::from_str(": add1 1 + ;"));
        index.update_file(&file2, &Rope::from_str(": test add1 add1 ;"));

        let refs = index.find_references("add1");
        // Should find 2 references in file2
        assert_eq!(refs.len(), 2);
        assert!(
            refs.iter()
                .all(|loc| loc.uri.to_string().contains("file2.forth"))
        );
    }

    #[test]
    fn test_find_all_references_with_declaration() {
        let mut index = DefinitionIndex::new();
        let temp_dir = env::temp_dir();
        let file_path = temp_dir.join("test.forth").to_string_lossy().to_string();

        index.update_file(&file_path, &Rope::from_str(": add1 1 + ;\n: test add1 ;"));

        let all_refs = index.find_all_references("add1", true);
        // Should find 1 definition + 1 reference = 2 total
        assert_eq!(all_refs.len(), 2);
    }

    #[test]
    fn test_find_all_references_without_declaration() {
        let mut index = DefinitionIndex::new();
        let temp_dir = env::temp_dir();
        let file_path = temp_dir.join("test.forth").to_string_lossy().to_string();

        index.update_file(&file_path, &Rope::from_str(": add1 1 + ;\n: test add1 ;"));

        let all_refs = index.find_all_references("add1", false);
        // Should find only 1 reference (no definition)
        assert_eq!(all_refs.len(), 1);
    }

    #[test]
    fn test_find_references_builtin_words() {
        let mut index = DefinitionIndex::new();
        let temp_dir = env::temp_dir();
        let file_path = temp_dir.join("test.forth").to_string_lossy().to_string();

        // Use built-in words that aren't defined in the file
        index.update_file(&file_path, &Rope::from_str(": test + - * ;"));

        let plus_refs = index.find_references("+");
        let minus_refs = index.find_references("-");
        let mul_refs = index.find_references("*");

        // Should find references to built-in words
        assert_eq!(plus_refs.len(), 1);
        assert_eq!(minus_refs.len(), 1);
        assert_eq!(mul_refs.len(), 1);
    }

    #[test]
    fn test_find_references_no_matches() {
        let mut index = DefinitionIndex::new();
        let temp_dir = env::temp_dir();
        let file_path = temp_dir.join("test.forth").to_string_lossy().to_string();

        index.update_file(&file_path, &Rope::from_str(": add1 1 + ;"));

        let refs = index.find_references("nonexistent");
        assert_eq!(refs.len(), 0);
    }

    #[test]
    fn test_variable_definition() {
        let mut index = DefinitionIndex::new();
        let temp_dir = env::temp_dir();
        let file_path = temp_dir.join("test.forth").to_string_lossy().to_string();

        index.update_file(&file_path, &Rope::from_str("VARIABLE DATE"));

        let defs = index.find_definitions("DATE");
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].range.start.line, 0);
        // Range should be for "DATE" (starts at character 9), not "VARIABLE"
        assert_eq!(defs[0].range.start.character, 9);
    }

    #[test]
    fn test_constant_definition() {
        let mut index = DefinitionIndex::new();
        let temp_dir = env::temp_dir();
        let file_path = temp_dir.join("test.forth").to_string_lossy().to_string();

        index.update_file(&file_path, &Rope::from_str("42 CONSTANT ANSWER"));

        let defs = index.find_definitions("ANSWER");
        assert_eq!(defs.len(), 1);
    }

    #[test]
    fn test_create_definition() {
        let mut index = DefinitionIndex::new();
        let temp_dir = env::temp_dir();
        let file_path = temp_dir.join("test.forth").to_string_lossy().to_string();

        index.update_file(&file_path, &Rope::from_str("CREATE BUFFER 100 ALLOT"));

        let defs = index.find_definitions("BUFFER");
        assert_eq!(defs.len(), 1);
    }

    #[test]
    fn test_value_definition() {
        let mut index = DefinitionIndex::new();
        let temp_dir = env::temp_dir();
        let file_path = temp_dir.join("test.forth").to_string_lossy().to_string();

        index.update_file(&file_path, &Rope::from_str("10 VALUE counter"));

        let defs = index.find_definitions("counter");
        assert_eq!(defs.len(), 1);
    }

    #[test]
    fn test_multiple_defining_words() {
        let mut index = DefinitionIndex::new();
        let temp_dir = env::temp_dir();
        let file_path = temp_dir.join("test.forth").to_string_lossy().to_string();

        index.update_file(
            &file_path,
            &Rope::from_str("VARIABLE x\n10 CONSTANT max\nCREATE buf 100 ALLOT\n: double 2 * ;"),
        );

        assert_eq!(index.find_definitions("x").len(), 1);
        assert_eq!(index.find_definitions("max").len(), 1);
        assert_eq!(index.find_definitions("buf").len(), 1);
        assert_eq!(index.find_definitions("double").len(), 1);
    }

    #[test]
    fn test_defining_word_case_insensitive() {
        let mut index = DefinitionIndex::new();
        let temp_dir = env::temp_dir();
        let file_path = temp_dir.join("test.forth").to_string_lossy().to_string();

        // Test with lowercase variable keyword
        index.update_file(&file_path, &Rope::from_str("variable myvar"));

        let defs = index.find_definitions("myvar");
        assert_eq!(defs.len(), 1);
    }

    #[test]
    fn test_2variable_definition() {
        let mut index = DefinitionIndex::new();
        let temp_dir = env::temp_dir();
        let file_path = temp_dir.join("test.forth").to_string_lossy().to_string();

        index.update_file(&file_path, &Rope::from_str("2VARIABLE dval"));

        let defs = index.find_definitions("dval");
        assert_eq!(defs.len(), 1);
    }

    #[test]
    fn test_variable_reference_not_definition() {
        let mut index = DefinitionIndex::new();
        let temp_dir = env::temp_dir();
        let file_path = temp_dir.join("test.forth").to_string_lossy().to_string();

        // Define a variable and use it
        index.update_file(&file_path, &Rope::from_str("VARIABLE x\n: test x @ ;"));

        // Should find 1 definition
        let defs = index.find_definitions("x");
        assert_eq!(defs.len(), 1);

        // Should find 1 reference (in the test word)
        let refs = index.find_references("x");
        assert_eq!(refs.len(), 1);
    }

    #[test]
    fn test_find_stack_effect() {
        let mut index = DefinitionIndex::new();
        let temp_dir = env::temp_dir();
        let file_path = temp_dir.join("test.forth").to_string_lossy().to_string();

        index.update_file(&file_path, &Rope::from_str(": foo ( n -- n ) 1 + ;"));

        let effect = index.find_stack_effect("foo");
        assert_eq!(effect, Some("( n -- n )".to_string()));
    }

    #[test]
    fn test_find_stack_effect_no_comment() {
        let mut index = DefinitionIndex::new();
        let temp_dir = env::temp_dir();
        let file_path = temp_dir.join("test.forth").to_string_lossy().to_string();

        index.update_file(&file_path, &Rope::from_str(": bar 1 + ;"));

        let effect = index.find_stack_effect("bar");
        assert_eq!(effect, None);
    }

    #[test]
    fn test_find_stack_effect_case_insensitive() {
        let mut index = DefinitionIndex::new();
        let temp_dir = env::temp_dir();
        let file_path = temp_dir.join("test.forth").to_string_lossy().to_string();

        index.update_file(&file_path, &Rope::from_str(": MyWord ( a b -- c ) + ;"));

        assert!(index.find_stack_effect("myword").is_some());
        assert!(index.find_stack_effect("MYWORD").is_some());
        assert!(index.find_stack_effect("MyWord").is_some());
    }

    #[test]
    fn test_code_definition() {
        let mut index = DefinitionIndex::new();
        let temp_dir = env::temp_dir();
        let file_path = temp_dir.join("test.forth").to_string_lossy().to_string();

        index.update_file(
            &file_path,
            &Rope::from_str("CODE syscall0 MOV RAX RBX END-CODE\n: test syscall0 ;"),
        );

        let defs = index.find_definitions("syscall0");
        assert_eq!(defs.len(), 1);

        let refs = index.find_references("syscall0");
        assert_eq!(refs.len(), 1);
    }

    #[test]
    fn test_colon_def_with_doc_comment_following_definition_no_blank_line() {
        let mut index = DefinitionIndex::new();
        let temp_dir = env::temp_dir();
        let file_path = temp_dir.join("test.forth").to_string_lossy().to_string();

        let src =
            "42 constant FOO\n\\ doc comment for my-word\n: my-word ( 0 -- 1 )\n  foo\n  bar ;";
        index.update_file(&file_path, &Rope::from_str(src));

        let details = index.find_definition_details("my-word");
        assert_eq!(details.len(), 1);
        assert_eq!(
            details[0].doc_comment.as_deref(),
            Some("doc comment for my-word")
        );
        assert_eq!(details[0].stack_effect.as_deref(), Some("( 0 -- 1 )"));
    }

    #[test]
    fn test_defining_word_with_doc_comment_following_definition_no_blank_line() {
        let mut index = DefinitionIndex::new();
        let temp_dir = env::temp_dir();
        let file_path = temp_dir.join("test.forth").to_string_lossy().to_string();

        let src = ": prev-word\n  foo\n  bar ;\n\\ doc-comment for qux\nvariable qux";
        index.update_file(&file_path, &Rope::from_str(src));

        let details = index.find_definition_details("qux");
        assert_eq!(details.len(), 1);
        assert_eq!(
            details[0].doc_comment.as_deref(),
            Some("doc-comment for qux")
        );
        assert_eq!(details[0].stack_effect, None);
    }

    #[test]
    fn test_multiline_doc_comment() {
        let mut index = DefinitionIndex::new();
        let temp_dir = env::temp_dir();
        let file_path = temp_dir.join("test.forth").to_string_lossy().to_string();

        let src = "\\ here comes\n\\ some comment block\n\\ documenting word\n: word ... ;";
        index.update_file(&file_path, &Rope::from_str(src));

        let details = index.find_definition_details("word");
        assert_eq!(details.len(), 1);
        assert_eq!(
            details[0].doc_comment.as_deref(),
            Some("here comes some comment block documenting word")
        );
    }

    #[test]
    fn test_doc_comment_detached_by_blank_line() {
        let mut index = DefinitionIndex::new();
        let temp_dir = env::temp_dir();
        let file_path = temp_dir.join("test.forth").to_string_lossy().to_string();

        let src = "\\ detached comment\n\n: word 1 ;";
        index.update_file(&file_path, &Rope::from_str(src));

        let details = index.find_definition_details("word");
        assert_eq!(details.len(), 1);
        assert_eq!(details[0].doc_comment, None);
    }

    #[test]
    fn test_stack_comment_on_next_line() {
        let mut index = DefinitionIndex::new();
        let temp_dir = env::temp_dir();
        let file_path = temp_dir.join("test.forth").to_string_lossy().to_string();

        let src = "\\ my doc\n: word\n  ( a -- b )\n  1 ;";
        index.update_file(&file_path, &Rope::from_str(src));

        let details = index.find_definition_details("word");
        assert_eq!(details.len(), 1);
        assert_eq!(details[0].stack_effect.as_deref(), Some("( a -- b )"));
        assert_eq!(details[0].doc_comment.as_deref(), Some("my doc"));
    }

    #[test]
    fn test_stack_comment_not_immediate() {
        let mut index = DefinitionIndex::new();
        let temp_dir = env::temp_dir();
        let file_path = temp_dir.join("test.forth").to_string_lossy().to_string();

        let src = "\\ my doc\n: word\n  1 2 +\n  ( a -- b ) ;";
        index.update_file(&file_path, &Rope::from_str(src));

        let details = index.find_definition_details("word");
        assert_eq!(details.len(), 1);
        assert_eq!(details[0].stack_effect, None);
        assert_eq!(details[0].doc_comment.as_deref(), Some("my doc"));
    }
}
