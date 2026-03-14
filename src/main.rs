mod collector;
mod config;
mod generator;
mod type_map;

use anyhow::{Context, Result};
use clap::Parser;
use std::path::PathBuf;
use std::process::Command;

#[derive(Parser, Debug)]
#[command(
    name = "rylai",
    about = "Generate Python .pyi stub files from pyo3-annotated Rust source code"
)]
struct Cli {
    /// Path to the crate root (default: current directory)
    #[arg(default_value = ".")]
    crate_root: PathBuf,

    /// Output directory for generated .pyi files (default: crate root).
    /// Created automatically if it does not exist.
    /// Each top-level #[pymodule] produces one <module_name>.pyi inside this directory.
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// Path to rylai.toml config (default: <crate_root>/rylai.toml)
    #[arg(short, long)]
    config: Option<PathBuf>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    // Load config: merge [tool.rylai] from pyproject.toml (base) and rylai.toml (override).
    // Duplicate keys are resolved in favor of rylai.toml.
    let rylai_toml_path = cli
        .config
        .unwrap_or_else(|| cli.crate_root.join("rylai.toml"));
    let pyproject_path = cli.crate_root.join("pyproject.toml");
    let config = config::Config::load_merged(&rylai_toml_path, &pyproject_path)?;

    // Collect all pyo3 items from the crate
    let items = collector::collect_crate(&cli.crate_root, &config)?;

    // Resolve output directory, create it if necessary
    let out_dir = cli.output.unwrap_or_else(|| cli.crate_root.clone());
    std::fs::create_dir_all(&out_dir)?;

    let mut generated_paths: Vec<PathBuf> = Vec::new();

    if items.is_empty() {
        // No pymodules found: write an empty stub using the best-guess name
        let name = infer_module_name_from_pyproject(&cli.crate_root)
            .or_else(|| infer_module_name_from_cargo(&cli.crate_root))
            .unwrap_or_else(|| "stub".to_string());
        let path = out_dir.join(format!("{name}.pyi"));
        let stub = generator::generate(&items, &config)?;
        std::fs::write(&path, stub)?;
        println!("Generated: {}", path.display());
        generated_paths.push(path);
    } else {
        // One .pyi file per top-level #[pymodule]; name comes from the AST
        for module in &items {
            let path = out_dir.join(format!("{}.pyi", module.name));
            let stub = generator::generate(std::slice::from_ref(module), &config)?;
            std::fs::write(&path, stub)?;
            println!("Generated: {}", path.display());
            generated_paths.push(path);
        }
    }

    if !config.format.is_empty() && !generated_paths.is_empty() {
        run_format_commands(&config.format, &generated_paths)?;
    }

    Ok(())
}

/// Run each entry in `format` as a command with all generated .pyi paths appended.
/// E.g. `ruff format` with paths [a.pyi, b.pyi] runs `ruff format a.pyi b.pyi`.
/// Empty or whitespace-only entries are skipped.
fn run_format_commands(format_commands: &[String], pyi_paths: &[PathBuf]) -> Result<()> {
    for cmd_str in format_commands {
        let cmd_str = cmd_str.trim();
        if cmd_str.is_empty() {
            continue;
        }
        let parts: Vec<&str> = cmd_str.split_whitespace().collect();
        let (program, args) = match parts.split_first() {
            Some((p, rest)) => (*p, rest.to_vec()),
            None => continue,
        };
        let mut cmd = Command::new(program);
        cmd.args(&args);
        cmd.args(pyi_paths);
        let status = cmd
            .status()
            .with_context(|| format!("failed to run format command: {}", cmd_str))?;
        if !status.success() {
            anyhow::bail!("format command exited with {}: {}", status, cmd_str);
        }
    }
    Ok(())
}

/// Read `pyproject.toml` and return `tool.maturin.module-name` if present.
/// This is the canonical Python module name set by maturin builds.
fn infer_module_name_from_pyproject(crate_root: &std::path::Path) -> Option<String> {
    let text = std::fs::read_to_string(crate_root.join("pyproject.toml")).ok()?;
    let doc: toml::Value = text.parse().ok()?;
    doc.get("tool")?
        .get("maturin")?
        .get("module-name")?
        .as_str()
        .map(|s| s.to_string())
}

/// Read `Cargo.toml` and return `package.name` with `-` replaced by `_`.
/// Used only as a last resort; Cargo names are Rust identifiers, not Python ones.
fn infer_module_name_from_cargo(crate_root: &std::path::Path) -> Option<String> {
    let text = std::fs::read_to_string(crate_root.join("Cargo.toml")).ok()?;
    let doc: toml::Value = text.parse().ok()?;
    doc.get("package")?
        .get("name")?
        .as_str()
        .map(|s| s.replace('-', "_"))
}
