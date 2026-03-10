use super::model::*;
use crate::config::Config;
use std::collections::HashMap;
use std::path::Path;
use syn::{
    Attribute, Expr, FnArg, ImplItem, Item, ItemFn, ItemMod, Meta, Pat, ReturnType, Signature,
    Type, TypePath,
};

/// Maximum recursion depth when expanding type aliases to avoid infinite loops.
/// Aliases nested deeper than this value are returned unexpanded.
const MAX_TYPE_ALIAS_DEPTH: u8 = 10;

// ── Entry point ──────────────────────────────────────────────────────────────

/// Build a map: Rust type alias name -> underlying `syn::Type` from all `type Foo = ...` in the crate.
/// Used to resolve e.g. `PyBbox` to `(f32, f32, f32, f32)` when mapping to Python types.
/// If the same alias name appears in multiple files, the last occurrence wins.
pub fn build_type_alias_map(files: &[(std::path::PathBuf, syn::File)]) -> HashMap<String, Type> {
    let mut map = HashMap::new();
    for (_path, file) in files {
        collect_type_aliases_from_items(&file.items, &mut map);
    }
    map
}

fn collect_type_aliases_from_items(items: &[Item], map: &mut HashMap<String, Type>) {
    for item in items {
        match item {
            Item::Type(ta) => {
                map.insert(ta.ident.to_string(), (*ta.ty).clone());
            }
            Item::Mod(m) => {
                if let Some((_, content)) = &m.content {
                    collect_type_aliases_from_items(content, map);
                }
            }
            _ => {}
        }
    }
}

/// Recursively expand type aliases in `ty` using `map`. Stops at depth limit to avoid cycles.
fn expand_type_aliases(ty: &Type, map: &HashMap<String, Type>, depth: u8) -> Type {
    if depth >= MAX_TYPE_ALIAS_DEPTH {
        return ty.clone();
    }
    match ty {
        Type::Path(tp) if is_single_ident_path(tp) => {
            let name = tp.path.segments.last().unwrap().ident.to_string();
            if let Some(underlying) = map.get(&name) {
                return expand_type_aliases(underlying, map, depth + 1);
            }
            ty.clone()
        }
        Type::Path(tp) => {
            let mut new_tp = tp.clone();
            if let Some(last) = new_tp.path.segments.last_mut()
                && let syn::PathArguments::AngleBracketed(ref mut ab) = last.arguments
            {
                for arg in ab.args.iter_mut() {
                    if let syn::GenericArgument::Type(t) = arg {
                        *t = expand_type_aliases(t, map, depth + 1);
                    }
                }
            }
            Type::Path(new_tp)
        }
        Type::Tuple(t) => {
            let mut elems = syn::punctuated::Punctuated::new();
            for pair in t.elems.pairs() {
                elems.push_value(expand_type_aliases(pair.value(), map, depth + 1));
                if let Some(punct) = pair.punct() {
                    elems.push_punct(**punct);
                }
            }
            Type::Tuple(syn::TypeTuple {
                paren_token: t.paren_token,
                elems,
            })
        }
        Type::Reference(r) => {
            let elem = expand_type_aliases(&r.elem, map, depth + 1);
            Type::Reference(syn::TypeReference {
                elem: Box::new(elem),
                ..r.clone()
            })
        }
        _ => ty.clone(),
    }
}

fn is_single_ident_path(tp: &TypePath) -> bool {
    tp.path.leading_colon.is_none()
        && tp.path.segments.len() == 1
        && matches!(tp.path.segments[0].arguments, syn::PathArguments::None)
}

/// Build a map: Rust type name (struct/enum ident) -> Python name from `#[pyclass(name = "...")]`.
/// Used so that when we see `m.add_class::<PyPdfDocument>()` we can emit `class PdfDocument`.
/// If the same Rust type name appears in multiple files, the last occurrence wins.
pub fn build_pyclass_name_map(
    files: &[(std::path::PathBuf, syn::File)],
) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for (_path, file) in files {
        collect_pyclass_names_from_items(&file.items, &mut map);
    }
    map
}

fn collect_pyclass_names_from_items(items: &[Item], map: &mut HashMap<String, String>) {
    for item in items {
        match item {
            Item::Struct(s) if has_attr(&s.attrs, "pyclass") => {
                if let Some(python_name) = extract_pyo3_name(&s.attrs) {
                    map.insert(s.ident.to_string(), python_name);
                }
            }
            Item::Enum(e) if has_attr(&e.attrs, "pyclass") => {
                if let Some(python_name) = extract_pyo3_name(&e.attrs) {
                    map.insert(e.ident.to_string(), python_name);
                }
            }
            Item::Mod(m) => {
                if let Some((_, content)) = &m.content {
                    collect_pyclass_names_from_items(content, map);
                }
            }
            _ => {}
        }
    }
}

