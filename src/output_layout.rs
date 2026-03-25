//! Resolves output .pyi layout: one file per top-level pymodule, or package layout
//! when `#[pyclass(module = "...")]` is used.
//!
//! The `#[pymodule]` name scopes **collection** (which items belong to this extension). The
//! `#[pyclass(module = "...")]` string is the Python `__module__` for that class: it may differ
//! from the pymodule name (e.g. `_pkg` vs `pkg.abc`). Output paths use a hybrid rule
//! (see [`stub_rel_path`]): when `module` starts with `{pymodule}.`, the first segment is still
//! implicit under `-o` (backward compatible); otherwise the **public** top-level package segment is
//! dropped and the remainder is mirrored as directories (`pkg.abc` → `abc.pyi`;
//! `pkg.cba.foo` → `cba/foo.pyi`).

use crate::collector::{PyItem, PyModule};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Maps each `#[pyclass]` Rust struct/enum **simple name** to the dotted Python module where
/// that class’s stub is emitted. Logic matches [`target_output_module`] for `PyItem::Class`.
/// Used by the generator to insert `from ... import ...` when types reference classes from other stubs.
pub fn rust_class_defining_modules(modules: &[PyModule]) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for top in modules {
        collect_class_modules_recursive(top, &top.name, &mut out);
    }
    out
}

fn collect_class_modules_recursive(
    module: &PyModule,
    root: &str,
    out: &mut HashMap<String, String>,
) {
    for item in &module.items {
        match item {
            PyItem::Class(c) => {
                let m = target_output_module(item, root);
                let m = layout_emit_module_for_imports(root, &m);
                out.insert(c.rust_name.clone(), m);
            }
            PyItem::Module(sub) => {
                collect_class_modules_recursive(sub, root, out);
            }
            _ => {}
        }
    }
}

/// One output stub: path relative to `-o` (e.g. `pkg.pyi`, `sub.pyi`, or `nested/more.pyi`), and the module to generate.
pub type OutputSpec = (PathBuf, PyModule);

/// Resolve each top-level `#[pymodule]` into one or more (path, PyModule) pairs.
/// Single-file mode: one `{name}.pyi` per pymodule when every class either has no `module = "..."`
/// or its `module` equals the pymodule root only.
/// Package mode: `{root}.pyi` for the pymodule root plus additional stubs per distinct class
/// `module` string. When `module` starts with `{root}.`, the first segment is implicit under `-o`
/// (e.g. `abcd.efg` → `efg.pyi`; `pkg.pkg.aaa` → `pkg/aaa.pyi`). When it does not (e.g.
/// `pkg.abc` under pymodule `_pkg`), strip the first dotted segment (public package
/// name) and mirror the rest (`abc.pyi`, `pkg.cba.x` → `cba/x.pyi`). See
/// [`stub_rel_path`].
///
/// Stub placement: `#[pyfunction]`, constants, and `#[pyclass]` **without** `module = "..."` use the
/// top-level `#[pymodule]` name (e.g. `abcd.pyi`). Classes with `#[pyclass(module = "...")]` use
/// that annotation for both bucket name and (via [`stub_rel_path`]) output path. `[tool.maturin]
/// module-name` does not change which `.pyi` file holds functions or unannotated classes.
pub fn resolve(modules: Vec<PyModule>) -> Vec<OutputSpec> {
    let mut out = Vec::new();
    for top in modules {
        let specs = resolve_one_top_level(top);
        out.extend(specs);
    }
    out
}

