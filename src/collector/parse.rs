use super::model::*;
use crate::config::Config;
use std::path::Path;
use syn::{
    Attribute, Expr, FnArg, ImplItem, Item, ItemFn, ItemMod, Meta,
    Pat, ReturnType, Signature, Type,
};

// ── Entry point ──────────────────────────────────────────────────────────────

/// Extract all `#[pymodule]` items from a parsed file.
pub fn extract_modules_from_file(
    file: &syn::File,
    path: &Path,
    config: &Config,
) -> Vec<PyModule> {
    // First pass: collect all #[pymethods] impl blocks keyed by struct name,
    // so we can attach them to PyClass items found later.
    let impl_map = collect_impl_blocks(&file.items, path, config);

    // Second pass: collect modules
    let mut result = Vec::new();
    for item in &file.items {
        match item {
            // Style A: #[pymodule] mod Foo { ... }
            Item::Mod(m) if has_attr(&m.attrs, "pymodule") => {
                if let Some(module) = parse_mod_style_module(m, path, config, &impl_map) {
                    result.push(module);
                }
            }
            // Style B: #[pymodule] fn foo(m: &Bound<PyModule>) -> PyResult<()> { ... }
            Item::Fn(f) if has_attr(&f.attrs, "pymodule") => {
                if let Some(module) = parse_fn_style_module(f, &file.items, path, config, &impl_map) {
                    result.push(module);
                }
            }
            _ => {}
        }
    }
    result
}

// ── Style A: inline mod ──────────────────────────────────────────────────────

fn parse_mod_style_module(
    m: &ItemMod,
    path: &Path,
    config: &Config,
    impl_map: &ImplMap,
) -> Option<PyModule> {
    let (_, items) = m.content.as_ref()?;
    let name = m.ident.to_string();
    let doc = extract_doc(&m.attrs);

    let py_items = collect_items_from_list(items, path, config, impl_map);

    Some(PyModule {
        name,
        doc,
        items: py_items,
        source_file: path.to_path_buf(),
    })
}

fn collect_items_from_list(
    items: &[Item],
    path: &Path,
    config: &Config,
    impl_map: &ImplMap,
) -> Vec<PyItem> {
    let mut result = Vec::new();
    for item in items {
        match item {
            Item::Fn(f) if has_attr(&f.attrs, "pyfunction") => {
                if let Some(func) = parse_pyfunction(f, path, config) {
                    result.push(PyItem::Function(func));
                }
            }
            Item::Struct(s) if has_attr(&s.attrs, "pyclass") => {
                let class = parse_pyclass_struct(
                    &s.ident.to_string(),
                    &s.attrs,
                    path,
                    impl_map,
                    config,
                );
                result.push(PyItem::Class(class));
            }
            Item::Enum(e) if has_attr(&e.attrs, "pyclass") => {
                let class = parse_pyclass_struct(
                    &e.ident.to_string(),
                    &e.attrs,
                    path,
                    impl_map,
                    config,
                );
                result.push(PyItem::Class(class));
            }
            // Nested submodule
            Item::Mod(sub) if has_attr(&sub.attrs, "pymodule") => {
                if let Some(sub_mod) = parse_mod_style_module(sub, path, config, impl_map) {
                    result.push(PyItem::Module(sub_mod));
                }
            }
            _ => {}
        }
    }
    result
}

// ── Style B: function-based module ──────────────────────────────────────────

fn parse_fn_style_module(
    f: &ItemFn,
    file_items: &[Item],
    path: &Path,
    config: &Config,
    impl_map: &ImplMap,
) -> Option<PyModule> {
    let name = f.sig.ident.to_string();
    let doc = extract_doc(&f.attrs);

    let mut py_items = Vec::new();

    // Walk the function body looking for m.add_function(...) / m.add_class::<T>()
    for stmt in &f.block.stmts {
        collect_add_calls_from_stmt(stmt, file_items, path, config, impl_map, &mut py_items);
    }

    Some(PyModule {
        name,
        doc,
        items: py_items,
        source_file: path.to_path_buf(),
    })
}

