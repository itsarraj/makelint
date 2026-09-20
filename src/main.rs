use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use makelint::parser::parse;
use makelint::rules::lint;

#[derive(Parser)]
#[command(name = "makelint", about = "Lints a Makefile for common footguns")]
struct Cli {
    /// Makefile to lint.
    #[arg(default_value = "Makefile")]
    file: PathBuf,
}

fn main() -> anyhow::Result<ExitCode> {
    let cli = Cli::parse();
    let content = fs::read_to_string(&cli.file)
        .map_err(|e| anyhow::anyhow!("reading {}: {e}", cli.file.display()))?;

    let parsed = parse(&content);
    let findings = lint(&parsed);

    if findings.is_empty() {
        println!("{}: no issues found", cli.file.display());
        return Ok(ExitCode::SUCCESS);
    }

    for f in &findings {
        println!("{}:{}: {}", cli.file.display(), f.line, f.message);
    }
    println!("\n{} issue(s) found", findings.len());
    Ok(ExitCode::from(1))
}
