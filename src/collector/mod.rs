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
    let root = if src_root.exists() { src_root } else { crate_root.to_path_buf() };

    let mut modules: Vec<PyModule> = Vec::new();

    for entry in WalkDir::new(&root)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map_or(false, |ext| ext == "rs"))
    {
        let path = entry.path();
        let source = std::fs::read_to_string(path)?;
        let file = syn::parse_file(&source).map_err(|e| {
            anyhow::anyhow!("Failed to parse {}: {}", path.display(), e)
        })?;

        let file_modules = parse::extract_modules_from_file(&file, path, config);
        modules.extend(file_modules);
    }

    Ok(modules)
}