fn collect_add_calls_from_stmt(
    stmt: &syn::Stmt,
    file_items: &[Item],
    path: &Path,
    config: &Config,
    impl_map: &ImplMap,
    out: &mut Vec<PyItem>,
) {
    let expr = match stmt {
        syn::Stmt::Expr(e, _) => e,
        syn::Stmt::Local(l) => {
            if let Some(init) = &l.init {
                collect_add_calls_from_expr(&init.expr, file_items, path, config, impl_map, out);
            }
            return;
        }
        _ => return,
    };
    collect_add_calls_from_expr(expr, file_items, path, config, impl_map, out);
}

fn collect_add_calls_from_expr(
    expr: &Expr,
    file_items: &[Item],
    path: &Path,
    config: &Config,
    impl_map: &ImplMap,
    out: &mut Vec<PyItem>,
) {
    match expr {
        // m.add_function(...)?  or  m.add_class::<T>()?
        Expr::Try(t) => {
            collect_add_calls_from_expr(&t.expr, file_items, path, config, impl_map, out)
        }
        Expr::MethodCall(mc) => {
            let method = mc.method.to_string();
            match method.as_str() {
                "add_function" => {
                    // m.add_function(wrap_pyfunction!(foo, m)?)
                    // Try to extract function name from the macro argument
                    if let Some(fn_name) = extract_wrap_pyfunction_name(mc.args.first()) {
                        if let Some(func) = find_pyfunction_by_name(&fn_name, file_items, path, config) {
                            out.push(PyItem::Function(func));
                        }
                    }
                }
                "add_class" => {
                    // m.add_class::<MyType>()
                    if let Some(type_name) = extract_turbofish_type_name(&mc.method, &mc.turbofish) {
                        let class = parse_pyclass_struct(&type_name, &[], path, impl_map, config);
                        out.push(PyItem::Class(class));
                    }
                }
                _ => {}
            }
        }
        _ => {}
    }
}

/// Extract the function name from `wrap_pyfunction!(foo, m)` or `wrap_pyfunction!(foo)`.
fn extract_wrap_pyfunction_name(arg: Option<&Expr>) -> Option<String> {
    let expr = arg?;
    // The macro call becomes a syn::ExprMacro
    if let Expr::Macro(m) = expr {
        let macro_name = m.mac.path.segments.last()?.ident.to_string();
        if macro_name == "wrap_pyfunction" {
            // Tokens: `foo , m` — first token is the ident
            let mut tokens = m.mac.tokens.clone().into_iter();
            if let Some(proc_macro2::TokenTree::Ident(id)) = tokens.next() {
                return Some(id.to_string());
            }
        }
    }
    // Also handle if it was already unwrapped (after `?`)
    if let Expr::Try(t) = expr {
        return extract_wrap_pyfunction_name(Some(&t.expr));
    }
    None
}

/// Extract `MyType` from `add_class::<MyType>()` turbofish.
fn extract_turbofish_type_name(
    _method: &syn::Ident,
    turbofish: &Option<syn::AngleBracketedGenericArguments>,
) -> Option<String> {
    let tf = turbofish.as_ref()?;
    for arg in &tf.args {
        if let syn::GenericArgument::Type(Type::Path(tp)) = arg {
            return Some(tp.path.segments.last()?.ident.to_string());
        }
    }
    None
}

/// Find a `#[pyfunction] fn <name>` in the file's top-level items.
fn find_pyfunction_by_name(
    name: &str,
    file_items: &[Item],
    path: &Path,
    config: &Config,
) -> Option<PyFunction> {
    for item in file_items {
        if let Item::Fn(f) = item {
            if f.sig.ident == name && has_attr(&f.attrs, "pyfunction") {
                return parse_pyfunction(f, path, config);
            }
        }
    }
    None
}

// ── #[pyfunction] parsing ────────────────────────────────────────────────────

pub fn parse_pyfunction(f: &ItemFn, path: &Path, config: &Config) -> Option<PyFunction> {
    let name = f.sig.ident.to_string();
    let doc = extract_doc(&f.attrs);
    let signature_override = extract_pyo3_signature(&f.attrs);
    let params = parse_params(&f.sig, config);
    let return_type = parse_return_type(&f.sig.output, config);

    Some(PyFunction {
        name,
        doc,
        signature_override,
        params,
        return_type,
        source_file: path.to_path_buf(),
    })
}

