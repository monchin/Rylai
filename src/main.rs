mod add_content;
mod collector;
mod config;
mod generator;
mod output_layout;
mod stub_constants;
mod type_map;

use anyhow::{Context, Result};
use clap::Parser;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
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
    /// Each top-level #[pymodule] produces <name>.pyi under this directory; submodules become
    /// sibling .pyi files (first module segment is implicit — e.g. `pkg.aaa` → `aaa.pyi`, not `pkg/aaa.pyi`).
    /// If this path's last component matches the pymodule name (e.g. `-o python/abcd`),
    /// that directory is the package root — layout paths that start with `abcd/` strip that prefix when joining.
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
    let (items, collector_warnings) = collector::collect_crate(&cli.crate_root, &config)?;
    let (known_classes, mut pre_warnings) = generator::collect_class_names(&items);
    pre_warnings.extend(collector_warnings);

    // Resolve output directory, create it if necessary
    let out_dir = cli.output.unwrap_or_else(|| cli.crate_root.clone());
    std::fs::create_dir_all(&out_dir)?;

    let mut generated_paths: Vec<PathBuf> = Vec::new();

    if items.is_empty() {
        // No pymodules found: write an empty stub using the best-guess name
        let name = infer_module_name_from_pyproject(&cli.crate_root)
            .or_else(|| infer_module_name_from_cargo(&cli.crate_root))
            .unwrap_or_else(|| "stub".to_string());
        let rel = PathBuf::from(format!("{name}.pyi"));
        config::validate_add_content_targets(&config.add_content, std::slice::from_ref(&rel))?;
        let path = out_dir.join(format!("{name}.pyi"));
        let stub = generator::generate_with_known_classes(
            &items,
            &config,
            &known_classes,
            &pre_warnings,
            None,
            Some(&rel),
        )?;
        let stub = add_content::apply_add_content(&stub, &rel, &config.add_content)?;
        std::fs::write(&path, stub)?;
        println!("Generated: {}", path.display());
        generated_paths.push(path);
    } else {
        // Resolve layout: one or more (path, PyModule) per top-level pymodule (package mode when
        // #[pyclass(module = "...")] is used). Functions/constants/unannotated classes use the root stub.
        let output_specs = output_layout::resolve(items.clone());
        let generated_rel_paths: Vec<PathBuf> =
            output_specs.iter().map(|(p, _)| p.clone()).collect();
        config::validate_add_content_targets(&config.add_content, &generated_rel_paths)?;
        let class_defining_modules = output_layout::rust_class_defining_modules(&items);
        let mut seen_output_paths: HashSet<PathBuf> = HashSet::new();
        for (idx, (rel_path, stub_module)) in output_specs.into_iter().enumerate() {
            let path = join_output_path(&out_dir, &rel_path);
            if !seen_output_paths.insert(path.clone()) {
                anyhow::bail!(
                    "duplicate output path: {}\n\
                     Try pointing `-o` at a parent directory so each stub lands on a distinct path \
                     (e.g. `-o stubs` instead of `-o stubs/<pymodule>` when layout paths would collide).",
                    path.display()
                );
            }
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let empty: &[String] = &[];
            let warnings = if idx == 0 {
                pre_warnings.as_slice()
            } else {
                empty
            };
            let stub = generator::generate_with_known_classes(
                std::slice::from_ref(&stub_module),
                &config,
                &known_classes,
                warnings,
                Some((stub_module.name.as_str(), &class_defining_modules)),
                Some(&rel_path),
            )?;
            let stub = add_content::apply_add_content(&stub, &rel_path, &config.add_content)?;
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

/// Combine output directory with a path from [`output_layout::resolve`].
///
/// When `-o` already points at the Python **package** directory, layout
/// paths still begin with the pymodule name. If `out_dir`'s last component equals the
/// first segment of `rel_path`, that segment is skipped.
///
/// If two different layout paths resolve to the same file, [`main`] detects the duplicate and
/// errors — use `-o` on the **parent** directory when needed so paths stay distinct.
fn join_output_path(out_dir: &Path, rel_path: &Path) -> PathBuf {
    use std::path::Component;
    let mut components = rel_path.components();
    match (components.next(), out_dir.file_name()) {
        (Some(Component::Normal(first)), Some(od)) if first == od => {
            let rest: PathBuf = components.collect();
            out_dir.join(rest)
        }
        _ => out_dir.join(rel_path),
    }
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

#[cfg(test)]
mod tests {
    use super::join_output_path;
    use std::path::Path;

    #[test]
    fn join_output_strips_pkg_root_when_out_dir_matches_first_segment() {
        let out = Path::new("python/abcd");
        assert_eq!(
            join_output_path(out, Path::new("abcd.pyi")),
            Path::new("python/abcd/abcd.pyi")
        );
        assert_eq!(
            join_output_path(out, Path::new("efg.pyi")),
            Path::new("python/abcd/efg.pyi")
        );
        assert_eq!(
            join_output_path(out, Path::new("abcd/xyz.pyi")),
            Path::new("python/abcd/xyz.pyi")
        );
    }

    #[test]
    fn join_output_keeps_parent_when_out_dir_is_parent_of_package() {
        let out = Path::new("stubs");
        assert_eq!(
            join_output_path(out, Path::new("abcd.pyi")),
            Path::new("stubs/abcd.pyi")
        );
        assert_eq!(
            join_output_path(out, Path::new("efg.pyi")),
            Path::new("stubs/efg.pyi")
        );
        assert_eq!(
            join_output_path(out, Path::new("pkg/aaa.pyi")),
            Path::new("stubs/pkg/aaa.pyi")
        );
    }
}
