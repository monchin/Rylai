mod config;
mod collector;
mod type_map;
mod generator;

use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "rylai",
    about = "Generate Python .pyi stub files from pyo3-annotated Rust source code"
)]
struct Cli {
    /// Path to the crate root (default: current directory)
    #[arg(default_value = ".")]
    crate_root: PathBuf,

    /// Output .pyi file path (default: <crate_root>/<crate_name>.pyi)
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// Path to rylai.toml config (default: <crate_root>/rylai.toml)
    #[arg(short, long)]
    config: Option<PathBuf>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    // Load config (optional, zero-config works)
    let config_path = cli
        .config
        .unwrap_or_else(|| cli.crate_root.join("rylai.toml"));
    let config = config::Config::load_or_default(&config_path)?;

    // Collect all pyo3 items from the crate
    let items = collector::collect_crate(&cli.crate_root, &config)?;

    // Determine output path
    let output_path = cli.output.unwrap_or_else(|| {
        // Try to infer crate name from Cargo.toml, fallback to directory name
        let name = infer_crate_name(&cli.crate_root).unwrap_or_else(|| "stub".to_string());
        cli.crate_root.join(format!("{name}.pyi"))
    });

    // Generate .pyi content
    let stub = generator::generate(&items, &config)?;

    std::fs::write(&output_path, stub)?;
    println!("Generated: {}", output_path.display());

    Ok(())
}

fn infer_crate_name(crate_root: &std::path::Path) -> Option<String> {
    let cargo_toml = std::fs::read_to_string(crate_root.join("Cargo.toml")).ok()?;
    let doc: toml::Value = cargo_toml.parse().ok()?;
    doc.get("package")?.get("name")?.as_str().map(|s| s.replace('-', "_"))
}
