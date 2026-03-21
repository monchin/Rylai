//! Resolves output .pyi layout: one file per top-level pymodule, or package layout
//! when `#[pyclass(module = "...")]` is used.

use crate::collector::{PyItem, PyModule};
use std::collections::HashMap;
use std::path::PathBuf;

/// One output stub: path relative to output dir (e.g. `abcd.pyi` or `abcd/__init__.pyi`), and the module to generate.
pub type OutputSpec = (PathBuf, PyModule);

/// Resolve each top-level `#[pymodule]` into one or more (path, PyModule) pairs.
/// Single-file mode: one `{name}.pyi` per pymodule when no class has `#[pyclass(module = "...")]`.
/// Package mode: `{root}/__init__.pyi` and `{root}/{sub}.pyi` for each submodule from pyclass(module=...).
///
/// `maturin_module_name`: optional `[tool.maturin] module-name` from `pyproject.toml`. When it is a
/// **submodule** of this pymodule (e.g. `abcd.abcd` for root `abcd`), items collected
/// from `m.add(...)` / `m.add_function(...)` (constants and functions) are emitted into that module's
/// stub — matching where the compiled extension exposes them — instead of the package root.
pub fn resolve(modules: Vec<PyModule>, maturin_module_name: Option<&str>) -> Vec<OutputSpec> {
    let mut out = Vec::new();
    for top in modules {
        let maturin_for_this = maturin_module_name.and_then(|m| {
            let first = m.split('.').next()?;
            (first == top.name.as_str()).then_some(m)
        });
        let specs = resolve_one_top_level(top, maturin_for_this);
        out.extend(specs);
    }
    out
}

fn resolve_one_top_level(module: PyModule, maturin_module_name: Option<&str>) -> Vec<OutputSpec> {
    let root = module.name.clone();
    let source_file = module.source_file.clone();
    let doc = module.doc.clone();

    // Flatten all items (including from nested PyItem::Module) and assign each to an output module name.
    let mut buckets: HashMap<String, Vec<PyItem>> = HashMap::new();
    flatten_into_buckets(&module, &root, maturin_module_name, &mut buckets);

    // No #[pyclass(module = "...")] → single file
    let submodule_names: Vec<String> = buckets
        .keys()
        .filter(|k| k.as_str() != root)
        .cloned()
        .collect();
    if submodule_names.is_empty() {
        let items = buckets.remove(&root).unwrap_or_default();
        let path = PathBuf::from(format!("{}.pyi", root));
        let stub_module = PyModule {
            name: root,
            doc,
            items,
            source_file,
        };
        return vec![(path, stub_module)];
    }

    // Package layout: root → __init__.pyi (even when there are no root-level items), each sub → path from dotted name.
    let mut specs = Vec::new();

    let root_items = buckets.remove(&root).unwrap_or_default();
    let init_path = PathBuf::from(&root).join("__init__.pyi");
    specs.push((
        init_path,
        PyModule {
            name: root.clone(),
            doc,
            items: root_items,
            source_file: source_file.clone(),
        },
    ));

    let mut submodule_names: Vec<String> = buckets.keys().cloned().collect();
    submodule_names.sort();
    for sub_name in submodule_names {
        let items = buckets.remove(&sub_name).unwrap_or_default();
        let path = module_name_to_path(&sub_name);
        specs.push((
            path,
            PyModule {
                name: sub_name,
                doc: vec![],
                items,
                source_file: source_file.clone(),
            },
        ));
    }

    specs
}

/// Recursively walk module and nested modules; assign each item to a bucket by target module name.
/// Nested `PyItem::Module` is not pushed to any bucket; only its items are flattened.
fn flatten_into_buckets(
    module: &PyModule,
    root: &str,
    maturin_module_name: Option<&str>,
    buckets: &mut HashMap<String, Vec<PyItem>>,
) {
    for item in &module.items {
        if let PyItem::Module(sub) = item {
            flatten_into_buckets(sub, root, maturin_module_name, buckets);
            continue;
        }
        let target = target_output_module(item, root, maturin_module_name);
        buckets.entry(target).or_default().push(item.clone());
    }
}

/// Target output module for this item: root, the class's `#[pyclass(module = ...)]`, or the Maturin
/// extension module for `m.add` / `m.add_function` when `module-name` is a submodule of `root`.
fn target_output_module(item: &PyItem, root: &str, maturin_module_name: Option<&str>) -> String {
    match item {
        PyItem::Class(c) => {
            if let Some(ref m) = c.module {
                if m == root || m.starts_with(&format!("{}.", root)) {
                    return m.clone();
                }
                eprintln!(
                    "warning: #[pyclass(module = \"{}\")] is not under pymodule \"{}\"; emitting class in root",
                    m, root
                );
            }
            root.to_string()
        }
        PyItem::Constant(_) | PyItem::Function(_) => {
            extension_module_for_add_items(root, maturin_module_name)
        }
        PyItem::Module(_) => unreachable!(
            "flatten_into_buckets inlines nested PyItem::Module before target_output_module"
        ),
    }
}

/// `m.add` / `add_function` run in the pymodule init for the compiled extension; when Maturin
/// `module-name` is `root.child...`, those symbols live on that submodule, not the package root.
fn extension_module_for_add_items(root: &str, maturin_module_name: Option<&str>) -> String {
    let Some(m) = maturin_module_name else {
        return root.to_string();
    };
    if m == root {
        return root.to_string();
    }
    let prefix = format!("{}.", root);
    if m.starts_with(&prefix) {
        return m.to_string();
    }
    root.to_string()
}

