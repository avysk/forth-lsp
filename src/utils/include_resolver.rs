use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use forth_lexer::{parser::Lexer, token::Token};
use ropey::Rope;

use crate::config::WorkspaceConfig;
use crate::utils::definition_index::DefinitionIndex;
use crate::utils::uri_helpers::path_to_uri;

/// Words that introduce a file dependency in Forth
const INCLUDE_WORDS: &[&str] = &["require", "include", "needs", "fload"];

/// Scan tokens for filenames referenced by `require`, `include`, `needs`, or `fload`.
pub fn scan_required_files(tokens: &[Token]) -> Vec<String> {
    let mut targets = Vec::new();
    let mut iter = tokens.iter();

    while let Some(token) = iter.next() {
        if let Token::Word(data) = token
            && INCLUDE_WORDS
                .iter()
                .any(|&w| w.eq_ignore_ascii_case(data.value))
        {
            // Find the next non-comment word token
            for next_tok in iter.by_ref() {
                match next_tok {
                    Token::Comment(_) | Token::StackComment(_) => continue,
                    Token::Word(arg_data) => {
                        let target = arg_data
                            .value
                            .trim_matches(|c| c == '"' || c == '\'');
                        if !target.is_empty() {
                            targets.push(target.to_string());
                        }
                        break;
                    }
                    _ => break,
                }
            }
        }
    }

    targets
}

