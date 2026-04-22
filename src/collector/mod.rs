mod model;
mod parse;
mod rename_all;

pub use model::*;

use crate::config::Config;
use crate::macro_expand;
use anyhow::Result;
use std::cell::RefCell;
use std::path::Path;
use walkdir::WalkDir;

/// Walk the entire crate (all .rs files under `crate_root/src/`) and collect
/// every pyo3-exposed item.
///
/// Builds [`parse::build_pyclass_enum_rust_names`] and passes it into [`parse::ParseContext`] so
/// Style B `m.add_class::<T>()` can classify enums; do not omit that pass when reusing the parser
/// for real crates.
pub fn collect_crate(crate_root: &Path, config: &Config) -> Result<(Vec<PyModule>, Vec<String>)> {
    let src_root = crate_root.join("src");
    let root = if src_root.exists() {
        src_root
    } else {
        crate_root.to_path_buf()
    };

    // Step 1: read all .rs files as raw text so we can optionally expand macros before parsing.
    let mut sources: Vec<(std::path::PathBuf, String)> = Vec::new();
    for entry in WalkDir::new(&root)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "rs"))
    {
        let path = entry.path().to_path_buf();
        let source = std::fs::read_to_string(&path)?;
        sources.push((path, source));
    }

    // Step 2: if [[macro_expand]] is configured, build expansion rules and pre-process every file.
    let mut macro_expand_warnings: Vec<String> = Vec::new();
    if !config.macro_expand.is_empty() {
        let macro_w = RefCell::new(Vec::new());
        let rules =
            macro_expand::build_macro_rules(&config.macro_expand, &sources, Some(&macro_w))?;
        macro_expand_warnings = macro_w.into_inner();
        if !rules.is_empty() {
            for (_path, source) in &mut sources {
                *source = macro_expand::expand_source(source, &rules)?;
            }
        }
    }

    // Step 3: parse all (possibly expanded) sources with syn.
    // All parsed ASTs are held in memory; for very large crates consider streaming or lazy parsing in the future.
    let mut files: Vec<(std::path::PathBuf, syn::File)> = Vec::new();
    for (path, source) in &sources {
        let file = syn::parse_file(source)
            .map_err(|e| anyhow::anyhow!("Failed to parse {}: {}", path.display(), e))?;
        files.push((path.clone(), file));
    }

    let enabled_features = &config.features.enabled;
    // First pass: build Rust type name -> Python name for #[pyclass(name = "...")]
    let pyclass_name_map = parse::build_pyclass_name_map(&files, enabled_features);
    // Second pass: build type alias name -> underlying type for `type Foo = ...`
    let type_alias_map = parse::build_type_alias_map(&files, enabled_features);
    // Third pass: build #[pymethods] impl map across the whole crate so that
    // impl blocks defined in a different file from the #[pymodule] are found.
    let impl_map = parse::build_impl_map(&files, enabled_features);
    // Fourth pass: build #[pyclass] struct fields map so that #[pyo3(get)] / #[pyo3(set)]
    // fields on structs defined in any file generate @property stubs.
    let struct_fields_map = parse::build_struct_fields_map(&files, enabled_features);
    // Fifth pass: build #[pyclass] type name -> attributes (for docstrings in Style B).
    let pyclass_attrs_map = parse::build_pyclass_attrs_map(&files, enabled_features);
    // Sixth pass: enum Rust names so Style B `add_class::<T>()` can tell structs from enums.
    let pyclass_enum_rust_names = parse::build_pyclass_enum_rust_names(&files, enabled_features);
    // Seventh pass: build pyfunction map for cross-module lookup.
    let pyfunction_map = parse::build_pyfunction_map(&files, enabled_features);

    let parse_warnings = RefCell::new(Vec::new());
    let type_map_preserve_idents = crate::config::type_map_preserve_alias_idents(&config.type_map);
    let cx = parse::ParseContext {
        config,
        impl_map: &impl_map,
        struct_fields_map: &struct_fields_map,
        type_alias_map: &type_alias_map,
        pyclass_attrs_map: &pyclass_attrs_map,
        pyclass_enum_rust_names: Some(&pyclass_enum_rust_names),
        parse_warnings: Some(&parse_warnings),
        type_map_preserve_idents: &type_map_preserve_idents,
        pyfunction_map: Some(&pyfunction_map),
    };

    let mut modules: Vec<PyModule> = Vec::new();
    for (path, file) in files {
        let file_modules = parse::extract_modules_from_file(&file, &path, &pyclass_name_map, cx);
        modules.extend(file_modules);
    }

    let mut all_warnings = macro_expand_warnings;
    all_warnings.extend(parse_warnings.into_inner());
    Ok((modules, all_warnings))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn write_file(base: &Path, rel: &str, content: &str) {
        let p = base.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&p, content).unwrap();
    }

    #[test]
    fn collect_crate_basic_style_a_module() {
        let dir = tempfile::tempdir().unwrap();
        write_file(
            dir.path(),
            "src/lib.rs",
            r#"
use pyo3::prelude::*;

#[pymodule]
mod my_module {
    use pyo3::prelude::*;

    #[pyfunction]
    fn greet(name: &str) -> String {
        format!("Hello, {}!", name)
    }

    #[pyfunction]
    fn add(a: i64, b: i64) -> i64 {
        a + b
    }
}
"#,
        );
        let config = Config::default();
        let (modules, warnings) = collect_crate(dir.path(), &config).unwrap();
        assert_eq!(modules.len(), 1);
        assert_eq!(modules[0].name, "my_module");
        assert!(warnings.is_empty());
        let fns: Vec<_> = modules[0]
            .items
            .iter()
            .filter_map(|i| match i {
                PyItem::Function(f) => Some(f.name.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(fns.len(), 2);
        assert!(fns.contains(&"greet".to_string()));
        assert!(fns.contains(&"add".to_string()));
    }

    #[test]
    fn collect_crate_no_src_dir_falls_back_to_crate_root() {
        let dir = tempfile::tempdir().unwrap();
        write_file(
            dir.path(),
            "lib.rs",
            r#"
use pyo3::prelude::*;

#[pymodule]
mod root_mod {
    use pyo3::prelude::*;

    #[pyfunction]
    fn foo() -> i32 { 42 }
}
"#,
        );
        let config = Config::default();
        let (modules, _) = collect_crate(dir.path(), &config).unwrap();
        assert_eq!(modules.len(), 1);
        assert_eq!(modules[0].name, "root_mod");
    }

    #[test]
    fn collect_crate_empty_crate_returns_no_modules() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/README.md"), "not rust").unwrap();
        let config = Config::default();
        let (modules, warnings) = collect_crate(dir.path(), &config).unwrap();
        assert!(modules.is_empty());
        assert!(warnings.is_empty());
    }

    #[test]
    fn collect_crate_invalid_rust_syntax_errors() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "src/lib.rs", "this is not valid rust <<<");
        let config = Config::default();
        assert!(collect_crate(dir.path(), &config).is_err());
    }

    #[test]
    fn collect_crate_pyclass_with_pymethods() {
        let dir = tempfile::tempdir().unwrap();
        write_file(
            dir.path(),
            "src/lib.rs",
            r#"
use pyo3::prelude::*;

#[pymodule]
mod animals {
    use pyo3::prelude::*;

    #[pyclass]
    pub struct Dog {
        name: String,
    }

    #[pymethods]
    impl Dog {
        #[new]
        fn new(name: String) -> Self {
            Self { name }
        }

        fn bark(&self) -> String {
            format!("{} says woof!", self.name)
        }
    }
}
"#,
        );
        let config = Config::default();
        let (modules, warnings) = collect_crate(dir.path(), &config).unwrap();
        assert_eq!(modules.len(), 1);
        assert!(warnings.is_empty());
        let classes: Vec<_> = modules[0]
            .items
            .iter()
            .filter_map(|i| match i {
                PyItem::Class(c) => Some(c),
                _ => None,
            })
            .collect();
        assert_eq!(classes.len(), 1);
        assert_eq!(classes[0].name, "Dog");
        assert!(
            classes[0].methods.len() >= 2,
            "expected new + bark, got {:?}",
            classes[0].methods
        );
    }

    #[test]
    fn collect_crate_style_b_module() {
        let dir = tempfile::tempdir().unwrap();
        write_file(
            dir.path(),
            "src/lib.rs",
            r#"
use pyo3::prelude::*;

#[pyclass]
pub struct Counter;

#[pymethods]
impl Counter {
    #[new]
    fn new() -> Self { Self }

    fn value(&self) -> i64 { 0 }
}

#[pyfunction]
fn double(x: i64) -> i64 {
    x * 2
}

#[pymodule]
fn my_counter(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<Counter>()?;
    m.add_function(wrap_pyfunction!(double, m)?)?;
    Ok(())
}
"#,
        );
        let config = Config::default();
        let (modules, _) = collect_crate(dir.path(), &config).unwrap();
        assert_eq!(modules.len(), 1);
        assert_eq!(modules[0].name, "my_counter");
        let classes: Vec<_> = modules[0]
            .items
            .iter()
            .filter_map(|i| match i {
                PyItem::Class(c) => Some(c.name.clone()),
                _ => None,
            })
            .collect();
        assert!(
            classes.contains(&"Counter".to_string()),
            "expected Counter class, got: {classes:?}"
        );
        let fns: Vec<_> = modules[0]
            .items
            .iter()
            .filter_map(|i| match i {
                PyItem::Function(f) => Some(f.name.clone()),
                _ => None,
            })
            .collect();
        assert!(
            fns.contains(&"double".to_string()),
            "expected double function, got: {fns:?}"
        );
    }

    #[test]
    fn collect_crate_with_macro_expand() {
        let dir = tempfile::tempdir().unwrap();
        write_file(
            dir.path(),
            "src/lib.rs",
            r#"
use pyo3::prelude::*;

macro_rules! register {
    ($($cls:ty),* $(,)?) => {
        $(add_class::<$cls>()?;)*
    };
}

#[pyclass]
pub struct Foo;

#[pymethods]
impl Foo {
    #[new]
    fn new() -> Self { Self }
}

#[pymodule]
fn expanded_mod(m: &Bound<'_, PyModule>) -> PyResult<()> {
    register!(Foo);
    Ok(())
}
"#,
        );
        let mut config = Config::default();
        config.macro_expand.push(crate::config::MacroExpandEntry {
            name: "register".to_string(),
            from: None,
            to: None,
        });
        let (modules, warnings) = collect_crate(dir.path(), &config).unwrap();
        assert_eq!(modules.len(), 1);
        assert_eq!(modules[0].name, "expanded_mod");
        assert!(
            warnings.is_empty(),
            "macro expansion should not produce warnings: {warnings:?}"
        );
    }

    #[test]
    fn collect_crate_multiple_rs_files() {
        let dir = tempfile::tempdir().unwrap();
        write_file(
            dir.path(),
            "src/lib.rs",
            r#"
use pyo3::prelude::*;

#[pymodule]
mod multi_file {
    use pyo3::prelude::*;

    #[pyfunction]
    fn top_fn() -> i32 { 1 }
}
"#,
        );
        write_file(
            dir.path(),
            "src/extra.rs",
            r#"
use pyo3::prelude::*;

#[pyclass]
pub struct Extra;

#[pymethods]
impl Extra {
    #[new]
    fn new() -> Self { Self }
}
"#,
        );
        let config = Config::default();
        let (modules, _) = collect_crate(dir.path(), &config).unwrap();
        assert_eq!(modules.len(), 1);
        assert_eq!(modules[0].name, "multi_file");
        let fns: Vec<_> = modules[0]
            .items
            .iter()
            .filter_map(|i| match i {
                PyItem::Function(f) => Some(f.name.clone()),
                _ => None,
            })
            .collect();
        assert!(
            fns.contains(&"top_fn".to_string()),
            "expected top_fn in multi_file module, got: {fns:?}"
        );
    }
}