/// Extract all `#[pymodule]` items from a parsed file.
pub fn extract_modules_from_file(
    file: &syn::File,
    path: &Path,
    config: &Config,
    pyclass_name_map: &HashMap<String, String>,
    type_alias_map: &HashMap<String, Type>,
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
                if let Some(module) =
                    parse_mod_style_module(m, path, config, &impl_map, type_alias_map)
                {
                    result.push(module);
                }
            }
            // Style B: #[pymodule] fn foo(m: &Bound<PyModule>) -> PyResult<()> { ... }
            Item::Fn(f) if has_attr(&f.attrs, "pymodule") => {
                if let Some(module) = parse_fn_style_module(
                    f,
                    &file.items,
                    path,
                    config,
                    &impl_map,
                    pyclass_name_map,
                    type_alias_map,
                ) {
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
    type_alias_map: &HashMap<String, Type>,
) -> Option<PyModule> {
    let (_, items) = m.content.as_ref()?;
    let name = m.ident.to_string();
    let doc = extract_doc(&m.attrs);

    let py_items = collect_items_from_list(items, path, config, impl_map, type_alias_map);

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
    type_alias_map: &HashMap<String, Type>,
) -> Vec<PyItem> {
    let mut result = Vec::new();
    for item in items {
        match item {
            Item::Fn(f) if has_attr(&f.attrs, "pyfunction") => {
                if let Some(func) = parse_pyfunction(f, path, config, type_alias_map) {
                    result.push(PyItem::Function(func));
                }
            }
            Item::Struct(s) if has_attr(&s.attrs, "pyclass") => {
                let name = extract_pyo3_name(&s.attrs).unwrap_or_else(|| s.ident.to_string());
                let rust_name = s.ident.to_string();
                let class = parse_pyclass_struct(
                    &name,
                    &rust_name,
                    &s.attrs,
                    path,
                    impl_map,
                    config,
                    type_alias_map,
                );
                result.push(PyItem::Class(class));
            }
            Item::Enum(e) if has_attr(&e.attrs, "pyclass") => {
                let name = extract_pyo3_name(&e.attrs).unwrap_or_else(|| e.ident.to_string());
                let rust_name = e.ident.to_string();
                let class = parse_pyclass_struct(
                    &name,
                    &rust_name,
                    &e.attrs,
                    path,
                    impl_map,
                    config,
                    type_alias_map,
                );
                result.push(PyItem::Class(class));
            }
            // Nested submodule
            Item::Mod(sub) if has_attr(&sub.attrs, "pymodule") => {
                if let Some(sub_mod) =
                    parse_mod_style_module(sub, path, config, impl_map, type_alias_map)
                {
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
    pyclass_name_map: &HashMap<String, String>,
    type_alias_map: &HashMap<String, Type>,
) -> Option<PyModule> {
    let name = f.sig.ident.to_string();
    let doc = extract_doc(&f.attrs);

    let mut py_items = Vec::new();

    // Walk the function body looking for m.add_function(...) / m.add_class::<T>()
    let ctx = CollectAddCallsContext {
        file_items,
        path,
        config,
        impl_map,
        pyclass_name_map,
        type_alias_map,
    };
    for stmt in &f.block.stmts {
        collect_add_calls_from_stmt(stmt, &ctx, &mut py_items);
    }

    Some(PyModule {
        name,
        doc,
        items: py_items,
        source_file: path.to_path_buf(),
    })
}

struct CollectAddCallsContext<'a> {
    file_items: &'a [Item],
    path: &'a Path,
    config: &'a Config,
    impl_map: &'a ImplMap,
    pyclass_name_map: &'a HashMap<String, String>,
    type_alias_map: &'a HashMap<String, Type>,
}

fn collect_add_calls_from_stmt(
    stmt: &syn::Stmt,
    ctx: &CollectAddCallsContext<'_>,
    out: &mut Vec<PyItem>,
) {
    let expr = match stmt {
        syn::Stmt::Expr(e, _) => e,
        syn::Stmt::Local(l) => {
            if let Some(init) = &l.init {
                collect_add_calls_from_expr(&init.expr, ctx, out);
            }
            return;
        }
        _ => return,
    };
    collect_add_calls_from_expr(expr, ctx, out);
}

fn collect_add_calls_from_expr(
    expr: &Expr,
    ctx: &CollectAddCallsContext<'_>,
    out: &mut Vec<PyItem>,
) {
    match expr {
        // m.add_function(...)?  or  m.add_class::<T>()?
        Expr::Try(t) => collect_add_calls_from_expr(&t.expr, ctx, out),
        Expr::MethodCall(mc) => {
            let method = mc.method.to_string();
            match method.as_str() {
                "add_function" => {
                    // m.add_function(wrap_pyfunction!(foo, m)?)
                    if let Some(fn_name) = extract_wrap_pyfunction_name(mc.args.first())
                        && let Some(func) = find_pyfunction_by_name(
                            &fn_name,
                            ctx.file_items,
                            ctx.path,
                            ctx.config,
                            ctx.type_alias_map,
                        )
                    {
                        out.push(PyItem::Function(func));
                    }
                }
                "add_class" => {
                    // m.add_class::<PyPdfDocument>() — use pyclass_name_map for #[pyclass(name = "PdfDocument")]
                    // Style B: we don't have the struct item here, so attrs are empty and the class has no doc in the stub.
                    if let Some(type_name) = extract_turbofish_type_name(&mc.method, &mc.turbofish)
                    {
                        let rust_name = type_name.clone();
                        let class_name = ctx
                            .pyclass_name_map
                            .get(&type_name)
                            .cloned()
                            .unwrap_or(type_name);
                        let class = parse_pyclass_struct(
                            &class_name,
                            &rust_name,
                            &[],
                            ctx.path,
                            ctx.impl_map,
                            ctx.config,
                            ctx.type_alias_map,
                        );
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
    type_alias_map: &HashMap<String, Type>,
) -> Option<PyFunction> {
    for item in file_items {
        if let Item::Fn(f) = item
            && f.sig.ident == name
            && has_attr(&f.attrs, "pyfunction")
        {
            return parse_pyfunction(f, path, config, type_alias_map);
        }
    }
    None
}

// ── #[pyfunction] parsing ────────────────────────────────────────────────────

pub fn parse_pyfunction(
    f: &ItemFn,
    path: &Path,
    config: &Config,
    type_alias_map: &HashMap<String, Type>,
) -> Option<PyFunction> {
    // #[pyo3(name = "foo")] overrides the Rust function name
    let name = extract_pyo3_name(&f.attrs).unwrap_or_else(|| f.sig.ident.to_string());
    let doc = extract_doc(&f.attrs);
    let signature_override = extract_pyo3_signature(&f.attrs);
    let params = parse_params(&f.sig, config, type_alias_map);
    let return_type = parse_return_type(&f.sig.output, config, type_alias_map);

    Some(PyFunction {
        name,
        doc,
        signature_override,
        params,
        return_type,
        source_file: path.to_path_buf(),
    })
}

fn parse_params(
    sig: &Signature,
    config: &Config,
    type_alias_map: &HashMap<String, Type>,
) -> Vec<PyParam> {
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
                let rust_type = expand_type_aliases(&pt.ty, type_alias_map, 0);
                let override_str = lookup_type_override(&pt.ty, config)
                    .or_else(|| lookup_type_override(&rust_type, config));
                params.push(PyParam {
                    name,
                    ty: PyType {
                        rust_type,
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

fn parse_return_type(
    output: &ReturnType,
    config: &Config,
    type_alias_map: &HashMap<String, Type>,
) -> PyType {
    match output {
        ReturnType::Default => PyType {
            rust_type: syn::parse_quote! { () },
            override_str: None,
        },
        ReturnType::Type(_, ty) => {
            let rust_type = expand_type_aliases(ty, type_alias_map, 0);
            let override_str = lookup_type_override(ty, config)
                .or_else(|| lookup_type_override(&rust_type, config));
            PyType {
                rust_type,
                override_str,
            }
        }
    }
}

// ── #[pyclass] parsing ───────────────────────────────────────────────────────

/// `display_name`: name used in .pyi (Python name, from #[pyclass(name = "...")] or ident).
/// `rust_name_for_impl`: Rust type name for looking up #[pymethods] impl block.
fn parse_pyclass_struct(
    display_name: &str,
    rust_name_for_impl: &str,
    attrs: &[Attribute],
    path: &Path,
    impl_map: &ImplMap,
    config: &Config,
    type_alias_map: &HashMap<String, Type>,
) -> PyClass {
    let doc = extract_doc(attrs);
    let methods = impl_map
        .get(rust_name_for_impl)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|item| parse_pymethod(&item, config, type_alias_map))
        .collect();

    PyClass {
        name: display_name.to_string(),
        rust_name: rust_name_for_impl.to_string(),
        doc,
        methods,
        source_file: path.to_path_buf(),
    }
}

fn parse_pymethod(
    item: &ImplItem,
    config: &Config,
    type_alias_map: &HashMap<String, Type>,
) -> Option<PyMethod> {
    let ImplItem::Fn(m) = item else { return None };

    // #[pyo3(name = "foo")] overrides the Rust method name
    let name = extract_pyo3_name(&m.attrs).unwrap_or_else(|| m.sig.ident.to_string());
    let doc = extract_doc(&m.attrs);
    let params = parse_params(&m.sig, config, type_alias_map);
    let return_type = parse_return_type(&m.sig.output, config, type_alias_map);
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
            if let Type::Path(tp) = imp.self_ty.as_ref()
                && let Some(seg) = tp.path.segments.last()
            {
                let name = seg.ident.to_string();
                map.entry(name).or_default().extend(imp.items.clone());
            }
        }
    }
    map
}

// ── Attribute helpers ────────────────────────────────────────────────────────

pub fn has_attr(attrs: &[Attribute], name: &str) -> bool {
    attrs.iter().any(|a| a.path().is_ident(name))
}

/// Extract `/// doc comment` text from attributes.
pub fn extract_doc(attrs: &[Attribute]) -> Vec<String> {
    attrs
        .iter()
        .filter_map(|a| {
            if !a.path().is_ident("doc") {
                return None;
            }
            if let Meta::NameValue(nv) = &a.meta
                && let Expr::Lit(lit) = &nv.value
                && let syn::Lit::Str(s) = &lit.lit
            {
                // syn includes a leading space: `" Some text"` → strip it
                return Some(s.value().trim_start_matches(' ').to_string());
            }
            None
        })
        .collect()
}

/// Extract `#[pyo3(signature = (...))]` as a raw string of the parameter list
/// **without** the surrounding parentheses, e.g. `page=None, clip=None, **kwargs`.
/// Only a single outer pair of parentheses is stripped; nested parens are preserved.
/// Malformed or multi-line values may yield unexpected results.
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
                    let value = rest[eq_pos + 1..].trim();
                    // Strip the outer parentheses that pyo3 requires
                    let inner = if value.starts_with('(') && value.ends_with(')') {
                        &value[1..value.len() - 1]
                    } else {
                        value
                    };
                    return Some(normalize_py_literals(inner.trim()));
                }
            }
        }
    }
    None
}

/// Replace whole-word Rust boolean literals (`true`/`false`) with their Python equivalents
/// (`True`/`False`). `None` is spelled the same in both languages and requires no replacement.
///
/// Handles word-boundary checks so that identifiers containing `true` or `false`
/// (e.g. `extract_text`) are not accidentally modified.
fn normalize_py_literals(s: &str) -> String {
    const REPLACEMENTS: &[(&str, &str)] = &[("true", "True"), ("false", "False")];

    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;

    'outer: while i < bytes.len() {
        for (from, to) in REPLACEMENTS {
            let flen = from.len();
            if bytes[i..].starts_with(from.as_bytes()) {
                let before_ok =
                    i == 0 || !(bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_');
                let after_ok = i + flen >= bytes.len()
                    || !(bytes[i + flen].is_ascii_alphanumeric() || bytes[i + flen] == b'_');
                if before_ok && after_ok {
                    out.push_str(to);
                    i += flen;
                    continue 'outer;
                }
            }
        }
        let ch = s[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// Extract `#[pyo3(name = "foo")]` or `#[pyclass(name = "Foo")]` rename.
/// Returns the override name if present, otherwise None.
/// The name is expected to be a simple identifier; escaped quotes inside the string are not handled.
pub fn extract_pyo3_name(attrs: &[Attribute]) -> Option<String> {
    for attr in attrs {
        // Handles both `#[pyo3(name = "foo")]` and `#[pyclass(name = "Foo")]`
        if !attr.path().is_ident("pyo3") && !attr.path().is_ident("pyclass") {
            continue;
        }
        if let Meta::List(ml) = &attr.meta {
            let tokens = ml.tokens.to_string();
            // Look for `name = "..."` — find the opening quote after `name =`
            if let Some(name_pos) = tokens.find("name") {
                let after_name = tokens[name_pos + 4..].trim_start();
                if let Some(after_eq) = after_name.strip_prefix('=') {
                    let after_eq = after_eq.trim_start();
                    if let Some(inner) = after_eq.strip_prefix('"') {
                        // Extract content between the first pair of quotes
                        if let Some(end) = inner.find('"') {
                            let name = &inner[..end];
                            if !name.is_empty() {
                                return Some(name.to_string());
                            }
                        }
                    }
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
        if let Type::Path(tp) = ty
            && let Some(seg) = tp.path.segments.last()
        {
            // Python<'_> by value, and pyo3 self-ref types used instead of &self / &mut self
            return matches!(
                seg.ident.to_string().as_str(),
                "Python" | "PyRef" | "PyRefMut"
            );
        }
        return false;
    };
    if let Type::Path(tp) = r.elem.as_ref()
        && let Some(seg) = tp.path.segments.last()
    {
        let name = seg.ident.to_string();
        if matches!(name.as_str(), "Python" | "PyModule") {
            return true;
        }
        // &Bound<'_, T> / &Borrowed<'_, T> — only injected when T is PyModule
        if matches!(name.as_str(), "Bound" | "Borrowed") {
            return match &seg.arguments {
                syn::PathArguments::AngleBracketed(ab) => ab.args.iter().any(|a| {
                    matches!(a, syn::GenericArgument::Type(Type::Path(tp))
                        if tp.path.segments.last().map(|s| s.ident == "PyModule").unwrap_or(false))
                }),
                _ => false,
            };
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, RenderPolicy};
    use std::path::Path;

    fn dummy_path() -> &'static Path {
        Path::new("test.rs")
    }

    // ── extract_pyo3_name ────────────────────────────────────────────────────

    #[test]
    fn pyo3_name_from_pyo3_attr() {
        // #[pyo3(name = "foo")]
        let item: syn::ItemFn = syn::parse_quote! {
            #[pyfunction]
            #[pyo3(name = "foo")]
            fn rust_foo() -> i32 { 0 }
        };
        assert_eq!(extract_pyo3_name(&item.attrs), Some("foo".to_string()));
    }

    #[test]
    fn pyo3_name_from_pyclass_attr() {
        // #[pyclass(name = "MyClass")]
        let item: syn::ItemStruct = syn::parse_quote! {
            #[pyclass(name = "MyClass")]
            struct RustStruct {}
        };
        assert_eq!(extract_pyo3_name(&item.attrs), Some("MyClass".to_string()));
    }

    #[test]
    fn pyo3_name_absent_returns_none() {
        // No name override → None
        let item: syn::ItemFn = syn::parse_quote! {
            #[pyfunction]
            fn plain_fn() -> i32 { 0 }
        };
        assert_eq!(extract_pyo3_name(&item.attrs), None);
    }

    #[test]
    fn pyclass_name_with_extra_attrs() {
        // #[pyclass(name = "PdfDocument", unsendable)] — name must be extracted correctly
        let item: syn::ItemStruct = syn::parse_quote! {
            #[pyclass(name = "PdfDocument", unsendable)]
            pub struct PyPdfDocument { inner: i32 }
        };
        assert_eq!(
            extract_pyo3_name(&item.attrs),
            Some("PdfDocument".to_string())
        );
    }

    // ── build_type_alias_map (type alias → Python type resolution) ───────────

    #[test]
    fn build_type_alias_map_collects_type_alias() {
        let file = syn::parse_file(
            r#"
type PyBbox = (f32, f32, f32, f32);
"#,
        )
        .unwrap();
        let path = std::path::PathBuf::from("lib.rs");
        let files = vec![(path, file)];
        let map = build_type_alias_map(&files);
        assert!(
            map.contains_key("PyBbox"),
            "PyBbox alias should be collected"
        );
    }

    /// When a #[pyfunction] returns Vec<PyBbox> and the crate has `type PyBbox = (f32, f32, f32, f32)`,
    /// the return type is expanded and maps to list[tuple[float, float, float, float]].
    #[test]
    fn type_alias_expansion_in_return_produces_correct_python_type() {
        let file = syn::parse_file(
            r#"
type PyBbox = (f32, f32, f32, f32);

#[pymodule]
mod my_mod {
    #[pyfunction]
    fn find_all_cells_bboxes() -> Vec<PyBbox> { vec![] }
}
"#,
        )
        .unwrap();
        let path = Path::new("lib.rs");
        let config = Config::default();
        let pyclass_map = HashMap::new();
        let type_alias_map = build_type_alias_map(&[(path.to_path_buf(), file.clone())]);
        let modules =
            extract_modules_from_file(&file, path, &config, &pyclass_map, &type_alias_map);
        let func = match &modules[0].items[0] {
            PyItem::Function(f) => f,
            other => panic!("expected PyItem::Function, got {other:?}"),
        };
        let policy = RenderPolicy::from_version(3, 10);
        let mapping = crate::type_map::map_type(
            &func.return_type.rust_type,
            &policy,
            None,
            &Default::default(),
        );
        assert_eq!(
            mapping.py_type, "list[tuple[float, float, float, float]]",
            "Vec<PyBbox> with type PyBbox = (f32, f32, f32, f32) should map to list[tuple[float, float, float, float]]"
        );
    }

    /// When a #[pyfunction] parameter uses a type alias, it is expanded and maps correctly.
    #[test]
    fn type_alias_expansion_in_param_produces_correct_python_type() {
        let file = syn::parse_file(
            r#"
type Score = f64;

#[pymodule]
mod my_mod {
    #[pyfunction]
    fn rank(score: Score) -> bool { score > 0.5 }
}
"#,
        )
        .unwrap();
        let path = Path::new("lib.rs");
        let config = Config::default();
        let pyclass_map = HashMap::new();
        let type_alias_map = build_type_alias_map(&[(path.to_path_buf(), file.clone())]);
        let modules =
            extract_modules_from_file(&file, path, &config, &pyclass_map, &type_alias_map);
        let func = match &modules[0].items[0] {
            PyItem::Function(f) => f,
            other => panic!("expected PyItem::Function, got {other:?}"),
        };
        assert_eq!(func.params.len(), 1);
        let policy = RenderPolicy::from_version(3, 9);
        let mapping = crate::type_map::map_type(
            &func.params[0].ty.rust_type,
            &policy,
            None,
            &Default::default(),
        );
        assert_eq!(
            mapping.py_type, "float",
            "Score alias (= f64) should expand to float"
        );
    }

    // ── build_pyclass_name_map (style B: add_class uses this) ───────────────

    #[test]
    fn build_pyclass_name_map_includes_renamed_class() {
        let file: syn::File = syn::parse_quote! {
            #[pyclass(name = "PdfDocument", unsendable)]
            pub struct PyPdfDocument { inner: i32 }
        };
        let path = std::path::PathBuf::from("lib.rs");
        let files = vec![(path, file)];
        let map = build_pyclass_name_map(&files);
        assert_eq!(map.get("PyPdfDocument"), Some(&"PdfDocument".to_string()));
    }

    /// Style B: when a module uses m.add_class::<PyPdfDocument>() and the crate has
    /// #[pyclass(name = "PdfDocument")] on that struct, the generated class name must be PdfDocument.
    #[test]
    fn extract_modules_from_file_style_b_uses_pyclass_name_map() {
        let file = syn::parse_file(
            r#"
#[pymodule]
fn my_mod(m: &pyo3::Bound<'_, pyo3::PyModule>) -> pyo3::PyResult<()> {
    m.add_class::<PyPdfDocument>()?;
    Ok(())
}
"#,
        )
        .unwrap();
        let path = Path::new("lib.rs");
        let config = Config::default();
        let mut map = HashMap::new();
        map.insert("PyPdfDocument".to_string(), "PdfDocument".to_string());

        let type_alias_map = HashMap::new();
        let modules = extract_modules_from_file(&file, path, &config, &map, &type_alias_map);
        assert_eq!(modules.len(), 1, "one pymodule");
        let module = &modules[0];
        assert_eq!(module.items.len(), 1, "one item (the class)");
        match &module.items[0] {
            PyItem::Class(c) => assert_eq!(c.name, "PdfDocument", "class name from map"),
            other => panic!("expected PyItem::Class, got {:?}", other),
        }
    }

    /// Style B: when the map has no entry for the type, we fall back to the Rust type name.
    #[test]
    fn extract_modules_from_file_style_b_fallback_to_rust_name_when_not_in_map() {
        let file = syn::parse_file(
            r#"
#[pymodule]
fn my_mod(m: &pyo3::Bound<'_, pyo3::PyModule>) -> pyo3::PyResult<()> {
    m.add_class::<MyRustType>()?;
    Ok(())
}
"#,
        )
        .unwrap();
        let path = Path::new("lib.rs");
        let config = Config::default();
        let map = HashMap::new(); // empty map
        let type_alias_map = HashMap::new();

        let modules = extract_modules_from_file(&file, path, &config, &map, &type_alias_map);
        assert_eq!(modules.len(), 1);
        match &modules[0].items[0] {
            PyItem::Class(c) => assert_eq!(c.name, "MyRustType"),
            other => panic!("expected PyItem::Class, got {:?}", other),
        }
    }

    // ── parse_pyfunction respects #[pyo3(name = "...")] ─────────────────────

    #[test]
    fn pyfunction_uses_pyo3_name() {
        let item: syn::ItemFn = syn::parse_quote! {
            #[pyfunction]
            #[pyo3(name = "find_all_cells_bboxes")]
            fn py_find_all_cells_bboxes(a: usize) -> usize { a }
        };
        let config = Config::default();
        let func = parse_pyfunction(&item, dummy_path(), &config, &HashMap::new()).unwrap();
        assert_eq!(func.name, "find_all_cells_bboxes");
    }

    #[test]
    fn pyfunction_without_rename_uses_rust_name() {
        let item: syn::ItemFn = syn::parse_quote! {
            #[pyfunction]
            fn sum_as_string(a: usize, b: usize) -> String { String::new() }
        };
        let config = Config::default();
        let func = parse_pyfunction(&item, dummy_path(), &config, &HashMap::new()).unwrap();
        assert_eq!(func.name, "sum_as_string");
    }

    // ── pyclass name extraction ──────────────────────────────────────────────

    #[test]
    fn pyclass_struct_uses_pyclass_name_attr() {
        let item: syn::ItemStruct = syn::parse_quote! {
            #[pyclass(name = "Point")]
            struct RustPoint { x: f64, y: f64 }
        };
        let impl_map = ImplMap::default();
        let config = Config::default();
        let type_alias_map = HashMap::new();
        let name = extract_pyo3_name(&item.attrs).unwrap_or_else(|| item.ident.to_string());
        let rust_name = item.ident.to_string();
        let class = parse_pyclass_struct(
            &name,
            &rust_name,
            &item.attrs,
            dummy_path(),
            &impl_map,
            &config,
            &type_alias_map,
        );
        assert_eq!(class.name, "Point");
    }

    #[test]
    fn pyclass_struct_without_rename_uses_rust_ident() {
        let item: syn::ItemStruct = syn::parse_quote! {
            #[pyclass]
            struct MyType {}
        };
        let impl_map = ImplMap::default();
        let config = Config::default();
        let type_alias_map = HashMap::new();
        let name = extract_pyo3_name(&item.attrs).unwrap_or_else(|| item.ident.to_string());
        let rust_name = item.ident.to_string();
        let class = parse_pyclass_struct(
            &name,
            &rust_name,
            &item.attrs,
            dummy_path(),
            &impl_map,
            &config,
            &type_alias_map,
        );
        assert_eq!(class.name, "MyType");
    }

    // ── pyo3(signature) extraction still works alongside name ────────────────

    #[test]
    fn pyfunction_pyo3_name_and_signature_coexist() {
        // Single #[pyo3(name = "foo", signature = (a, b=0))]
        let item: syn::ItemFn = syn::parse_quote! {
            #[pyfunction]
            #[pyo3(name = "foo", signature = (a, b=0))]
            fn rust_foo(a: i64, b: i64) -> i64 { a + b }
        };
        let config = Config::default();
        let type_alias_map = HashMap::new();
        let func = parse_pyfunction(&item, dummy_path(), &config, &type_alias_map).unwrap();
        assert_eq!(func.name, "foo");
        assert!(func.signature_override.is_some());
    }

    // ── signature_override strips outer parens ───────────────────────────────

    #[test]
    fn signature_override_strips_outer_parens() {
        let item: syn::ItemFn = syn::parse_quote! {
            #[pyfunction]
            #[pyo3(signature = (page=None, clip=None, **kwargs))]
            fn py_find(page: Option<i32>, clip: Option<i32>) -> i32 { 0 }
        };
        let config = Config::default();
        let func = parse_pyfunction(&item, dummy_path(), &config, &HashMap::new()).unwrap();
        let sig = func.signature_override.unwrap();
        // Must NOT start with '(' or end with ')'
        assert!(
            !sig.starts_with('('),
            "signature should not start with '(', got: {sig}"
        );
        assert!(
            !sig.ends_with(')'),
            "signature should not end with ')', got: {sig}"
        );
        // Must contain the parameters
        assert!(
            sig.contains("page"),
            "signature should contain 'page', got: {sig}"
        );
        assert!(
            sig.contains("kwargs"),
            "signature should contain 'kwargs', got: {sig}"
        );
    }

    #[test]
    fn signature_override_separate_attr_strips_outer_parens() {
        // Two separate attributes: #[pyo3(name = "...")] + #[pyo3(signature = (...))]
        let item: syn::ItemFn = syn::parse_quote! {
            #[pyfunction]
            #[pyo3(name = "find_all_cells_bboxes", signature = (page=None, clip=None, tf_settings=None, **kwargs))]
            fn py_find_all_cells_bboxes(page: Option<i32>, clip: Option<i32>) -> i32 { 0 }
        };
        let config = Config::default();
        let func = parse_pyfunction(&item, dummy_path(), &config, &HashMap::new()).unwrap();
        assert_eq!(func.name, "find_all_cells_bboxes");
        let sig = func.signature_override.unwrap();
        assert!(!sig.starts_with('('), "got: {sig}");
        assert!(!sig.ends_with(')'), "got: {sig}");
    }

    /// Rust `true`/`false` defaults in a pyo3 signature are rewritten to Python `True`/`False`.
    /// Identifiers that merely contain the word (e.g. `extract_text`) must not be modified.
    #[test]
    fn signature_override_normalizes_rust_bools_to_python() {
        let item: syn::ItemFn = syn::parse_quote! {
            #[pyfunction]
            #[pyo3(signature = (page = None, extract_text = true, verbose = false, clip = None, **kwargs))]
            fn find_tables(
                page: Option<i32>,
                extract_text: bool,
                verbose: bool,
                clip: Option<i32>,
            ) -> Vec<i32> { vec![] }
        };
        let config = Config::default();
        let func = parse_pyfunction(&item, dummy_path(), &config, &HashMap::new()).unwrap();
        let sig = func.signature_override.unwrap();
        assert!(sig.contains("True"), "expected Python True, got: {sig}");
        assert!(
            !sig.contains(" = true"),
            "Rust `true` should have been replaced, got: {sig}"
        );
        assert!(sig.contains("False"), "expected Python False, got: {sig}");
        assert!(
            !sig.contains(" = false"),
            "Rust `false` should have been replaced, got: {sig}"
        );
        assert!(
            sig.contains("extract_text"),
            "identifier 'extract_text' must not be mangled, got: {sig}"
        );
    }

    /// Only the single outer pair of parentheses is stripped; nested parens are preserved.
    #[test]
    fn signature_override_nested_parens_strips_only_outer() {
        let item: syn::ItemFn = syn::parse_quote! {
            #[pyfunction]
            #[pyo3(signature = (a, (b, c)))]
            fn f(a: i64, b: i64, c: i64) -> i64 { 0 }
        };
        let config = Config::default();
        let func = parse_pyfunction(&item, dummy_path(), &config, &HashMap::new()).unwrap();
        let sig = func.signature_override.unwrap();
        // We strip only the single outer pair; result may still end with ')' if nested
        assert!(
            !sig.starts_with('('),
            "outer open paren should be stripped: {sig}"
        );
        assert!(
            sig.contains('b') && sig.contains('c'),
            "inner content (b, c) should be preserved: {sig}"
        );
    }

    // ── plain instance methods in #[pymethods] are collected ─────────────────

    /// Regular methods in a #[pymethods] block (no special attribute) must be collected.
    /// Previously only methods with #[new]/#[getter]/etc. were emitted; all others were dropped.
    #[test]
    fn pymethods_plain_instance_methods_are_collected() {
        let file = syn::parse_file(
            r#"
#[pyclass]
struct Counter {}

#[pymethods]
impl Counter {
    fn count(&self) -> i32 { 0 }
    fn reset(&mut self) {}
}

#[pymodule]
fn my_mod(m: &pyo3::Bound<'_, pyo3::PyModule>) -> pyo3::PyResult<()> {
    m.add_class::<Counter>()?;
    Ok(())
}
"#,
        )
        .unwrap();

        let config = Config::default();
        let map = HashMap::new();
        let modules =
            extract_modules_from_file(&file, Path::new("lib.rs"), &config, &map, &HashMap::new());
        let class = match &modules[0].items[0] {
            PyItem::Class(c) => c,
            other => panic!("expected PyItem::Class, got {other:?}"),
        };
        let names: Vec<&str> = class.methods.iter().map(|m| m.name.as_str()).collect();
        assert!(names.contains(&"count"), "count missing: {names:?}");
        assert!(names.contains(&"reset"), "reset missing: {names:?}");
    }

    /// __iter__ and __next__ are plain instance methods in pyo3 (no special attribute needed).
    /// They must appear in the collected method list.
    #[test]
    fn pymethods_dunder_iter_and_next_are_collected() {
        let file = syn::parse_file(
            r#"
#[pyclass]
struct PageIterator {}

#[pymethods]
impl PageIterator {
    fn __iter__(slf: pyo3::PyRef<'_, Self>) -> pyo3::PyRef<'_, Self> { slf }
    fn __next__(&mut self) -> Option<i32> { None }
}

#[pymodule]
fn my_mod(m: &pyo3::Bound<'_, pyo3::PyModule>) -> pyo3::PyResult<()> {
    m.add_class::<PageIterator>()?;
    Ok(())
}
"#,
        )
        .unwrap();

        let config = Config::default();
        let map = HashMap::new();
        let modules =
            extract_modules_from_file(&file, Path::new("lib.rs"), &config, &map, &HashMap::new());
        let class = match &modules[0].items[0] {
            PyItem::Class(c) => c,
            other => panic!("expected PyItem::Class, got {other:?}"),
        };
        let names: Vec<&str> = class.methods.iter().map(|m| m.name.as_str()).collect();
        assert!(names.contains(&"__iter__"), "__iter__ missing: {names:?}");
        assert!(names.contains(&"__next__"), "__next__ missing: {names:?}");
    }

    // ── PyRef / PyRefMut self-params are excluded from stubs ─────────────────

    /// `slf: PyRef<'_, Self>` is pyo3's way to write `&self`; it must not appear
    /// as an explicit Python parameter in the generated stub.
    #[test]
    fn pyref_self_param_is_excluded() {
        let file = syn::parse_file(
            r#"
#[pyclass]
struct MyIter {}

#[pymethods]
impl MyIter {
    fn __iter__(slf: pyo3::PyRef<'_, Self>) -> pyo3::PyRef<'_, Self> { slf }
}

#[pymodule]
fn my_mod(m: &pyo3::Bound<'_, pyo3::PyModule>) -> pyo3::PyResult<()> {
    m.add_class::<MyIter>()?;
    Ok(())
}
"#,
        )
        .unwrap();

        let config = Config::default();
        let map = HashMap::new();
        let modules =
            extract_modules_from_file(&file, Path::new("lib.rs"), &config, &map, &HashMap::new());
        let class = match &modules[0].items[0] {
            PyItem::Class(c) => c,
            other => panic!("expected PyItem::Class, got {other:?}"),
        };
        let iter_m = class
            .methods
            .iter()
            .find(|m| m.name == "__iter__")
            .expect("__iter__ not found");
        let param_names: Vec<&str> = iter_m.params.iter().map(|p| p.name.as_str()).collect();
        assert!(
            param_names.is_empty(),
            "PyRef<Self> should not appear as a param, got: {param_names:?}"
        );
    }

    /// `mut slf: PyRefMut<'_, Self>` is pyo3's way to write `&mut self`; same rule.
    #[test]
    fn pyrefmut_self_param_is_excluded() {
        let file = syn::parse_file(
            r#"
#[pyclass]
struct MyIter {}

#[pymethods]
impl MyIter {
    fn __next__(mut slf: pyo3::PyRefMut<'_, Self>) -> Option<i32> { None }
}

#[pymodule]
fn my_mod(m: &pyo3::Bound<'_, pyo3::PyModule>) -> pyo3::PyResult<()> {
    m.add_class::<MyIter>()?;
    Ok(())
}
"#,
        )
        .unwrap();

        let config = Config::default();
        let map = HashMap::new();
        let modules =
            extract_modules_from_file(&file, Path::new("lib.rs"), &config, &map, &HashMap::new());
        let class = match &modules[0].items[0] {
            PyItem::Class(c) => c,
            other => panic!("expected PyItem::Class, got {other:?}"),
        };
        let next_m = class
            .methods
            .iter()
            .find(|m| m.name == "__next__")
            .expect("__next__ not found");
        let param_names: Vec<&str> = next_m.params.iter().map(|p| p.name.as_str()).collect();
        assert!(
            param_names.is_empty(),
            "PyRefMut<Self> should not appear as a param, got: {param_names:?}"
        );
    }

    /// PyRef self-param is excluded even when the method has additional real params.
    #[test]
    fn pyref_self_excluded_with_extra_params() {
        let file = syn::parse_file(
            r#"
#[pyclass]
struct Foo {}

#[pymethods]
impl Foo {
    fn get(slf: pyo3::PyRef<'_, Self>, index: i32) -> i32 { index }
}

#[pymodule]
fn my_mod(m: &pyo3::Bound<'_, pyo3::PyModule>) -> pyo3::PyResult<()> {
    m.add_class::<Foo>()?;
    Ok(())
}
"#,
        )
        .unwrap();

        let config = Config::default();
        let map = HashMap::new();
        let modules =
            extract_modules_from_file(&file, Path::new("lib.rs"), &config, &map, &HashMap::new());
        let class = match &modules[0].items[0] {
            PyItem::Class(c) => c,
            other => panic!("expected PyItem::Class, got {other:?}"),
        };
        let get_m = class
            .methods
            .iter()
            .find(|m| m.name == "get")
            .expect("get not found");
        let param_names: Vec<&str> = get_m.params.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(
            param_names,
            vec!["index"],
            "only 'index' should remain, got: {param_names:?}"
        );
    }

    // ── &Bound<'_, PyBytes> etc. are real params; &Bound<'_, PyModule> is injected ─────

    /// A #[pyfunction] with `data: &Bound<'_, PyBytes>` must keep the param and map it to Python `bytes`.
    #[test]
    fn bound_pybytes_param_kept_and_maps_to_bytes() {
        let file = syn::parse_file(
            r#"
#[pymodule]
mod my_mod {
    #[pyfunction]
    fn bytes_len(data: &pyo3::Bound<'_, pyo3::types::PyBytes>) -> usize { data.as_bytes().len() }
}
"#,
        )
        .unwrap();
        let path = Path::new("lib.rs");
        let config = Config::default();
        let type_alias_map = HashMap::new();
        let pyclass_map = HashMap::new();
        let modules =
            extract_modules_from_file(&file, path, &config, &pyclass_map, &type_alias_map);
        let func = match &modules[0].items[0] {
            PyItem::Function(f) => f,
            other => panic!("expected PyItem::Function, got {other:?}"),
        };
        assert_eq!(
            func.params.len(),
            1,
            "&Bound<PyBytes> must not be filtered as injected"
        );
        assert_eq!(func.params[0].name, "data");
        let policy = RenderPolicy::from_version(3, 9);
        let mapping = crate::type_map::map_type(
            &func.params[0].ty.rust_type,
            &policy,
            None,
            &Default::default(),
        );
        assert_eq!(
            mapping.py_type, "bytes",
            "PyBytes should map to Python bytes"
        );
    }

    /// A `#[staticmethod]` returning `PyResult<Self>` should have its return type collected
    /// as the raw `Self` type, which the generator later resolves to the Python class name.
    #[test]
    fn static_method_pyresult_self_return_type_is_collected() {
        let file = syn::parse_file(
            r#"
#[pyclass(name = "PdfDocument")]
struct PyPdfDocument {}

#[pymethods]
impl PyPdfDocument {
    #[staticmethod]
    fn from_bytes(data: &[u8]) -> PyResult<Self> {
        unimplemented!()
    }
}

#[pymodule]
fn pdf_oxide(m: &pyo3::Bound<'_, pyo3::PyModule>) -> pyo3::PyResult<()> {
    m.add_class::<PyPdfDocument>()?;
    Ok(())
}
"#,
        )
        .unwrap();
        let path = Path::new("lib.rs");
        let config = Config::default();
        let mut pyclass_map = HashMap::new();
        pyclass_map.insert("PyPdfDocument".to_string(), "PdfDocument".to_string());
        let type_alias_map = HashMap::new();
        let modules =
            extract_modules_from_file(&file, path, &config, &pyclass_map, &type_alias_map);
        let class = match &modules[0].items[0] {
            PyItem::Class(c) => c,
            other => panic!("expected PyItem::Class, got {other:?}"),
        };
        let from_bytes = class
            .methods
            .iter()
            .find(|m| m.name == "from_bytes")
            .expect("from_bytes not found");
        // The return type should be `PyResult<Self>` in the raw Rust AST.
        // When the generator resolves it with self_type = "PdfDocument", it should
        // unwrap PyResult and resolve Self → PdfDocument.
        let policy = RenderPolicy::from_version(3, 9);
        let mapping = crate::type_map::map_type(
            &from_bytes.return_type.rust_type,
            &policy,
            Some("PdfDocument"),
            &Default::default(),
        );
        assert_eq!(
            mapping.py_type, "PdfDocument",
            "PyResult<Self> with class context should resolve to PdfDocument"
        );
    }

    /// A #[pyfunction] with `m: &Bound<'_, PyModule>` must exclude that param (injected by pyo3).
    #[test]
    fn bound_pymodule_param_excluded() {
        let file = syn::parse_file(
            r#"
#[pymodule]
mod my_mod {
    #[pyfunction]
    fn needs_module(m: &pyo3::Bound<'_, pyo3::PyModule>, x: i64) -> i64 { x }
}
"#,
        )
        .unwrap();
        let path = Path::new("lib.rs");
        let config = Config::default();
        let type_alias_map = HashMap::new();
        let pyclass_map = HashMap::new();
        let modules =
            extract_modules_from_file(&file, path, &config, &pyclass_map, &type_alias_map);
        let func = match &modules[0].items[0] {
            PyItem::Function(f) => f,
            other => panic!("expected PyItem::Function, got {other:?}"),
        };
        assert_eq!(
            func.params.len(),
            1,
            "only x should remain; m (Bound<PyModule>) is injected"
        );
        assert_eq!(func.params[0].name, "x");
    }
}