/// Helper to check if a candidate path exists as a file or with standard Forth extensions.
fn check_candidate(path: &Path, workspace: &WorkspaceConfig) -> Option<PathBuf> {
    if path.is_file() {
        return Some(path.to_path_buf());
    }

    if path.extension().is_none() {
        for ext in &workspace.extensions {
            let candidate = path.with_extension(ext);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    None
}

/// Search for a required file target:
/// 1. If `target` is absolute, check it.
/// 2. If `current_file_dir` is provided, check relative to it.
/// 3. Check relative to each directory in `include_dirs` in order.
pub fn find_required_file(
    target: &str,
    current_file_dir: Option<&Path>,
    include_dirs: &[PathBuf],
    workspace: &WorkspaceConfig,
) -> Option<PathBuf> {
    let target_path = Path::new(target);

    // 1. Absolute path
    if target_path.is_absolute()
        && let Some(found) = check_candidate(target_path, workspace)
    {
        return Some(found);
    }

    // 2. Relative to current file's directory
    if let Some(dir) = current_file_dir
        && let Some(found) = check_candidate(&dir.join(target_path), workspace)
    {
        return Some(found);
    }

    // 3. Relative to each include directory in order
    for inc_dir in include_dirs {
        if let Some(found) = check_candidate(&inc_dir.join(target_path), workspace) {
            return Some(found);
        }
    }

    None
}

/// Recursively resolve and load required files from the given tokens into `files` and `def_index`.
pub fn resolve_and_load_dependencies(
    tokens: &[Token],
    current_file_path: Option<&Path>,
    include_dirs: &[PathBuf],
    files: &mut HashMap<String, Rope>,
    def_index: &mut DefinitionIndex,
    workspace: &WorkspaceConfig,
) {
    let mut visited = HashSet::new();
    if let Some(path) = current_file_path {
        if let Ok(canon) = path.canonicalize() {
            visited.insert(canon);
        } else {
            visited.insert(path.to_path_buf());
        }
    }

    resolve_dependencies_recursive(
        tokens,
        current_file_path,
        include_dirs,
        files,
        def_index,
        workspace,
        &mut visited,
    );
}

fn resolve_dependencies_recursive(
    tokens: &[Token],
    current_file_path: Option<&Path>,
    include_dirs: &[PathBuf],
    files: &mut HashMap<String, Rope>,
    def_index: &mut DefinitionIndex,
    workspace: &WorkspaceConfig,
    visited: &mut HashSet<PathBuf>,
) {
    let current_dir = current_file_path.and_then(|p| p.parent());
    let targets = scan_required_files(tokens);

    for target in targets {
        if let Some(found_path) =
            find_required_file(&target, current_dir, include_dirs, workspace)
        {
            let canonical = found_path
                .canonicalize()
                .unwrap_or_else(|_| found_path.clone());
            if !visited.insert(canonical) {
                continue;
            }

            let Some(file_uri) = path_to_uri(&found_path) else {
                continue;
            };
            let uri_str = file_uri.to_string();

            if files.contains_key(&uri_str) {
                continue;
            }

            if let Ok(raw_content) = fs::read(&found_path) {
                let content = String::from_utf8_lossy(&raw_content);
                let rope = Rope::from_str(&content);
                let mut lexer = Lexer::new(&content);
                let dep_tokens = lexer.parse();

                def_index.update_file_from_tokens(&uri_str, &dep_tokens, &rope);
                files.insert(uri_str, rope);

                resolve_dependencies_recursive(
                    &dep_tokens,
                    Some(&found_path),
                    include_dirs,
                    files,
                    def_index,
                    workspace,
                    visited,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_scan_required_files() {
        let source = "require foo.4th \n include \"bar.fs\" \n needs 'baz.fth' \n fload qux";
        let tokens = Lexer::new(source).parse();
        let files = scan_required_files(&tokens);
        assert_eq!(files, vec!["foo.4th", "bar.fs", "baz.fth", "qux"]);
    }

    #[test]
    fn test_scan_required_files_ignores_comments() {
        let source = "require \\ comment\n foo.4th";
        let tokens = Lexer::new(source).parse();
        let files = scan_required_files(&tokens);
        assert_eq!(files, vec!["foo.4th"]);
    }

    #[test]
    fn test_find_required_file_precedence() {
        let temp = tempdir().unwrap();
        let inc1 = temp.path().join("inc1");
        let inc2 = temp.path().join("inc2");
        fs::create_dir(&inc1).unwrap();
        fs::create_dir(&inc2).unwrap();

        // Create same file name in both include dirs
        fs::write(inc1.join("common.fs"), ": FROM_INC1 ;").unwrap();
        fs::write(inc2.join("common.fs"), ": FROM_INC2 ;").unwrap();

        let ws = WorkspaceConfig::default();
        let include_dirs = vec![inc1.clone(), inc2.clone()];

        // inc1 should be found first
        let found = find_required_file("common.fs", None, &include_dirs, &ws).unwrap();
        assert_eq!(found, inc1.join("common.fs"));
    }

    #[test]
    fn test_find_required_file_current_dir_over_include_dir() {
        let temp = tempdir().unwrap();
        let current_dir = temp.path().join("current");
        let inc = temp.path().join("inc");
        fs::create_dir(&current_dir).unwrap();
        fs::create_dir(&inc).unwrap();

        fs::write(current_dir.join("local.fs"), ": LOCAL ;").unwrap();
        fs::write(inc.join("local.fs"), ": INC ;").unwrap();

        let ws = WorkspaceConfig::default();
        let include_dirs = vec![inc];

        let found = find_required_file("local.fs", Some(&current_dir), &include_dirs, &ws).unwrap();
        assert_eq!(found, current_dir.join("local.fs"));
    }

    #[test]
    fn test_find_required_file_with_extension_fallback() {
        let temp = tempdir().unwrap();
        let inc = temp.path().join("inc");
        fs::create_dir(&inc).unwrap();
        fs::write(inc.join("helper.4th"), ": HELPER ;").unwrap();

        let ws = WorkspaceConfig::default();
        let include_dirs = vec![inc.clone()];

        // Target specifies no extension; should find helper.4th
        let found = find_required_file("helper", None, &include_dirs, &ws).unwrap();
        assert_eq!(found, inc.join("helper.4th"));
    }

    #[test]
    fn test_resolve_and_load_dependencies_recursive_and_circular() {
        let temp = tempdir().unwrap();
        let inc = temp.path().join("inc");
        fs::create_dir(&inc).unwrap();

        // Circular dependency: A requires B, B requires A
        fs::write(inc.join("a.fs"), "require b.fs\n: WORD_A ;").unwrap();
        fs::write(inc.join("b.fs"), "require a.fs\n: WORD_B ;").unwrap();

        let root_file = temp.path().join("main.fs");
        let root_source = "require a.fs\n: MAIN WORD_A WORD_B ;";
        fs::write(&root_file, root_source).unwrap();

        let mut files = HashMap::new();
        let mut def_index = DefinitionIndex::new();
        let ws = WorkspaceConfig::default();
        let include_dirs = vec![inc];

        let tokens = Lexer::new(root_source).parse();
        resolve_and_load_dependencies(
            &tokens,
            Some(&root_file),
            &include_dirs,
            &mut files,
            &mut def_index,
            &ws,
        );

        // Both A and B should have been loaded into files and indexed in def_index
        assert!(!def_index.find_definitions("WORD_A").is_empty());
        assert!(!def_index.find_definitions("WORD_B").is_empty());
    }
}