/// Convert dotted module name to path: "abcd.efg" → "abcd/efg.pyi"
fn module_name_to_path(name: &str) -> PathBuf {
    let parts: Vec<&str> = name.split('.').collect();
    let mut p = PathBuf::new();
    for (i, part) in parts.iter().enumerate() {
        if i + 1 == parts.len() {
            p.push(format!("{}.pyi", part));
        } else {
            p.push(*part);
        }
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collector::{PyClass, PyConstant, PyFunction, PyItem, PyModule, PyType};
    use std::path::PathBuf;

    fn dummy_path() -> PathBuf {
        PathBuf::from("lib.rs")
    }

    fn make_class(name: &str, module: Option<&str>) -> PyClass {
        PyClass {
            name: name.to_string(),
            rust_name: name.to_string(),
            module: module.map(str::to_string),
            doc: vec![],
            methods: vec![],
            source_file: dummy_path(),
        }
    }

    #[test]
    fn single_module_no_pyclass_module() {
        let m = PyModule {
            name: "abcd".to_string(),
            doc: vec![],
            items: vec![
                PyItem::Function(PyFunction {
                    name: "foo".to_string(),
                    doc: vec![],
                    signature_override: None,
                    params: vec![],
                    return_type: PyType {
                        rust_type: syn::parse_quote! { () },
                        override_str: None,
                    },
                    source_file: dummy_path(),
                }),
                PyItem::Class(make_class("Layer", None)),
            ],
            source_file: dummy_path(),
        };
        let specs = resolve_one_top_level(m, None);
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].0, PathBuf::from("abcd.pyi"));
        assert_eq!(specs[0].1.name, "abcd");
        assert_eq!(specs[0].1.items.len(), 2);
    }

    #[test]
    fn package_mode_with_pyclass_module() {
        let m = PyModule {
            name: "abcd".to_string(),
            doc: vec![],
            items: vec![
                PyItem::Constant(PyConstant {
                    name: "__version__".to_string(),
                    py_type: "str".to_string(),
                }),
                PyItem::Class(make_class("Layer", Some("abcd.efg"))),
                PyItem::Class(make_class("Operator", None)),
            ],
            source_file: dummy_path(),
        };
        let specs = resolve_one_top_level(m, None);
        assert_eq!(specs.len(), 2);
        let by_path: HashMap<_, _> = specs.into_iter().collect();
        assert!(by_path.contains_key(&PathBuf::from("abcd/__init__.pyi")));
        assert!(by_path.contains_key(&PathBuf::from("abcd/efg.pyi")));
        let init = by_path.get(&PathBuf::from("abcd/__init__.pyi")).unwrap();
        assert_eq!(init.items.len(), 2); // constant + Operator class (no maturin)
        let layers = by_path.get(&PathBuf::from("abcd/efg.pyi")).unwrap();
        assert_eq!(layers.items.len(), 1);
        match &layers.items[0] {
            PyItem::Class(c) => assert_eq!(c.name, "Layer"),
            _ => panic!("expected class"),
        }
    }

    #[test]
    fn maturin_module_name_moves_constants_and_functions_to_extension_stub() {
        let m = PyModule {
            name: "abcd".to_string(),
            doc: vec![],
            items: vec![
                PyItem::Constant(PyConstant {
                    name: "VERSION".to_string(),
                    py_type: "str".to_string(),
                }),
                PyItem::Function(PyFunction {
                    name: "helper".to_string(),
                    doc: vec![],
                    signature_override: None,
                    params: vec![],
                    return_type: PyType {
                        rust_type: syn::parse_quote! { () },
                        override_str: None,
                    },
                    source_file: dummy_path(),
                }),
            ],
            source_file: dummy_path(),
        };
        let specs = resolve_one_top_level(m, Some("abcd.abcd"));
        assert_eq!(specs.len(), 2);
        let by_path: HashMap<_, _> = specs.into_iter().collect();
        assert!(by_path.contains_key(&PathBuf::from("abcd/__init__.pyi")));
        assert!(by_path.contains_key(&PathBuf::from("abcd/abcd.pyi")));
        let init = by_path.get(&PathBuf::from("abcd/__init__.pyi")).unwrap();
        assert!(init.items.is_empty());
        let ext = by_path.get(&PathBuf::from("abcd/abcd.pyi")).unwrap();
        assert_eq!(ext.name, "abcd.abcd");
        assert_eq!(ext.items.len(), 2);
    }

    #[test]
    fn package_mode_emits_empty_init_when_all_items_are_in_submodules() {
        let m = PyModule {
            name: "abcd".to_string(),
            doc: vec!["Root doc.".to_string()],
            items: vec![PyItem::Class(make_class("Layer", Some("abcd.efg")))],
            source_file: dummy_path(),
        };
        let specs = resolve_one_top_level(m, None);
        assert_eq!(specs.len(), 2);
        let by_path: HashMap<_, _> = specs.into_iter().collect();
        let init = by_path.get(&PathBuf::from("abcd/__init__.pyi")).unwrap();
        assert!(init.items.is_empty());
        assert_eq!(init.doc, vec!["Root doc.".to_string()]);
        let layers = by_path.get(&PathBuf::from("abcd/efg.pyi")).unwrap();
        assert_eq!(layers.items.len(), 1);
    }
}
