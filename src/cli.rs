use clap::Parser;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "forth-lsp",
    about = "LSP for the Forth programming language",
    disable_version_flag = true
)]
pub struct Cli {
    /// Print version information
    #[arg(short = 'V', long = "version")]
    pub version: bool,

    /// Additional directory to search for required Forth files and word definitions
    #[arg(short = 'I', long = "include", value_name = "DIR")]
    pub include: Vec<PathBuf>,
}