fn resolve_one_top_level(module: PyModule) -> Vec<OutputSpec> {
    let root = module.name.clone();
    let source_file = module.source_file.clone();
    let doc = module.doc.clone();

    // Flatten all items (including from nested PyItem::Module) and assign each to an output module name.
    let mut buckets: HashMap<String, Vec<PyItem>> = HashMap::new();
    flatten_into_buckets(&module, &root, &mut buckets);
    merge_buckets_sharing_root_stub(&root, &mut buckets);

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

    // Package layout: `{root}.pyi` for the extension root; other modules per [`stub_rel_path`].
    let mut specs = Vec::new();

    let root_items = buckets.remove(&root).unwrap_or_default();
    let root_path = PathBuf::from(format!("{}.pyi", root));
    specs.push((
        root_path,
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
        let path = stub_rel_path(&root, &sub_name);
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
fn flatten_into_buckets(module: &PyModule, root: &str, buckets: &mut HashMap<String, Vec<PyItem>>) {
    for item in &module.items {
        if let PyItem::Module(sub) = item {
            flatten_into_buckets(sub, root, buckets);
            continue;
        }
        let target = target_output_module(item, root);
        buckets.entry(target).or_default().push(item.clone());
    }
}

/// Target Python module name for bucketing: top-level `#[pymodule]` for functions, constants, and
/// classes **without** `#[pyclass(module = "...")]`, else the annotated `module` string as written.
fn target_output_module(item: &PyItem, root: &str) -> String {
    match item {
        PyItem::Class(c) => {
            if let Some(ref m) = c.module {
                return m.clone();
            }
            root.to_string()
        }
        PyItem::Constant(_) | PyItem::Function(_) => root.to_string(),
        PyItem::Module(_) => unreachable!(
            "flatten_into_buckets inlines nested PyItem::Module before target_output_module"
        ),
    }
}

/// `full_module` is a dotted child of `root` (`root` or `root.suffix...`), using a segment
/// boundary match so `root` must not be a mere prefix of the first segment (e.g. `ab` vs `abcd`).
fn is_child_module_of_root(full_module: &str, root: &str) -> bool {
    full_module.len() > root.len()
        && full_module.as_bytes().get(..root.len()) == Some(root.as_bytes())
        && full_module.as_bytes().get(root.len()) == Some(&b'.')
}

/// Relative path for the `.pyi` file under `-o` for Python module `full_module`, given top-level
/// pymodule `root`.
///
/// - `root` → `{root}.pyi`
/// - `{root}.child...` → strip `root.` then apply [`module_name_to_path`] (legacy layout:
///   `pkg.aaa` → `aaa.pyi`, `pkg.pkg` → `pkg.pyi`, `pkg.pkg.aaa` → `pkg/aaa.pyi`)
/// - any other name → drop the first dotted segment (public package), then [`module_name_to_path`]
///   on the remainder (e.g. `pkg.abc` → `abc.pyi`; `pkg.cba.x` →
///   `cba/x.pyi`). A single segment (no dot) uses that segment as the file stem.
fn stub_rel_path(root: &str, full_module: &str) -> PathBuf {
    if full_module == root {
        return PathBuf::from(format!("{}.pyi", root));
    }
    if is_child_module_of_root(full_module, root) {
        let rest = &full_module[root.len() + 1..];
        return module_name_to_path(rest);
    }
    let rest = full_module
        .split_once('.')
        .map(|(_, after)| after)
        .unwrap_or(full_module);
    module_name_to_path(rest)
}

/// For cross-stub imports: if the layout places `full` in the same file as `{root}.pyi`, use
/// `root` as the defining module so the generator does not emit `from root.child import` for
/// symbols in the same stub.
fn layout_emit_module_for_imports(root: &str, full: &str) -> String {
    let p = stub_rel_path(root, full);
    let root_stub = format!("{}.pyi", root);
    if p == Path::new(&root_stub) {
        root.to_string()
    } else {
        full.to_string()
    }
}

/// Merge submodule buckets that resolve to the same path as `{root}.pyi` (e.g. `pkg.pkg` → `pkg.pyi`).
fn merge_buckets_sharing_root_stub(root: &str, buckets: &mut HashMap<String, Vec<PyItem>>) {
    let root_py = PathBuf::from(format!("{}.pyi", root));
    let to_merge: Vec<String> = buckets
        .keys()
        .filter(|k| *k != root && stub_rel_path(root, k.as_str()) == root_py)
        .cloned()
        .collect();
    for k in to_merge {
        let items = buckets.remove(&k).unwrap_or_default();
        buckets.entry(root.to_string()).or_default().extend(items);
    }
}

/// Convert dotted module name (without top-level prefix) to path: "efg" → "efg.pyi", "pkg.aaa" → "pkg/aaa.pyi"
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
            allows_python_subclass: false,
            extends: None,
            is_enum: false,
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
        let specs = resolve_one_top_level(m);
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
        let specs = resolve_one_top_level(m);
        assert_eq!(specs.len(), 2);
        let by_path: HashMap<_, _> = specs.into_iter().collect();
        assert!(by_path.contains_key(&PathBuf::from("abcd.pyi")));
        assert!(by_path.contains_key(&PathBuf::from("efg.pyi")));
        let init = by_path.get(&PathBuf::from("abcd.pyi")).unwrap();
        assert_eq!(init.items.len(), 2); // constant + Operator class (no maturin)
        let efg = by_path.get(&PathBuf::from("efg.pyi")).unwrap();
        assert_eq!(efg.items.len(), 1);
        match &efg.items[0] {
            PyItem::Class(c) => assert_eq!(c.name, "Layer"),
            _ => panic!("expected class"),
        }
    }

    #[test]
    fn maturin_module_name_does_not_move_constants_and_functions_to_extension_stub() {
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
        // Even if pyproject sets module-name to a submodule, stubs stay under the top pymodule .pyi.
        let specs = resolve_one_top_level(m);
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].0, PathBuf::from("abcd.pyi"));
        assert_eq!(specs[0].1.items.len(), 2);
    }

    #[test]
    fn package_mode_emits_empty_root_when_all_items_are_in_submodules() {
        let m = PyModule {
            name: "abcd".to_string(),
            doc: vec!["Root doc.".to_string()],
            items: vec![PyItem::Class(make_class("Layer", Some("abcd.efg")))],
            source_file: dummy_path(),
        };
        let specs = resolve_one_top_level(m);
        assert_eq!(specs.len(), 2);
        let by_path: HashMap<_, _> = specs.into_iter().collect();
        let init = by_path.get(&PathBuf::from("abcd.pyi")).unwrap();
        assert!(init.items.is_empty());
        assert_eq!(init.doc, vec!["Root doc.".to_string()]);
        let efg = by_path.get(&PathBuf::from("efg.pyi")).unwrap();
        assert_eq!(efg.items.len(), 1);
    }

    /// `pkg.pkg.aaa` keeps a `pkg/` directory for the second segment onward.
    #[test]
    fn package_mode_pkg_pkg_aaa_uses_nested_folder() {
        let m = PyModule {
            name: "pkg".to_string(),
            doc: vec![],
            items: vec![
                PyItem::Constant(PyConstant {
                    name: "ROOT".to_string(),
                    py_type: "int".to_string(),
                }),
                PyItem::Class(make_class("X", Some("pkg.pkg.aaa"))),
            ],
            source_file: dummy_path(),
        };
        let specs = resolve_one_top_level(m);
        assert_eq!(specs.len(), 2);
        let by_path: HashMap<_, _> = specs.into_iter().collect();
        assert!(by_path.contains_key(&PathBuf::from("pkg.pyi")));
        assert!(by_path.contains_key(&PathBuf::from("pkg/aaa.pyi")));
    }

    #[test]
    fn rust_class_defining_modules_matches_layout() {
        let m = PyModule {
            name: "abcd".to_string(),
            doc: vec![],
            items: vec![
                PyItem::Class(make_class("Layer", Some("abcd.efg"))),
                PyItem::Class(make_class("Operator", None)),
            ],
            source_file: dummy_path(),
        };
        let map = rust_class_defining_modules(std::slice::from_ref(&m));
        assert_eq!(map.get("Layer").map(String::as_str), Some("abcd.efg"));
        assert_eq!(map.get("Operator").map(String::as_str), Some("abcd"));
    }

    /// `#[pyclass(module = "pkg.pkg")]` shares one stub with the top-level extension module (`pkg.pyi`).
    #[test]
    fn same_name_submodule_collapses_to_root_stub() {
        let m = PyModule {
            name: "pkg".to_string(),
            doc: vec![],
            items: vec![
                PyItem::Constant(PyConstant {
                    name: "VERSION".to_string(),
                    py_type: "str".to_string(),
                }),
                PyItem::Class(make_class("PyDoc", Some("pkg.pkg"))),
            ],
            source_file: dummy_path(),
        };
        let specs = resolve_one_top_level(m);
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].0, PathBuf::from("pkg.pyi"));
        assert_eq!(specs[0].1.items.len(), 2);
    }

    #[test]
    fn rust_class_defining_modules_collapses_root_root() {
        let m = PyModule {
            name: "abcd".to_string(),
            doc: vec![],
            items: vec![PyItem::Class(make_class("X", Some("abcd.abcd")))],
            source_file: dummy_path(),
        };
        let map = rust_class_defining_modules(std::slice::from_ref(&m));
        assert_eq!(map.get("X").map(String::as_str), Some("abcd"));
    }

    /// Extension module name (`_pkg`) may differ from public `#[pyclass(module = "pkg.*")]`.
    #[test]
    fn pyclass_module_independent_of_pymodule_name() {
        let m = PyModule {
            name: "_pkg".to_string(),
            doc: vec![],
            items: vec![
                PyItem::Function(PyFunction {
                    name: "version".to_string(),
                    doc: vec![],
                    signature_override: None,
                    params: vec![],
                    return_type: PyType {
                        rust_type: syn::parse_quote! { () },
                        override_str: None,
                    },
                    source_file: dummy_path(),
                }),
                PyItem::Class(make_class("Operator", Some("pkg.abc"))),
            ],
            source_file: dummy_path(),
        };
        let specs = resolve_one_top_level(m);
        assert_eq!(specs.len(), 2);
        let by_path: HashMap<_, _> = specs.into_iter().collect();
        assert!(by_path.contains_key(&PathBuf::from("_pkg.pyi")));
        assert!(by_path.contains_key(&PathBuf::from("abc.pyi")));
        let ext = by_path.get(&PathBuf::from("_pkg.pyi")).unwrap();
        assert_eq!(ext.items.len(), 1);
        let op = by_path.get(&PathBuf::from("abc.pyi")).unwrap();
        assert_eq!(op.name, "pkg.abc");
        assert_eq!(op.items.len(), 1);
    }

    #[test]
    fn rust_class_defining_modules_independent_pyclass_module() {
        let m = PyModule {
            name: "_pkg".to_string(),
            doc: vec![],
            items: vec![PyItem::Class(make_class("Operator", Some("pkg.abc")))],
            source_file: dummy_path(),
        };
        let map = rust_class_defining_modules(std::slice::from_ref(&m));
        assert_eq!(map.get("Operator").map(String::as_str), Some("pkg.abc"));
    }

    #[test]
    fn pyclass_public_module_strips_only_top_level_package() {
        let m = PyModule {
            name: "_pkg".to_string(),
            doc: vec![],
            items: vec![PyItem::Class(make_class("X", Some("pkg.cba.abc")))],
            source_file: dummy_path(),
        };
        let specs = resolve_one_top_level(m);
        assert_eq!(specs.len(), 2);
        let paths: Vec<_> = specs.iter().map(|(p, _)| p.clone()).collect();
        assert!(paths.contains(&PathBuf::from("_pkg.pyi")));
        assert!(paths.contains(&PathBuf::from("cba/abc.pyi")));
    }
}