fn parse_params(sig: &Signature, config: &Config) -> Vec<PyParam> {
    let mut params = Vec::new();
    for input in &sig.inputs {
        match input {
            FnArg::Receiver(_) => {} // skip `self`
            FnArg::Typed(pt) => {
                // Skip pyo3 injected parameters: &Python<'_>, &Bound<PyModule>, etc.
                if is_pyo3_injected_param(&pt.ty) {
                    continue;
                }
                let name = match pt.pat.as_ref() {
                    Pat::Ident(pi) => pi.ident.to_string(),
                    _ => "_".to_string(),
                };
                let override_str = lookup_type_override(&pt.ty, config);
                params.push(PyParam {
                    name,
                    ty: PyType {
                        rust_type: *pt.ty.clone(),
                        override_str,
                    },
                    default: None,
                    kind: ParamKind::Regular,
                });
            }
        }
    }
    params
}

fn parse_return_type(output: &ReturnType, config: &Config) -> PyType {
    match output {
        ReturnType::Default => PyType {
            rust_type: syn::parse_quote! { () },
            override_str: None,
        },
        ReturnType::Type(_, ty) => {
            let override_str = lookup_type_override(ty, config);
            PyType {
                rust_type: *ty.clone(),
                override_str,
            }
        }
    }
}

// ── #[pyclass] parsing ───────────────────────────────────────────────────────

fn parse_pyclass_struct(
    name: &str,
    attrs: &[Attribute],
    path: &Path,
    impl_map: &ImplMap,
    config: &Config,
) -> PyClass {
    let doc = extract_doc(attrs);
    let methods = impl_map
        .get(name)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|item| parse_pymethod(&item, config))
        .collect();

    PyClass {
        name: name.to_string(),
        doc,
        methods,
        source_file: path.to_path_buf(),
    }
}

fn parse_pymethod(item: &ImplItem, config: &Config) -> Option<PyMethod> {
    let ImplItem::Fn(m) = item else { return None };
    if !has_any_pyo3_method_attr(&m.attrs) {
        return None;
    }

    let name = m.sig.ident.to_string();
    let doc = extract_doc(&m.attrs);
    let params = parse_params(&m.sig, config);
    let return_type = parse_return_type(&m.sig.output, config);
    let kind = detect_method_kind(&m.attrs, &name);

    Some(PyMethod {
        name,
        doc,
        kind,
        params,
        return_type,
    })
}

fn detect_method_kind(attrs: &[Attribute], name: &str) -> MethodKind {
    if has_attr(attrs, "new") {
        return MethodKind::New;
    }
    if has_attr(attrs, "staticmethod") {
        return MethodKind::Static;
    }
    if has_attr(attrs, "classmethod") {
        return MethodKind::Class;
    }
    // #[getter] / #[getter(rename)]
    if let Some(prop_name) = extract_getter_name(attrs, name) {
        return MethodKind::Getter(prop_name);
    }
    if let Some(prop_name) = extract_setter_name(attrs, name) {
        return MethodKind::Setter(prop_name);
    }
    MethodKind::Instance
}

fn extract_getter_name(attrs: &[Attribute], fn_name: &str) -> Option<String> {
    for attr in attrs {
        if attr.path().is_ident("getter") {
            let rename = extract_attr_string_arg(attr);
            return Some(rename.unwrap_or_else(|| fn_name.to_string()));
        }
    }
    None
}

fn extract_setter_name(attrs: &[Attribute], fn_name: &str) -> Option<String> {
    for attr in attrs {
        if attr.path().is_ident("setter") {
            let rename = extract_attr_string_arg(attr);
            return Some(rename.unwrap_or_else(|| fn_name.to_string()));
        }
    }
    None
}

fn extract_attr_string_arg(attr: &Attribute) -> Option<String> {
    if let Meta::List(ml) = &attr.meta {
        // Try to parse the tokens as a single ident or string literal
        let tokens_str = ml.tokens.to_string();
        let trimmed = tokens_str.trim().trim_matches('"').to_string();
        if !trimmed.is_empty() {
            return Some(trimmed);
        }
    }
    None
}

// ── impl block collection ────────────────────────────────────────────────────

/// Maps struct/enum name → list of ImplItem from `#[pymethods]` blocks.
type ImplMap = std::collections::HashMap<String, Vec<ImplItem>>;

