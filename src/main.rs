mod collector;
mod config;
mod generator;
mod type_map;

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

    // Load config (optional, zero-config works)
    let config_path = cli
        .config
        .unwrap_or_else(|| cli.crate_root.join("rylai.toml"));
    let config = config::Config::load_or_default(&config_path)?;

    // Collect all pyo3 items from the crate
    let items = collector::collect_crate(&cli.crate_root, &config)?;

    // Resolve output directory, create it if necessary
    let out_dir = cli.output.unwrap_or_else(|| cli.crate_root.clone());
    std::fs::create_dir_all(&out_dir)?;

    if items.is_empty() {
        // No pymodules found: write an empty stub using the best-guess name
        let name = infer_module_name_from_pyproject(&cli.crate_root)
            .or_else(|| infer_module_name_from_cargo(&cli.crate_root))
            .unwrap_or_else(|| "stub".to_string());
        let path = out_dir.join(format!("{name}.pyi"));
        let stub = generator::generate(&items, &config)?;
        std::fs::write(&path, stub)?;
        println!("Generated: {}", path.display());
    } else {
        // One .pyi file per top-level #[pymodule]; name comes from the AST
        for module in &items {
            let path = out_dir.join(format!("{}.pyi", module.name));
            let stub = generator::generate(std::slice::from_ref(module), &config)?;
            std::fs::write(&path, stub)?;
            println!("Generated: {}", path.display());
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
