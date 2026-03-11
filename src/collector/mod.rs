mod model;
mod parse;

pub use model::*;

use crate::config::Config;
use anyhow::Result;
use std::path::Path;
use walkdir::WalkDir;

/// Walk the entire crate (all .rs files under `crate_root/src/`) and collect
/// every pyo3-exposed item.
pub fn collect_crate(crate_root: &Path, config: &Config) -> Result<Vec<PyModule>> {
    let src_root = crate_root.join("src");
    let root = if src_root.exists() {
        src_root
    } else {
        crate_root.to_path_buf()
    };

    // Collect all .rs paths and parsed files up front; subsequent passes operate over this slice.
    // All parsed ASTs are held in memory; for very large crates consider streaming or lazy parsing in the future.
    let mut files: Vec<(std::path::PathBuf, syn::File)> = Vec::new();
    for entry in WalkDir::new(&root)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "rs"))
    {
        let path = entry.path().to_path_buf();
        let source = std::fs::read_to_string(&path)?;
        let file = syn::parse_file(&source)
            .map_err(|e| anyhow::anyhow!("Failed to parse {}: {}", path.display(), e))?;
        files.push((path, file));
    }

    // First pass: build Rust type name -> Python name for #[pyclass(name = "...")]
    let pyclass_name_map = parse::build_pyclass_name_map(&files);
    // Second pass: build type alias name -> underlying type for `type Foo = ...`
    let type_alias_map = parse::build_type_alias_map(&files);
    // Third pass: build #[pymethods] impl map across the whole crate so that
    // impl blocks defined in a different file from the #[pymodule] are found.
    let impl_map = parse::build_impl_map(&files);

    let mut modules: Vec<PyModule> = Vec::new();
    for (path, file) in files {
        let file_modules = parse::extract_modules_from_file(
            &file,
            &path,
            config,
            &pyclass_name_map,
            &type_alias_map,
            &impl_map,
        );
        modules.extend(file_modules);
    }

    Ok(modules)
}