fn collect_impl_blocks(items: &[Item], _path: &Path, _config: &Config) -> ImplMap {
    let mut map: ImplMap = std::collections::HashMap::new();
    for item in items {
        if let Item::Impl(imp) = item {
            if !has_attr(&imp.attrs, "pymethods") {
                continue;
            }
            // Get the struct/enum name from `impl MyType { ... }`
            if let Type::Path(tp) = imp.self_ty.as_ref() {
                if let Some(seg) = tp.path.segments.last() {
                    let name = seg.ident.to_string();
                    map.entry(name).or_default().extend(imp.items.clone());
                }
            }
        }
    }
    map
}

// ── Attribute helpers ────────────────────────────────────────────────────────

pub fn has_attr(attrs: &[Attribute], name: &str) -> bool {
    attrs.iter().any(|a| a.path().is_ident(name))
}

fn has_any_pyo3_method_attr(attrs: &[Attribute]) -> bool {
    const METHOD_ATTRS: &[&str] = &[
        "pyo3", "new", "getter", "setter", "staticmethod", "classmethod",
        "pyfunction", // sometimes used directly
    ];
    METHOD_ATTRS.iter().any(|name| has_attr(attrs, name))
        || attrs.iter().any(|a| {
            // Check for bare `fn` without attributes — regular instance methods in #[pymethods]
            // are included by default, so we also want those.
            // We'll collect all methods from #[pymethods] impl blocks regardless.
            let _ = a;
            false
        })
}

/// Extract `/// doc comment` text from attributes.
pub fn extract_doc(attrs: &[Attribute]) -> Vec<String> {
    attrs
        .iter()
        .filter_map(|a| {
            if !a.path().is_ident("doc") {
                return None;
            }
            if let Meta::NameValue(nv) = &a.meta {
                if let Expr::Lit(lit) = &nv.value {
                    if let syn::Lit::Str(s) = &lit.lit {
                        // syn includes a leading space: `" Some text"` → strip it
                        return Some(s.value().trim_start_matches(' ').to_string());
                    }
                }
            }
            None
        })
        .collect()
}

/// Extract `#[pyo3(signature = (...))]` as a raw string.
fn extract_pyo3_signature(attrs: &[Attribute]) -> Option<String> {
    for attr in attrs {
        if !attr.path().is_ident("pyo3") {
            continue;
        }
        if let Meta::List(ml) = &attr.meta {
            let tokens = ml.tokens.to_string();
            // Look for `signature = (...)`
            if let Some(start) = tokens.find("signature") {
                let rest = &tokens[start..];
                if let Some(eq_pos) = rest.find('=') {
                    return Some(rest[eq_pos + 1..].trim().to_string());
                }
            }
        }
    }
    None
}

/// Returns true for pyo3 "injected" parameter types that should not appear in the Python stub:
/// `Python<'_>`, `&Bound<'_, PyModule>`, etc.
fn is_pyo3_injected_param(ty: &Type) -> bool {
    let Type::Reference(r) = ty else {
        // Python<'_> (by value) is also injected
        if let Type::Path(tp) = ty {
            if let Some(seg) = tp.path.segments.last() {
                return matches!(seg.ident.to_string().as_str(), "Python");
            }
        }
        return false;
    };
    if let Type::Path(tp) = r.elem.as_ref() {
        if let Some(seg) = tp.path.segments.last() {
            let name = seg.ident.to_string();
            return matches!(name.as_str(), "Python" | "PyModule" | "Bound" | "Borrowed");
        }
    }
    false
}

/// Look up a Rust type's fully-qualified path in the user's [type_map] config.
fn lookup_type_override(ty: &Type, config: &Config) -> Option<String> {
    if config.type_map.is_empty() {
        return None;
    }
    // Convert type to a rough string key for lookup
    let key = type_to_key(ty);
    config.type_map.get(&key).cloned()
}

fn type_to_key(ty: &Type) -> String {
    match ty {
        Type::Path(tp) => tp
            .path
            .segments
            .iter()
            .map(|s| s.ident.to_string())
            .collect::<Vec<_>>()
            .join("::"),
        Type::Reference(r) => type_to_key(&r.elem),
        _ => String::new(),
    }
}
