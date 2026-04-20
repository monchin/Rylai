use super::model::*;
use crate::config::Config;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use syn::{
    Attribute, Expr, Fields, FnArg, ImplItem, Item, ItemFn, ItemMod, Lit, LitStr, Meta, Pat,
    ReturnType, Signature, Type, TypePath,
};

/// Maximum recursion depth when expanding type aliases to avoid infinite loops.
/// Aliases nested deeper than this value are returned unexpanded.
const MAX_TYPE_ALIAS_DEPTH: u8 = 10;

// ── Utility functions ──────────────────────────────────────────────────────────

/// Runs `collector` over every file's top-level items.
fn build_map_from_files<M, F>(
    files: &[(std::path::PathBuf, syn::File)],
    enabled_features: &[String],
    default: M,
    mut collector: F,
) -> M
where
    M: Default,
    F: FnMut(&[Item], &mut M, &[String]),
{
    let mut map = default;
    for (_path, file) in files {
        collector(&file.items, &mut map, enabled_features);
    }
    map
}

/// Like [`build_map_from_files`] but passes each file path into `collector`.
fn build_map_from_files_with_ctx<M, F>(
    files: &[(std::path::PathBuf, syn::File)],
    enabled_features: &[String],
    default: M,
    mut collector: F,
) -> M
where
    M: Default,
    F: FnMut(&[Item], &Path, &mut M, &[String]),
{
    let mut map = default;
    for (path, file) in files {
        collector(&file.items, path, &mut map, enabled_features);
    }
    map
}

/// Attributes used for `#[cfg(...)]` filtering while walking the item tree.
/// Items without a dedicated attribute list here are skipped (same as before: no nested walk).
fn item_attrs_for_cfg_walk(item: &Item) -> Option<&[Attribute]> {
    match item {
        Item::Type(ta) => Some(&ta.attrs),
        Item::Struct(s) => Some(&s.attrs),
        Item::Enum(e) => Some(&e.attrs),
        Item::Mod(m) => Some(&m.attrs),
        Item::Fn(f) => Some(&f.attrs),
        Item::Impl(imp) => Some(&imp.attrs),
        _ => None,
    }
}

/// Walks nested modules; respects `#[cfg]` via [`cfg_is_active`]. Callback receives only cfg-active items.
fn walk_items_with_cfg<F>(items: &[Item], enabled_features: &[String], callback: &mut F)
where
    F: FnMut(&Item),
{
    for item in items {
        let Some(attrs) = item_attrs_for_cfg_walk(item) else {
            continue;
        };

        if !cfg_is_active(attrs, enabled_features) {
            continue;
        }

        callback(item);

        if let Item::Mod(m) = item
            && let Some((_, content)) = &m.content
        {
            walk_items_with_cfg(content, enabled_features, callback);
        }
    }
}

/// [`walk_items_with_cfg`] with extra context per callback (e.g. file path).
fn walk_items_with_ctx<C: ?Sized, F>(
    items: &[Item],
    ctx: &C,
    enabled_features: &[String],
    callback: &mut F,
) where
    F: FnMut(&Item, &C),
{
    for item in items {
        let Some(attrs) = item_attrs_for_cfg_walk(item) else {
            continue;
        };

        if !cfg_is_active(attrs, enabled_features) {
            continue;
        }

        callback(item, ctx);

        if let Item::Mod(m) = item
            && let Some((_, content)) = &m.content
        {
            walk_items_with_ctx(content, ctx, enabled_features, callback);
        }
    }
}

// ── Entry point ──────────────────────────────────────────────────────────────

/// Build a map: Rust type alias name -> underlying `syn::Type` from all `type Foo = ...` in the crate.
/// Used to resolve e.g. `PyBbox` to `(f32, f32, f32, f32)` when mapping to Python types.
/// If the same alias name appears in multiple files, the last occurrence wins.
/// Items behind `#[cfg(feature = "...")]` are included only when the feature is in `enabled_features`.
pub fn build_type_alias_map(
    files: &[(std::path::PathBuf, syn::File)],
    enabled_features: &[String],
) -> HashMap<String, Type> {
    build_map_from_files(
        files,
        enabled_features,
        HashMap::new(),
        collect_type_aliases_from_items,
    )
}

fn collect_type_aliases_from_items(
    items: &[Item],
    map: &mut HashMap<String, Type>,
    enabled_features: &[String],
) {
    walk_items_with_cfg(items, enabled_features, &mut |item| {
        if let Item::Type(ta) = item {
            map.insert(ta.ident.to_string(), (*ta.ty).clone());
        }
    });
}

/// Recursively expand type aliases in `ty` using `map`. Stops at depth limit to avoid cycles.
///
/// When `preserve_alias_idents` contains the alias ident, expansion is skipped so the name is
/// preserved for [`crate::type_map::map_type`] (via merged `known_classes`) — e.g. `Vec<PyBbox>`
/// → `list[BBox]` instead of losing `PyBbox` after expanding to a tuple. The set is built from
/// [`crate::config::type_map_preserve_alias_idents`] so ambiguous `[type_map]` keys do not preserve
/// the wrong alias.
fn expand_type_aliases(
    ty: &Type,
    map: &HashMap<String, Type>,
    preserve_alias_idents: &HashSet<String>,
    depth: u8,
) -> Type {
    if depth >= MAX_TYPE_ALIAS_DEPTH {
        return ty.clone();
    }
    match ty {
        Type::Path(tp) if is_single_ident_path(tp) => {
            let name = tp.path.segments.last().unwrap().ident.to_string();
            if let Some(underlying) = map.get(&name) {
                if preserve_alias_idents.contains(&name) {
                    return ty.clone();
                }
                return expand_type_aliases(underlying, map, preserve_alias_idents, depth + 1);
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
                        *t = expand_type_aliases(t, map, preserve_alias_idents, depth + 1);
                    }
                }
            }
            Type::Path(new_tp)
        }
        Type::Tuple(t) => {
            let mut elems = syn::punctuated::Punctuated::new();
            for pair in t.elems.pairs() {
                elems.push_value(expand_type_aliases(
                    pair.value(),
                    map,
                    preserve_alias_idents,
                    depth + 1,
                ));
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
            let elem = expand_type_aliases(&r.elem, map, preserve_alias_idents, depth + 1);
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
/// Items behind `#[cfg(feature = "...")]` are included only when the feature is in `enabled_features`.
pub fn build_pyclass_name_map(
    files: &[(std::path::PathBuf, syn::File)],
    enabled_features: &[String],
) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for (_path, file) in files {
        collect_pyclass_names_from_items(&file.items, &mut map, enabled_features);
    }
    map
}

fn collect_pyclass_names_from_items(
    items: &[Item],
    map: &mut HashMap<String, String>,
    enabled_features: &[String],
) {
    for item in items {
        match item {
            Item::Struct(s) if has_attr(&s.attrs, "pyclass") => {
                if cfg_is_active(&s.attrs, enabled_features)
                    && let Some(python_name) = extract_pyo3_name(&s.attrs)
                {
                    map.insert(s.ident.to_string(), python_name);
                }
            }
            Item::Enum(e) if has_attr(&e.attrs, "pyclass") => {
                if cfg_is_active(&e.attrs, enabled_features)
                    && let Some(python_name) = extract_pyo3_name(&e.attrs)
                {
                    map.insert(e.ident.to_string(), python_name);
                }
            }
            Item::Mod(m) => {
                if cfg_is_active(&m.attrs, enabled_features)
                    && let Some((_, content)) = &m.content
                {
                    collect_pyclass_names_from_items(content, map, enabled_features);
                }
            }
            _ => {}
        }
    }
}

/// Shared "environment" threaded through all parsing functions: the four global
/// maps / config that every function in the parse pipeline requires.
///
/// Grouping them avoids the `clippy::too_many_arguments` lint and makes it easy
/// to add future per-crate configuration without touching every call site.
#[derive(Copy, Clone)]
pub(crate) struct ParseContext<'a> {
    pub config: &'a Config,
    pub impl_map: &'a ImplMap,
    pub struct_fields_map: &'a StructFieldsMap,
    pub type_alias_map: &'a HashMap<String, Type>,
    /// Attributes of each `#[pyclass]` struct/enum by Rust type name. Used in Style B to restore docstrings.
    pub pyclass_attrs_map: &'a PyclassAttrsMap,
    /// Rust type names of `#[pyclass]` enums in the crate, from [`build_pyclass_enum_rust_names`].
    ///
    /// **Style B:** `m.add_class::<T>()` does not say whether `T` is a struct or enum. Production
    /// entry points must pass [`Some`] so enum items get correct stub semantics (`@t.final`, no
    /// `extends`). Unit tests may pass [`None`] to simulate struct-only call sites.
    pub pyclass_enum_rust_names: Option<&'a PyclassEnumRustNames>,
    /// Optional sink for parse-time warnings (e.g. invalid `rename_all` literals).
    pub parse_warnings: Option<&'a RefCell<Vec<String>>>,
    /// From [`crate::config::type_map_preserve_alias_idents`]; used when expanding `type` aliases.
    pub type_map_preserve_idents: &'a HashSet<String>,
    /// Global map of all pyfunctions in the crate, indexed by function name (last segment of path).
    /// Allows resolving `wrap_pyfunction!(crate::module::foo)` calls.
    pub pyfunction_map: Option<&'a PyFunctionMap>,
}

/// Context for walking `m.add` / `m.add_function` / `m.add_class` in Style B pymodule functions and in
/// declarative `#[pymodule_init]`.
struct CollectAddCallsContext<'a> {
    file_items: &'a [Item],
    path: &'a Path,
    pyclass_name_map: &'a HashMap<String, String>,
    cx: ParseContext<'a>,
}

/// Extract all `#[pymodule]` items from a parsed file.
///
/// `impl_map` (inside `cx`) must be the **global** map built by [`build_impl_map`] across the
/// entire crate so that `#[pymethods]` blocks in other files are resolved.
///
/// `struct_fields_map` (inside `cx`) must be the **global** map built by
/// [`build_struct_fields_map`] so that `#[pyo3(get)]` / `#[pyo3(set)]` fields from other
/// files generate properties.
pub fn extract_modules_from_file(
    file: &syn::File,
    path: &Path,
    pyclass_name_map: &HashMap<String, String>,
    cx: ParseContext<'_>,
) -> Vec<PyModule> {
    // Collect modules
    let mut result = Vec::new();
    for item in &file.items {
        match item {
            // Style A: #[pymodule] mod Foo { ... }
            Item::Mod(m) if has_attr(&m.attrs, "pymodule") => {
                if cfg_is_active(&m.attrs, &cx.config.features.enabled)
                    && let Some(module) = parse_mod_style_module(m, path, pyclass_name_map, cx)
                {
                    result.push(module);
                }
            }
            // Style B: #[pymodule] fn foo(m: &Bound<PyModule>) -> PyResult<()> { ... }
            Item::Fn(f) if has_attr(&f.attrs, "pymodule") => {
                if cfg_is_active(&f.attrs, &cx.config.features.enabled)
                    && let Some(module) =
                        parse_fn_style_module(f, &file.items, path, pyclass_name_map, cx)
                {
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
    pyclass_name_map: &HashMap<String, String>,
    cx: ParseContext<'_>,
) -> Option<PyModule> {
    let (_, items) = m.content.as_ref()?;
    let name = m.ident.to_string();
    let doc = extract_doc(&m.attrs);

    let py_items = collect_items_from_list(items, path, pyclass_name_map, cx);

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
    pyclass_name_map: &HashMap<String, String>,
    cx: ParseContext<'_>,
) -> Vec<PyItem> {
    let enabled = &cx.config.features.enabled;
    let mut result = Vec::new();
    for item in items {
        match item {
            Item::Fn(f) if has_attr(&f.attrs, "pyfunction") => {
                if cfg_is_active(&f.attrs, enabled)
                    && let Some(func) = parse_pyfunction(
                        f,
                        path,
                        cx.config,
                        cx.type_alias_map,
                        cx.type_map_preserve_idents,
                    )
                {
                    result.push(PyItem::Function(func));
                }
            }
            Item::Struct(s)
                if has_attr(&s.attrs, "pyclass") && cfg_is_active(&s.attrs, enabled) =>
            {
                let name = extract_pyo3_name(&s.attrs).unwrap_or_else(|| s.ident.to_string());
                let rust_name = s.ident.to_string();
                let class = parse_pyclass_struct(&name, &rust_name, &s.attrs, path, cx, false);
                result.push(PyItem::Class(class));
            }
            Item::Enum(e) if has_attr(&e.attrs, "pyclass") && cfg_is_active(&e.attrs, enabled) => {
                let name = extract_pyo3_name(&e.attrs).unwrap_or_else(|| e.ident.to_string());
                let rust_name = e.ident.to_string();
                let class = parse_pyclass_struct(&name, &rust_name, &e.attrs, path, cx, true);
                result.push(PyItem::Class(class));
            }
            // Nested submodule
            Item::Mod(sub) if has_attr(&sub.attrs, "pymodule") => {
                if cfg_is_active(&sub.attrs, enabled)
                    && let Some(sub_mod) = parse_mod_style_module(sub, path, pyclass_name_map, cx)
                {
                    result.push(PyItem::Module(sub_mod));
                }
            }
            // Declarative `#[pymodule] mod foo { #[pymodule_init] fn init(m) { m.add(...); } }`
            Item::Fn(f)
                if has_attr(&f.attrs, "pymodule_init") && cfg_is_active(&f.attrs, enabled) =>
            {
                let ctx = CollectAddCallsContext {
                    file_items: items,
                    path,
                    pyclass_name_map,
                    cx,
                };
                for stmt in &f.block.stmts {
                    collect_add_calls_from_stmt(stmt, &ctx, &mut result);
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
    pyclass_name_map: &HashMap<String, String>,
    cx: ParseContext<'_>,
) -> Option<PyModule> {
    let name = f.sig.ident.to_string();
    let doc = extract_doc(&f.attrs);

    let mut py_items = Vec::new();

    // Walk the function body looking for m.add_function(...) / m.add_class::<T>()
    let ctx = CollectAddCallsContext {
        file_items,
        path,
        pyclass_name_map,
        cx,
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
        Expr::Block(b) => {
            for stmt in &b.block.stmts {
                collect_add_calls_from_stmt(stmt, ctx, out);
            }
        }
        Expr::MethodCall(mc) => {
            let method = mc.method.to_string();
            match method.as_str() {
                "add_function" => {
                    // m.add_function(wrap_pyfunction!(foo, m)?)
                    if let Some(fn_name) = extract_wrap_pyfunction_name(mc.args.first())
                        && let Some(func) = find_pyfunction_by_name(&fn_name, ctx)
                    {
                        out.push(PyItem::Function(func));
                    }
                }
                "add_class" => {
                    // m.add_class::<PyPdfDocument>() — use pyclass_name_map for #[pyclass(name = "PdfDocument")]
                    // Style B: look up struct/enum attrs (e.g. doc comments) from pyclass_attrs_map.
                    if let Some(type_name) = extract_turbofish_type_name(&mc.method, &mc.turbofish)
                    {
                        let rust_name = type_name.clone();
                        let class_name = ctx
                            .pyclass_name_map
                            .get(&type_name)
                            .cloned()
                            .unwrap_or(type_name);
                        let attrs: &[Attribute] = ctx
                            .cx
                            .pyclass_attrs_map
                            .get(&rust_name)
                            .map(Vec::as_slice)
                            .unwrap_or(&[]);
                        let is_enum = ctx
                            .cx
                            .pyclass_enum_rust_names
                            .map(|s| s.contains(rust_name.as_str()))
                            .unwrap_or(false);
                        let class = parse_pyclass_struct(
                            &class_name,
                            &rust_name,
                            attrs,
                            ctx.path,
                            ctx.cx,
                            is_enum,
                        );
                        out.push(PyItem::Class(class));
                    }
                }
                "add" => {
                    // m.add("__version__", env!("CARGO_PKG_VERSION")) — module-level constant
                    let mut args = mc.args.iter();
                    if let Some((name, py_type)) =
                        extract_add_constant_name_and_type(args.next(), args.next())
                    {
                        out.push(PyItem::Constant(PyConstant { name, py_type }));
                    }
                }
                _ => {}
            }
        }
        _ => {}
    }
}

/// Extract the function name from `wrap_pyfunction!(foo, m)` or `wrap_pyfunction!(foo)`.
/// Supports both simple names (`foo`) and full paths (`crate::module::foo`).
fn extract_wrap_pyfunction_name(arg: Option<&Expr>) -> Option<String> {
    let expr = arg?;
    // Also handle if it was already unwrapped (after `?`)
    if let Expr::Try(t) = expr {
        return extract_wrap_pyfunction_name(Some(&t.expr));
    }
    // The macro call becomes a syn::ExprMacro
    if let Expr::Macro(m) = expr {
        let macro_name = m.mac.path.segments.last()?.ident.to_string();
        if macro_name == "wrap_pyfunction" {
            // Tokens: `foo , m` or `crate::module::foo , m`
            // Collect all identifiers until we hit a comma
            let mut path_parts = Vec::new();
            for token in m.mac.tokens.clone() {
                match token {
                    proc_macro2::TokenTree::Ident(id) => {
                        path_parts.push(id.to_string());
                    }
                    proc_macro2::TokenTree::Punct(p) if p.as_char() == ',' => {
                        // Stop at comma (argument separator)
                        break;
                    }
                    // Ignore other tokens (:: punctuation and whitespace)
                    _ => {}
                }
            }
            // Validate that we have at least one identifier part
            if !path_parts.is_empty() {
                // Ensure all path parts are valid Rust identifiers
                // Identifiers must start with a letter or underscore, and contain only letters,
                // digits, and underscores. The syn crate already validates this when parsing
                // Ident tokens, so we just need to ensure we collected at least one.
                return Some(path_parts.join("::"));
            }
        }
    }
    None
}

/// Extract name and Python type from `m.add("name", value)` for module-level constants.
/// - First arg must be a string literal (e.g. `"__version__"`).
/// - Second arg type: `env!(...)` → str, literal → corresponding type, else str.
fn extract_add_constant_name_and_type(
    first: Option<&Expr>,
    second: Option<&Expr>,
) -> Option<(String, String)> {
    let name = match first? {
        Expr::Lit(expr_lit) => match &expr_lit.lit {
            Lit::Str(s) => s.value(),
            _ => return None,
        },
        _ => return None,
    };
    let expr = second?;
    let py_type = infer_constant_value_type(expr);
    Some((name, py_type))
}

/// Infers the Python type string for a constant value expression.
/// - `env!(...)` → `"str"`.
/// - Literals (str, int, float, bool) → corresponding type.
/// - Any other macro (e.g. `concat!(...)`) or expression → `"str"` as a safe default.
fn infer_constant_value_type(expr: &Expr) -> String {
    match expr {
        // env!("CARGO_PKG_VERSION") etc. → str
        Expr::Macro(m) => {
            let seg = m.mac.path.segments.last();
            if seg.map(|s| s.ident == "env") == Some(true) {
                return "str".to_string();
            }
            "str".to_string()
        }
        Expr::Lit(expr_lit) => match &expr_lit.lit {
            Lit::Str(_) | Lit::ByteStr(_) | Lit::Char(_) => "str".to_string(),
            Lit::Int(_) | Lit::Byte(_) => "int".to_string(),
            Lit::Float(_) => "float".to_string(),
            Lit::Bool(_) => "bool".to_string(),
            _ => "str".to_string(),
        },
        _ => "str".to_string(),
    }
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
fn find_pyfunction_by_name<'a>(name: &str, ctx: &CollectAddCallsContext<'a>) -> Option<PyFunction> {
    // Extract the last segment (function name) from a potential path like "crate::module::foo"
    // Note: This approach has limitations. If multiple modules have functions with the same name,
    // we may resolve to the wrong function. A full semantic analysis would be needed for
    // precise resolution, but that's beyond AST-level parsing capabilities.
    let lookup_name = name.rsplit("::").next().unwrap_or(name);

    // First try to find in global pyfunction map (for cross-module references)
    if let Some(map) = ctx.cx.pyfunction_map
        && let Some(func) = map.get(lookup_name)
    {
        return Some(func.clone());
    }

    // Fallback to searching in current file
    for item in ctx.file_items {
        if let Item::Fn(f) = item
            && f.sig.ident == lookup_name
            && has_attr(&f.attrs, "pyfunction")
        {
            return parse_pyfunction(
                f,
                ctx.path,
                ctx.cx.config,
                ctx.cx.type_alias_map,
                ctx.cx.type_map_preserve_idents,
            );
        }
    }

    // If not found and we have a global map, this might be an alias case
    // (e.g., `use foo as bar` and `wrap_pyfunction!(bar)`)
    // or a full path reference that couldn't be resolved.
    // Unfortunately, AST-level parsing cannot reliably resolve aliases without full
    // semantic analysis. Users should use the original function name or full path.
    if let Some(warnings) = ctx.cx.parse_warnings {
        if name.contains("::") {
            warnings.borrow_mut().push(format!(
                "Could not resolve function '{}'. This may be due to aliasing or cross-module reference limitations. Try using the original function name.",
                name
            ));
        } else if ctx.cx.pyfunction_map.is_some() {
            warnings.borrow_mut().push(format!(
                "Could not find pyfunction '{}'. Ensure the function has #[pyfunction] attribute and is accessible.",
                lookup_name
            ));
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
    type_map_preserve_idents: &HashSet<String>,
) -> Option<PyFunction> {
    // #[pyo3(name = "foo")] overrides the Python-exposed name; keep Rust ident for [[override]] keys.
    let rust_name = f.sig.ident.to_string();
    let name = extract_pyo3_name(&f.attrs).unwrap_or_else(|| rust_name.clone());
    let doc = extract_doc(&f.attrs);
    let signature_override = extract_pyo3_signature(&f.attrs);
    let params = parse_params(&f.sig, config, type_alias_map, type_map_preserve_idents);
    let return_type = parse_return_type(
        &f.sig.output,
        config,
        type_alias_map,
        type_map_preserve_idents,
    );

    Some(PyFunction {
        name,
        rust_name,
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
    type_map_preserve_idents: &HashSet<String>,
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
                let rust_type =
                    expand_type_aliases(&pt.ty, type_alias_map, type_map_preserve_idents, 0);
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
    type_map_preserve_idents: &HashSet<String>,
) -> PyType {
    match output {
        ReturnType::Default => PyType {
            rust_type: syn::parse_quote! { () },
            override_str: None,
        },
        ReturnType::Type(_, ty) => {
            let rust_type = expand_type_aliases(ty, type_alias_map, type_map_preserve_idents, 0);
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

/// Returns `(has_get, has_set)` by inspecting `#[pyo3(get)]`, `#[pyo3(set)]`,
/// or `#[pyo3(get, set)]` on a struct field.
///
/// # Assumption — single combined attribute
///
/// This function returns on the **first** `#[pyo3(...)]` attribute it finds.
/// pyo3 convention (and the macro's own documentation) places all field modifiers
/// in a single attribute, e.g. `#[pyo3(get, set, name = "foo")]`.  Splitting them
/// across two separate `#[pyo3(...)]` attributes on the same field is unsupported
/// by pyo3 itself, so this early-return is safe for all real-world usage.
fn pyo3_field_flags(attrs: &[Attribute]) -> (bool, bool) {
    for attr in attrs {
        if !attr.path().is_ident("pyo3") {
            continue;
        }
        if let Meta::List(ml) = &attr.meta {
            let tokens = ml.tokens.to_string();
            let has_get = tokens.split(',').any(|p| p.trim() == "get");
            let has_set = tokens.split(',').any(|p| p.trim() == "set");
            return (has_get, has_set);
        }
    }
    (false, false)
}

/// Build a [`PyType`] directly from a struct field's `syn::Type`.
fn make_field_py_type(
    ty: &Type,
    config: &Config,
    type_alias_map: &HashMap<String, Type>,
    type_map_preserve_idents: &HashSet<String>,
) -> PyType {
    let rust_type = expand_type_aliases(ty, type_alias_map, type_map_preserve_idents, 0);
    let override_str =
        lookup_type_override(ty, config).or_else(|| lookup_type_override(&rust_type, config));
    PyType {
        rust_type,
        override_str,
    }
}

/// Generate [`PyMethod`] stubs for struct fields that carry `#[pyo3(get)]` and/or `#[pyo3(set)]`,
/// or inherit accessors from class-level `#[pyclass(get_all)]` / `#[pyclass(set_all)]` (or the same
/// flags on `#[pyo3(...)]`).
///
/// For each exposed field a getter is emitted first, then (if applicable) a setter.
fn parse_struct_fields_as_methods(
    fields: &[syn::Field],
    cx: ParseContext<'_>,
    class_get_all: bool,
    class_set_all: bool,
    rename_all: Option<&str>,
    parse_warnings: Option<&RefCell<Vec<String>>>,
) -> Vec<PyMethod> {
    let mut methods = Vec::new();
    for field in fields {
        let (field_get, field_set) = pyo3_field_flags(&field.attrs);
        let has_get = field_get || class_get_all;
        let has_set = field_set || class_set_all;
        if !has_get && !has_set {
            continue;
        }
        let field_name = match &field.ident {
            Some(id) => id.to_string(),
            None => continue, // tuple struct field — skip
        };
        let prop_name = extract_pyo3_name(&field.attrs).unwrap_or_else(|| {
            if let Some(rule) = rename_all {
                super::rename_all::apply_pyclass_rename_all(&field_name, rule, parse_warnings)
            } else {
                field_name.clone()
            }
        });
        let doc = extract_doc(&field.attrs);
        let py_type = make_field_py_type(
            &field.ty,
            cx.config,
            cx.type_alias_map,
            cx.type_map_preserve_idents,
        );

        if has_get {
            methods.push(PyMethod {
                rust_ident: field_name.clone(),
                name: prop_name.clone(),
                doc: doc.clone(),
                kind: MethodKind::Getter(prop_name.clone()),
                signature_override: None,
                params: vec![],
                return_type: py_type.clone(),
            });
        }
        if has_set {
            methods.push(PyMethod {
                rust_ident: field_name.clone(),
                name: prop_name.clone(),
                doc: doc.clone(),
                kind: MethodKind::Setter(prop_name.clone()),
                signature_override: None,
                params: vec![PyParam {
                    name: "value".to_string(),
                    ty: py_type.clone(),
                    default: None,
                    kind: ParamKind::Regular,
                }],
                return_type: PyType {
                    rust_type: syn::parse_quote! { () },
                    override_str: None,
                },
            });
        }
    }
    methods
}

/// `display_name`: name used in .pyi (Python name, from `#[pyclass(name = "...")]` or ident).
/// `rust_name_for_impl`: Rust type name for looking up `#[pymethods]` impl block and struct fields.
fn parse_pyclass_struct(
    display_name: &str,
    rust_name_for_impl: &str,
    attrs: &[Attribute],
    path: &Path,
    cx: ParseContext<'_>,
    is_enum: bool,
) -> PyClass {
    let doc = extract_doc(attrs);

    let extends_ty = extract_pyclass_extends_type(attrs, cx.parse_warnings);
    if is_enum
        && extends_ty.is_some()
        && let Some(w) = cx.parse_warnings
    {
        w.borrow_mut().push(
            "rylai: #[pyclass] on enum cannot inherit (PyO3); ignoring `extends` for stub"
                .to_string(),
        );
    }

    let allows_python_subclass = !is_enum && extract_pyclass_subclass(attrs);
    let extends = if is_enum {
        None
    } else {
        extends_ty.as_ref().and_then(extends_spec_from_syn_type)
    };

    let (class_get_all, class_set_all) = extract_pyclass_get_all_set_all(attrs);
    let rename_all = extract_pyclass_rename_all(attrs);
    let rename_lit = rename_all.as_deref();

    // Invalid `rename_all` is warned once per class; per-field apply must not repeat the same message.
    let field_parse_warnings = match rename_lit {
        Some(rule) if !super::rename_all::is_valid_pyclass_rename_all_rule(rule) => {
            if let Some(w) = cx.parse_warnings {
                w.borrow_mut()
                    .push(super::rename_all::format_invalid_pyclass_rename_all_warning(rule));
            }
            None
        }
        _ => cx.parse_warnings,
    };

    // Properties from `#[pyo3(get)]` / `#[pyo3(set)]` struct fields (or `get_all` / `set_all` on the
    // pyclass) come first.
    let mut methods: Vec<PyMethod> =
        if let Some(fields) = cx.struct_fields_map.get(rust_name_for_impl) {
            parse_struct_fields_as_methods(
                fields,
                cx,
                class_get_all,
                class_set_all,
                rename_lit,
                field_parse_warnings,
            )
        } else {
            vec![]
        };

    // Methods from `#[pymethods]` impl blocks follow.
    methods.extend(
        cx.impl_map
            .get(rust_name_for_impl)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|item| {
                parse_pymethod(
                    &item,
                    cx.config,
                    cx.type_alias_map,
                    cx.type_map_preserve_idents,
                )
            }),
    );

    PyClass {
        name: display_name.to_string(),
        rust_name: rust_name_for_impl.to_string(),
        module: extract_pyclass_module(attrs),
        allows_python_subclass,
        extends,
        is_enum,
        doc,
        methods,
        source_file: path.to_path_buf(),
    }
}

fn parse_pymethod(
    item: &ImplItem,
    config: &Config,
    type_alias_map: &HashMap<String, Type>,
    type_map_preserve_idents: &HashSet<String>,
) -> Option<PyMethod> {
    let ImplItem::Fn(m) = item else { return None };

    let rust_ident = m.sig.ident.to_string();
    // #[pyo3(name = "foo")] overrides the Rust method name
    let name = extract_pyo3_name(&m.attrs).unwrap_or_else(|| rust_ident.clone());
    let doc = extract_doc(&m.attrs);
    let signature_override = extract_pyo3_signature(&m.attrs);
    let params = parse_params(&m.sig, config, type_alias_map, type_map_preserve_idents);
    let return_type = parse_return_type(
        &m.sig.output,
        config,
        type_alias_map,
        type_map_preserve_idents,
    );
    let kind = detect_method_kind(&m.attrs, &name);

    Some(PyMethod {
        rust_ident,
        name,
        doc,
        kind,
        signature_override,
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
    extract_property_name(attrs, "getter", fn_name, |_| fn_name.to_string())
}

fn extract_setter_name(attrs: &[Attribute], fn_name: &str) -> Option<String> {
    extract_property_name(attrs, "setter", fn_name, infer_pyo3_setter_property_name)
}

/// Reads `#[getter]` / `#[setter]` (optional rename) for pymethod kind detection.
fn extract_property_name<F>(
    attrs: &[Attribute],
    attr_name: &str,
    fn_name: &str,
    default_name: F,
) -> Option<String>
where
    F: FnOnce(&str) -> String,
{
    for attr in attrs {
        if attr.path().is_ident(attr_name) {
            let rename = extract_attr_string_arg(attr);
            return Some(rename.unwrap_or_else(|| default_name(fn_name)));
        }
    }
    None
}

/// PyO3: `#[setter]` without a rename uses the Rust fn name with a leading `set_` stripped when
/// present (e.g. `set_num` → property `num`).
fn infer_pyo3_setter_property_name(fn_name: &str) -> String {
    fn_name
        .strip_prefix("set_")
        .filter(|s| !s.is_empty())
        .unwrap_or(fn_name)
        .to_string()
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

/// Maps struct/enum **simple name** → list of ImplItem from `#[pymethods]` blocks.
///
/// # Known limitation — name collision
///
/// The key is the last path segment of the `impl` self-type (e.g. `Foo` for both
/// `mod a::Foo` and `mod b::Foo`).  If two different structs in separate modules
/// share the same simple name *and* both carry `#[pymethods]`, their impl items
/// are merged under a single key and will both appear on whichever class happens
/// to match that name.  In practice this is rare for pyo3 codebases because
/// Python class names (the public API) are almost always unique within a crate,
/// but it is a known static-analysis blind spot.
///
/// A proper fix requires tracking fully-qualified Rust paths for both the
/// `#[pyclass]` struct and the `impl` block, which is planned for Phase 5
/// (cross-file symbol resolution).  Until then, avoid exposing two `#[pyclass]`
/// types with the same simple name from the same crate.
pub type ImplMap = std::collections::HashMap<String, Vec<ImplItem>>;

/// Maps Rust struct **simple name** → named fields of that `#[pyclass]` struct.
/// Used to generate `@property` stubs for fields annotated with `#[pyo3(get)]` / `#[pyo3(set)]`,
/// or for all fields when the class uses `get_all` / `set_all` on `#[pyclass]` / `#[pyo3]`.
/// Python-visible names follow `#[pyo3(name = ...)]` on the field, else `rename_all` on the class,
/// else the Rust field ident.
///
/// # Known limitation — name collision
///
/// The key is the struct's simple ident (e.g. `Foo` for both `mod a::Foo` and `mod b::Foo`).
/// If two `#[pyclass]` structs in separate modules share the same simple name, the last file
/// processed wins and the other struct's fields are silently dropped.  In practice this is
/// rare for pyo3 codebases because Python class names are almost always unique within a crate,
/// but it is a known static-analysis blind spot.
///
/// A proper fix requires tracking fully-qualified Rust paths, which is planned for Phase 5
/// (cross-file symbol resolution).  Until then, avoid two `#[pyclass]` structs with the same
/// simple name in the same crate.
pub type StructFieldsMap = std::collections::HashMap<String, Vec<syn::Field>>;

/// Build a global [`StructFieldsMap`] from every `.rs` file in the crate.
///
/// Only named-field `#[pyclass]` structs are included; tuple structs and enums are skipped.
/// Items behind `#[cfg(feature = "...")]` are included only when the feature is in `enabled_features`.
pub fn build_struct_fields_map(
    files: &[(std::path::PathBuf, syn::File)],
    enabled_features: &[String],
) -> StructFieldsMap {
    build_map_from_files(
        files,
        enabled_features,
        StructFieldsMap::new(),
        collect_struct_fields_from_items,
    )
}

fn collect_struct_fields_from_items(
    items: &[Item],
    map: &mut StructFieldsMap,
    enabled_features: &[String],
) {
    walk_items_with_cfg(items, enabled_features, &mut |item| {
        if let Item::Struct(s) = item
            && has_attr(&s.attrs, "pyclass")
            && let Fields::Named(named) = &s.fields
        {
            let fields: Vec<syn::Field> = named.named.iter().cloned().collect();
            map.insert(s.ident.to_string(), fields);
        }
    });
}

/// Maps Rust `#[pyclass]` type name → its attributes (e.g. for doc comments).
/// Used in Style B so that `m.add_class::<T>()` can still get the docstring from the struct/enum definition.
///
/// Keys are bare type names (no module path). Duplicate type names across the crate overwrite;
/// the last one wins. Same limitation as [`ImplMap`].
pub type PyclassAttrsMap = std::collections::HashMap<String, Vec<Attribute>>;

/// Build a global [`PyclassAttrsMap`] from every `.rs` file in the crate.
///
/// See [`PyclassAttrsMap`] for the known name-collision limitation.
/// Items behind `#[cfg(feature = "...")]` are included only when the feature is in `enabled_features`.
pub fn build_pyclass_attrs_map(
    files: &[(std::path::PathBuf, syn::File)],
    enabled_features: &[String],
) -> PyclassAttrsMap {
    build_map_from_files(
        files,
        enabled_features,
        PyclassAttrsMap::new(),
        collect_pyclass_attrs_from_items,
    )
}

fn collect_pyclass_attrs_from_items(
    items: &[Item],
    map: &mut PyclassAttrsMap,
    enabled_features: &[String],
) {
    walk_items_with_cfg(items, enabled_features, &mut |item| {
        if let Item::Struct(s) = item
            && has_attr(&s.attrs, "pyclass")
        {
            map.insert(s.ident.to_string(), s.attrs.clone());
        } else if let Item::Enum(e) = item
            && has_attr(&e.attrs, "pyclass")
        {
            map.insert(e.ident.to_string(), e.attrs.clone());
        }
    });
}

/// Rust type names of items that are `#[pyclass]` enums (for Style B `m.add_class::<E>()`).
pub type PyclassEnumRustNames = HashSet<String>;

/// Build [`PyclassEnumRustNames`] from every `.rs` file in the crate.
pub fn build_pyclass_enum_rust_names(
    files: &[(std::path::PathBuf, syn::File)],
    enabled_features: &[String],
) -> PyclassEnumRustNames {
    build_map_from_files(
        files,
        enabled_features,
        PyclassEnumRustNames::new(),
        collect_pyclass_enum_rust_names_from_items,
    )
}

fn collect_pyclass_enum_rust_names_from_items(
    items: &[Item],
    set: &mut PyclassEnumRustNames,
    enabled_features: &[String],
) {
    walk_items_with_cfg(items, enabled_features, &mut |item| {
        if let Item::Enum(e) = item
            && has_attr(&e.attrs, "pyclass")
        {
            set.insert(e.ident.to_string());
        }
    });
}

/// Build a global ImplMap from every `.rs` file in the crate.
///
/// This must be called before [`extract_modules_from_file`] so that
/// `#[pymethods]` blocks defined in a different file from the `#[pymodule]`
/// (e.g. `edges.rs` vs `lib.rs`) are still resolved correctly.
///
/// See [`ImplMap`] for the known name-collision limitation.
/// Items behind `#[cfg(feature = "...")]` are included only when the feature is in `enabled_features`.
pub fn build_impl_map(
    files: &[(std::path::PathBuf, syn::File)],
    enabled_features: &[String],
) -> ImplMap {
    build_map_from_files(
        files,
        enabled_features,
        ImplMap::new(),
        collect_impl_blocks_from_items,
    )
}

fn collect_impl_blocks_from_items(items: &[Item], map: &mut ImplMap, enabled_features: &[String]) {
    walk_items_with_cfg(items, enabled_features, &mut |item| {
        if let Item::Impl(imp) = item
            && has_attr(&imp.attrs, "pymethods")
            && let Type::Path(tp) = imp.self_ty.as_ref()
            && let Some(seg) = tp.path.segments.last()
        {
            let name = seg.ident.to_string();
            map.entry(name).or_default().extend(imp.items.clone());
        }
    });
}

/// Map from function name (last segment) to PyFunction for cross-module lookup.
/// When a pyfunction is referenced via path like `crate::module::foo`,
/// we look it up by the last segment (`foo`) since Rust function identifiers
/// must be unique within their scope.
pub type PyFunctionMap = HashMap<String, PyFunction>;

/// Build a global map of all pyfunctions in the crate.
///
/// This allows finding pyfunctions referenced via full paths (e.g., `crate::module::foo`)
/// from `wrap_pyfunction!` macro calls in the `#[pymodule]` function.
///
/// Items behind `#[cfg(feature = "...")]` are included only when the feature is in `enabled_features`.
pub fn build_pyfunction_map(
    files: &[(std::path::PathBuf, syn::File)],
    enabled_features: &[String],
) -> PyFunctionMap {
    build_map_from_files_with_ctx(
        files,
        enabled_features,
        PyFunctionMap::new(),
        collect_pyfunctions_from_items,
    )
}

fn collect_pyfunctions_from_items(
    items: &[Item],
    path: &std::path::Path,
    map: &mut PyFunctionMap,
    enabled_features: &[String],
) {
    walk_items_with_ctx(items, path, enabled_features, &mut |item, path| {
        if let Item::Fn(f) = item
            && has_attr(&f.attrs, "pyfunction")
            && let Some(func) = parse_pyfunction(
                f,
                path,
                &Config::default(),
                &HashMap::new(),
                &HashSet::new(),
            )
        {
            map.insert(func.rust_name.clone(), func);
        }
    });
}

// ── cfg(feature) evaluation for [features] enabled ──────────────────────────

/// Returns true if the item should be considered active given the configured enabled features.
/// - If there is no `#[cfg(...)]` attribute, returns true.
/// - If there are one or more `#[cfg(...)]`, all must evaluate to true (Rust semantics).
/// - Supports: `feature = "x"`, `not(...)`, `all(...)`, `any(...)`.
/// - Unknown predicates (e.g. `target_os`) are treated as true (permissive for stub generation).
pub fn cfg_is_active(attrs: &[Attribute], enabled_features: &[String]) -> bool {
    for attr in attrs {
        if !attr.path().is_ident("cfg") {
            continue;
        }
        let Meta::List(ml) = &attr.meta else {
            continue;
        };
        let Ok(inner) = syn::parse2::<Meta>(ml.tokens.clone()) else {
            continue;
        };
        match eval_cfg_meta(&inner, enabled_features) {
            Some(true) => {}
            Some(false) => return false,
            None => {}
        }
    }
    true
}

/// Parses a single nested meta from `parse_nested_meta` into a full `Meta`.
/// Handles `feature = "x"` (NameValue); used for all(...) and any(...) with feature predicates.
fn parse_nested_meta_as_meta(meta: syn::meta::ParseNestedMeta) -> syn::Result<Meta> {
    let path = meta.path.clone();
    if meta.input.peek(syn::token::Eq) {
        let eq_token: syn::token::Eq = meta.input.parse()?;
        let value: Expr = meta.input.parse()?;
        Ok(Meta::NameValue(syn::MetaNameValue {
            path,
            eq_token,
            value,
        }))
    } else {
        // Nested list (e.g. not(...)): re-parse the rest as Meta and wrap in List
        let inner: Meta = meta.input.parse()?;
        Ok(Meta::List(syn::MetaList {
            path,
            delimiter: syn::MacroDelimiter::Paren(Default::default()),
            tokens: quote::quote!(#inner),
        }))
    }
}

/// Evaluates a single cfg predicate (or compound). Returns None for unknown predicates (treated as true).
fn eval_cfg_meta(meta: &Meta, enabled: &[String]) -> Option<bool> {
    match meta {
        Meta::Path(_) => Some(true),
        Meta::NameValue(nv) => {
            if nv.path.is_ident("feature")
                && let Expr::Lit(lit) = &nv.value
                && let Lit::Str(s) = &lit.lit
            {
                let name = s.value();
                return Some(enabled.iter().any(|f| f == &name));
            }
            Some(true)
        }
        Meta::List(ml) => {
            let ident = ml.path.get_ident().map(|i| i.to_string());
            match ident.as_deref() {
                Some("not") => {
                    let inner: Meta = syn::parse2(ml.tokens.clone()).ok()?;
                    Some(!eval_cfg_meta(&inner, enabled)?)
                }
                Some("all") => {
                    let mut nested = Vec::new();
                    ml.parse_nested_meta(|meta| {
                        nested.push(parse_nested_meta_as_meta(meta)?);
                        Ok(())
                    })
                    .ok()?;
                    for m in &nested {
                        if !eval_cfg_meta(m, enabled)? {
                            return Some(false);
                        }
                    }
                    Some(true)
                }
                Some("any") => {
                    let mut nested = Vec::new();
                    ml.parse_nested_meta(|meta| {
                        nested.push(parse_nested_meta_as_meta(meta)?);
                        Ok(())
                    })
                    .ok()?;
                    for m in &nested {
                        if eval_cfg_meta(m, enabled)? {
                            return Some(true);
                        }
                    }
                    Some(false)
                }
                _ => Some(true),
            }
        }
    }
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

/// Extract `#[pyclass(module = "abcd.efg")]` from attributes.
/// Only considers `#[pyclass(...)]` attributes; `#[pyo3(module=...)]` is ignored.
/// Uses `parse_nested_meta` so string literals (including raw strings) parse correctly.
pub fn extract_pyclass_module(attrs: &[Attribute]) -> Option<String> {
    for attr in attrs {
        if !attr.path().is_ident("pyclass") {
            continue;
        }
        let mut found: Option<String> = None;
        if attr
            .parse_nested_meta(|meta| {
                if meta.path.is_ident("module") {
                    let value = meta.value()?;
                    let s: LitStr = value.parse()?;
                    let v = s.value();
                    if !v.is_empty() {
                        found = Some(v);
                    }
                    return Ok(());
                }
                // `parse_nested_meta` requires each nested item to be fully consumed.
                if meta.input.peek(syn::token::Eq) {
                    let _ = meta.value()?.parse::<Expr>()?;
                }
                Ok(())
            })
            .is_ok()
            && found.is_some()
        {
            return found;
        }
    }
    None
}

/// Returns `(get_all, set_all)` from any `#[pyclass(...)]` or `#[pyo3(...)]` on the pyclass item.
///
/// PyO3 allows these flags on either attribute; if either lists `get_all` / `set_all`, the
/// corresponding side is true.
pub fn extract_pyclass_get_all_set_all(attrs: &[Attribute]) -> (bool, bool) {
    let mut get_all = false;
    let mut set_all = false;
    for attr in attrs {
        if !attr.path().is_ident("pyclass") && !attr.path().is_ident("pyo3") {
            continue;
        }
        let _ = attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("get_all") {
                get_all = true;
                return Ok(());
            }
            if meta.path.is_ident("set_all") {
                set_all = true;
                return Ok(());
            }
            if meta.input.peek(syn::token::Eq) {
                let _ = meta.value()?.parse::<Expr>()?;
            }
            Ok(())
        });
    }
    (get_all, set_all)
}

/// Last `rename_all = "..."` from any `#[pyclass(...)]` or `#[pyo3(...)]` on the pyclass item.
///
/// When both attributes set `rename_all`, the **last** occurrence in source order wins (matching
/// typical macro “later wins” behavior for duplicate options).
pub fn extract_pyclass_rename_all(attrs: &[Attribute]) -> Option<String> {
    let mut last: Option<String> = None;
    for attr in attrs {
        if !attr.path().is_ident("pyclass") && !attr.path().is_ident("pyo3") {
            continue;
        }
        let _ = attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("rename_all") {
                if let Ok(v) = meta.value()
                    && let Ok(s) = v.parse::<LitStr>()
                {
                    last = Some(s.value());
                }
                return Ok(());
            }
            if meta.input.peek(syn::token::Eq) {
                let _ = meta.value()?.parse::<Expr>()?;
            }
            Ok(())
        });
    }
    last
}

/// `true` when `#[pyclass(subclass)]` or `#[pyo3(subclass)]` is present.
pub fn extract_pyclass_subclass(attrs: &[Attribute]) -> bool {
    for attr in attrs {
        if !attr.path().is_ident("pyclass") && !attr.path().is_ident("pyo3") {
            continue;
        }
        let mut found = false;
        let _ = attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("subclass") {
                found = true;
                return Ok(());
            }
            if meta.input.peek(syn::token::Eq) {
                let _ = meta.value()?.parse::<Expr>()?;
            }
            Ok(())
        });
        if found {
            return true;
        }
    }
    false
}

/// Last `extends = Ty` from `#[pyclass]` / `#[pyo3]` (later occurrence wins).
///
/// When a value after `extends =` is not a valid [`Type`], that clause is skipped; if
/// `parse_warnings` is set, a message is recorded for each failed parse.
pub fn extract_pyclass_extends_type(
    attrs: &[Attribute],
    parse_warnings: Option<&RefCell<Vec<String>>>,
) -> Option<Type> {
    let mut last: Option<Type> = None;
    for attr in attrs {
        if !attr.path().is_ident("pyclass") && !attr.path().is_ident("pyo3") {
            continue;
        }
        let _ = attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("extends") {
                let value = meta.value()?;
                match value.parse::<Type>() {
                    Ok(ty) => last = Some(ty),
                    Err(_) => {
                        if let Some(w) = parse_warnings {
                            w.borrow_mut().push(
                                "rylai: could not parse `extends` value as a type; omitting that base in the stub"
                                    .to_string(),
                            );
                        }
                    }
                }
                return Ok(());
            }
            if meta.input.peek(syn::token::Eq) {
                let _ = meta.value()?.parse::<Expr>()?;
            }
            Ok(())
        });
    }
    last
}

/// Last segment of a path type (e.g. `PyDict` from `pyo3::types::PyDict`). `extends` resolution
/// matches PyO3/`known_classes` by Rust **type name**, not full path — use the unqualified name in
/// `#[pyclass(extends = Base)]` if the base is another `#[pyclass]` in the same crate.
fn type_path_segment_for_extends(ty: &Type) -> Option<String> {
    match ty {
        Type::Path(tp) => tp.path.segments.last().map(|s| s.ident.to_string()),
        Type::Group(g) => type_path_segment_for_extends(&g.elem),
        Type::Paren(p) => type_path_segment_for_extends(&p.elem),
        _ => None,
    }
}

/// Maps `extends` type to a Python builtin base or a same-crate `#[pyclass]` by **last path
/// segment** only (see [`type_path_segment_for_extends`]). Builtin PyO3 bases use
/// [`crate::type_map::pyo3_builtin_segment_to_python_class`]; `PyAny` yields no explicit base.
pub fn extends_spec_from_syn_type(ty: &Type) -> Option<ExtendsSpec> {
    let seg = type_path_segment_for_extends(ty)?;
    if seg == "PyAny" {
        return None;
    }
    if let Some(b) = crate::type_map::pyo3_builtin_segment_to_python_class(&seg) {
        return Some(ExtendsSpec::Builtin(b));
    }
    Some(ExtendsSpec::PyClassRustName(seg))
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

/// Builds a `[type_map]` lookup key from `ty`. Only [`Type::Path`] (and references to one) produce
/// a non-empty string; tuples and other shapes return `""`, so literal `(T, U, …)` cannot be keyed
/// in config — users need a Rust `type` alias (documented in README).
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
    use crate::collector::MethodKind;
    use crate::config::{Config, RenderPolicy};
    use std::cell::RefCell;
    use std::path::Path;
    use std::sync::LazyLock;

    /// Empty `[type_map]` preserve set for `parse_pyfunction` tests with default config.
    static EMPTY_TYPE_MAP_PRESERVE: LazyLock<HashSet<String>> = LazyLock::new(HashSet::new);

    /// Empty enabled features for tests that do not gate on cfg(feature).
    fn no_features() -> Vec<String> {
        vec![]
    }

    fn dummy_path() -> &'static Path {
        Path::new("test.rs")
    }

    /// Construct a [`ParseContext`] from individual pieces.
    /// Prefer this helper over inline struct literals in tests to stay DRY.
    fn make_cx<'a>(
        config: &'a Config,
        impl_map: &'a ImplMap,
        struct_fields_map: &'a StructFieldsMap,
        type_alias_map: &'a HashMap<String, Type>,
        pyclass_attrs_map: &'a PyclassAttrsMap,
    ) -> ParseContext<'a> {
        let preserve: &'a HashSet<String> = Box::leak(Box::new(
            crate::config::type_map_preserve_alias_idents(&config.type_map),
        ));
        ParseContext {
            config,
            impl_map,
            struct_fields_map,
            type_alias_map,
            pyclass_attrs_map,
            pyclass_enum_rust_names: None,
            parse_warnings: None,
            type_map_preserve_idents: preserve,
            pyfunction_map: None,
        }
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

    // ── extract_pyclass_module ───────────────────────────────────────────────

    #[test]
    fn extract_pyclass_module_present() {
        let item: syn::ItemStruct = syn::parse_quote! {
            #[pyclass(module = "abcd.efg")]
            struct Layer {}
        };
        assert_eq!(
            extract_pyclass_module(&item.attrs),
            Some("abcd.efg".to_string())
        );
    }

    #[test]
    fn extract_pyclass_module_absent() {
        let item: syn::ItemStruct = syn::parse_quote! {
            #[pyclass]
            struct Foo {}
        };
        assert_eq!(extract_pyclass_module(&item.attrs), None);
    }

    #[test]
    fn extract_pyclass_module_with_name() {
        let item: syn::ItemStruct = syn::parse_quote! {
            #[pyclass(name = "MyLayer", module = "abcd.efg")]
            struct RustLayer {}
        };
        assert_eq!(
            extract_pyclass_module(&item.attrs),
            Some("abcd.efg".to_string())
        );
    }

    #[test]
    fn extract_pyclass_module_raw_string_literal() {
        let item: syn::ItemStruct = syn::parse_quote! {
            #[pyclass(module = r"abcd.efg")]
            struct Layer {}
        };
        assert_eq!(
            extract_pyclass_module(&item.attrs),
            Some("abcd.efg".to_string())
        );
    }

    // ── extract_pyclass_get_all_set_all ─────────────────────────────────────

    #[test]
    fn extract_pyclass_get_all_set_all_from_pyclass_list() {
        let item: syn::ItemStruct = syn::parse_quote! {
            #[pyclass(get_all, set_all, name = "Pt")]
            struct Point {
                x: i32,
                y: i32,
            }
        };
        assert_eq!(extract_pyclass_get_all_set_all(&item.attrs), (true, true));
    }

    #[test]
    fn extract_pyclass_get_all_set_all_from_pyo3_attr() {
        let item: syn::ItemStruct = syn::parse_quote! {
            #[pyclass(rename_all = "camelCase")]
            #[pyo3(get_all)]
            struct Point {
                x: i32,
            }
        };
        assert_eq!(extract_pyclass_get_all_set_all(&item.attrs), (true, false));
    }

    #[test]
    fn pyclass_get_all_set_all_collects_field_properties() {
        let file = syn::parse_file(
            r#"
#[pymodule]
mod my_mod {
    #[pyclass(get_all, set_all)]
    struct Point {
        x: i32,
        y: i32,
    }
}
"#,
        )
        .unwrap();
        let path = Path::new("lib.rs");
        let path_buf = path.to_path_buf();
        let config = Config::default();
        let pyclass_map = HashMap::new();
        let type_alias_map =
            build_type_alias_map(&[(path_buf.clone(), file.clone())], &no_features());
        let impl_map = build_impl_map(&[(path_buf.clone(), file.clone())], &no_features());
        let fields_map =
            build_struct_fields_map(&[(path_buf.clone(), file.clone())], &no_features());
        let attrs_map = build_pyclass_attrs_map(&[(path_buf, file.clone())], &no_features());
        let cx = make_cx(&config, &impl_map, &fields_map, &type_alias_map, &attrs_map);
        let modules = extract_modules_from_file(&file, path, &pyclass_map, cx);
        let class = match &modules[0].items[0] {
            PyItem::Class(c) => c,
            other => panic!("expected PyItem::Class, got {other:?}"),
        };
        assert_eq!(class.name, "Point");
        let getters: Vec<_> = class
            .methods
            .iter()
            .filter(|m| matches!(m.kind, MethodKind::Getter(_)))
            .map(|m| m.name.as_str())
            .collect();
        assert_eq!(getters, vec!["x", "y"]);
        let setters: Vec<_> = class
            .methods
            .iter()
            .filter(|m| matches!(m.kind, MethodKind::Setter(_)))
            .map(|m| m.name.as_str())
            .collect();
        assert_eq!(setters, vec!["x", "y"]);
    }

    // ── extract_pyclass_rename_all / get_all + rename_all ────────────────────

    #[test]
    fn extract_pyclass_rename_all_from_pyclass() {
        let item: syn::ItemStruct = syn::parse_quote! {
            #[pyclass(rename_all = "camelCase", get_all)]
            struct Point {
                x: i32,
            }
        };
        assert_eq!(
            extract_pyclass_rename_all(&item.attrs).as_deref(),
            Some("camelCase")
        );
    }

    #[test]
    fn extract_pyclass_rename_all_last_attr_wins() {
        let item: syn::ItemStruct = syn::parse_quote! {
            #[pyclass(rename_all = "camelCase")]
            #[pyo3(rename_all = "snake_case")]
            struct Point {}
        };
        assert_eq!(
            extract_pyclass_rename_all(&item.attrs).as_deref(),
            Some("snake_case")
        );
    }

    #[test]
    fn pyclass_get_all_with_rename_all_collects_camel_property_names() {
        let file = syn::parse_file(
            r#"
#[pymodule]
mod my_mod {
    #[pyclass(get_all, rename_all = "camelCase")]
    struct Point {
        foo_bar: i32,
        x: i32,
    }
}
"#,
        )
        .unwrap();
        let path = Path::new("lib.rs");
        let path_buf = path.to_path_buf();
        let config = Config::default();
        let pyclass_map = HashMap::new();
        let type_alias_map =
            build_type_alias_map(&[(path_buf.clone(), file.clone())], &no_features());
        let impl_map = build_impl_map(&[(path_buf.clone(), file.clone())], &no_features());
        let fields_map =
            build_struct_fields_map(&[(path_buf.clone(), file.clone())], &no_features());
        let attrs_map = build_pyclass_attrs_map(&[(path_buf, file.clone())], &no_features());
        let cx = make_cx(&config, &impl_map, &fields_map, &type_alias_map, &attrs_map);
        let modules = extract_modules_from_file(&file, path, &pyclass_map, cx);
        let class = match &modules[0].items[0] {
            PyItem::Class(c) => c,
            other => panic!("expected PyItem::Class, got {other:?}"),
        };
        let getters: Vec<_> = class
            .methods
            .iter()
            .filter(|m| matches!(m.kind, MethodKind::Getter(_)))
            .map(|m| m.name.as_str())
            .collect();
        assert_eq!(getters, vec!["fooBar", "x"]);
    }

    #[test]
    fn pyo3_field_name_overrides_class_rename_all() {
        let file = syn::parse_file(
            r#"
#[pymodule]
mod my_mod {
    #[pyclass(get_all, rename_all = "camelCase")]
    struct Point {
        #[pyo3(name = "still_snake")]
        foo_bar: i32,
    }
}
"#,
        )
        .unwrap();
        let path = Path::new("lib.rs");
        let path_buf = path.to_path_buf();
        let config = Config::default();
        let pyclass_map = HashMap::new();
        let type_alias_map =
            build_type_alias_map(&[(path_buf.clone(), file.clone())], &no_features());
        let impl_map = build_impl_map(&[(path_buf.clone(), file.clone())], &no_features());
        let fields_map =
            build_struct_fields_map(&[(path_buf.clone(), file.clone())], &no_features());
        let attrs_map = build_pyclass_attrs_map(&[(path_buf, file.clone())], &no_features());
        let cx = make_cx(&config, &impl_map, &fields_map, &type_alias_map, &attrs_map);
        let modules = extract_modules_from_file(&file, path, &pyclass_map, cx);
        let class = match &modules[0].items[0] {
            PyItem::Class(c) => c,
            other => panic!("expected PyItem::Class, got {other:?}"),
        };
        let getter = class
            .methods
            .iter()
            .find(|m| matches!(m.kind, MethodKind::Getter(_)))
            .expect("getter");
        assert_eq!(getter.name, "still_snake");
    }

    #[test]
    fn invalid_rename_all_records_parse_warning_and_keeps_rust_field_name() {
        let file = syn::parse_file(
            r#"
#[pymodule]
mod my_mod {
    #[pyclass(get_all, rename_all = "not_a_valid_rule")]
    struct Point {
        foo_bar: i32,
    }
}
"#,
        )
        .unwrap();
        let path = Path::new("lib.rs");
        let path_buf = path.to_path_buf();
        let config = Config::default();
        let pyclass_map = HashMap::new();
        let type_alias_map =
            build_type_alias_map(&[(path_buf.clone(), file.clone())], &no_features());
        let impl_map = build_impl_map(&[(path_buf.clone(), file.clone())], &no_features());
        let fields_map =
            build_struct_fields_map(&[(path_buf.clone(), file.clone())], &no_features());
        let attrs_map = build_pyclass_attrs_map(&[(path_buf, file.clone())], &no_features());
        let warnings = RefCell::new(Vec::new());
        let type_map_preserve_idents =
            crate::config::type_map_preserve_alias_idents(&config.type_map);
        let cx = ParseContext {
            config: &config,
            impl_map: &impl_map,
            struct_fields_map: &fields_map,
            type_alias_map: &type_alias_map,
            pyclass_attrs_map: &attrs_map,
            pyclass_enum_rust_names: None,
            parse_warnings: Some(&warnings),
            type_map_preserve_idents: &type_map_preserve_idents,
            pyfunction_map: None,
        };
        let modules = extract_modules_from_file(&file, path, &pyclass_map, cx);
        let class = match &modules[0].items[0] {
            PyItem::Class(c) => c,
            other => panic!("expected PyItem::Class, got {other:?}"),
        };
        let getter = class
            .methods
            .iter()
            .find(|m| matches!(m.kind, MethodKind::Getter(_)))
            .expect("getter");
        assert_eq!(getter.name, "foo_bar");
        assert_eq!(warnings.borrow().len(), 1);
        assert!(warnings.borrow()[0].contains("not_a_valid_rule"));
    }

    #[test]
    fn invalid_rename_all_single_warning_with_multiple_get_all_fields() {
        let file = syn::parse_file(
            r#"
#[pymodule]
mod my_mod {
    #[pyclass(get_all, rename_all = "not_a_valid_rule")]
    struct Point {
        foo_bar: i32,
        baz_qux: i32,
    }
}
"#,
        )
        .unwrap();
        let path = Path::new("lib.rs");
        let path_buf = path.to_path_buf();
        let config = Config::default();
        let pyclass_map = HashMap::new();
        let type_alias_map =
            build_type_alias_map(&[(path_buf.clone(), file.clone())], &no_features());
        let impl_map = build_impl_map(&[(path_buf.clone(), file.clone())], &no_features());
        let fields_map =
            build_struct_fields_map(&[(path_buf.clone(), file.clone())], &no_features());
        let attrs_map = build_pyclass_attrs_map(&[(path_buf, file.clone())], &no_features());
        let warnings = RefCell::new(Vec::new());
        let type_map_preserve_idents =
            crate::config::type_map_preserve_alias_idents(&config.type_map);
        let cx = ParseContext {
            config: &config,
            impl_map: &impl_map,
            struct_fields_map: &fields_map,
            type_alias_map: &type_alias_map,
            pyclass_attrs_map: &attrs_map,
            pyclass_enum_rust_names: None,
            parse_warnings: Some(&warnings),
            type_map_preserve_idents: &type_map_preserve_idents,
            pyfunction_map: None,
        };
        let _modules = extract_modules_from_file(&file, path, &pyclass_map, cx);
        assert_eq!(warnings.borrow().len(), 1);
        assert!(warnings.borrow()[0].contains("not_a_valid_rule"));
    }

    #[test]
    fn pyclass_rename_all_emits_camel_property_in_generated_stub() {
        let file = syn::parse_file(
            r#"
#[pymodule]
mod my_mod {
    #[pyclass(get_all, rename_all = "camelCase")]
    struct Point {
        foo_bar: i32,
    }
}
"#,
        )
        .unwrap();
        let path = Path::new("lib.rs");
        let path_buf = path.to_path_buf();
        let config = Config::default();
        let pyclass_map = HashMap::new();
        let type_alias_map =
            build_type_alias_map(&[(path_buf.clone(), file.clone())], &no_features());
        let impl_map = build_impl_map(&[(path_buf.clone(), file.clone())], &no_features());
        let fields_map =
            build_struct_fields_map(&[(path_buf.clone(), file.clone())], &no_features());
        let attrs_map = build_pyclass_attrs_map(&[(path_buf, file.clone())], &no_features());
        let cx = make_cx(&config, &impl_map, &fields_map, &type_alias_map, &attrs_map);
        let modules = extract_modules_from_file(&file, path, &pyclass_map, cx);
        let stub = crate::generator::generate(&modules, &config).expect("stub");
        assert!(
            stub.contains("def fooBar(self) -> int:"),
            "expected camelCase property in stub, got:\n{stub}"
        );
    }

    // ── cfg_is_active ([features] enabled) ───────────────────────────────────

    fn attrs_from_cfg(cfg_expr: &str) -> Vec<syn::Attribute> {
        let item: syn::ItemFn =
            syn::parse_str(&format!("#[cfg({cfg_expr})]\nfn _dummy() {{}}")).unwrap();
        item.attrs
    }

    #[test]
    fn cfg_is_active_no_cfg_returns_true() {
        let item: syn::ItemFn = syn::parse_quote! {
            #[pyfunction]
            fn foo() -> i32 { 0 }
        };
        assert!(cfg_is_active(&item.attrs, &[]));
        assert!(cfg_is_active(&item.attrs, &["extra".to_string()]));
    }

    #[test]
    fn cfg_is_active_feature_enabled() {
        let attrs = attrs_from_cfg(r#"feature = "foo""#);
        assert!(!cfg_is_active(&attrs, &[]));
        assert!(cfg_is_active(&attrs, &["foo".to_string()]));
        assert!(!cfg_is_active(&attrs, &["bar".to_string()]));
        assert!(cfg_is_active(
            &attrs,
            &["bar".to_string(), "foo".to_string()]
        ));
    }

    #[test]
    fn cfg_is_active_not_feature() {
        let attrs = attrs_from_cfg(r#"not(feature = "foo")"#);
        assert!(cfg_is_active(&attrs, &[]));
        assert!(!cfg_is_active(&attrs, &["foo".to_string()]));
        assert!(cfg_is_active(&attrs, &["bar".to_string()]));
    }

    #[test]
    fn cfg_is_active_all() {
        let attrs = attrs_from_cfg(r#"all(feature = "a", feature = "b")"#);
        assert!(!cfg_is_active(&attrs, &[]));
        assert!(!cfg_is_active(&attrs, &["a".to_string()]));
        assert!(!cfg_is_active(&attrs, &["b".to_string()]));
        assert!(cfg_is_active(&attrs, &["a".to_string(), "b".to_string()]));
    }

    #[test]
    fn cfg_is_active_any() {
        let attrs = attrs_from_cfg(r#"any(feature = "a", feature = "b")"#);
        assert!(!cfg_is_active(&attrs, &[]));
        assert!(cfg_is_active(&attrs, &["a".to_string()]));
        assert!(cfg_is_active(&attrs, &["b".to_string()]));
        assert!(cfg_is_active(&attrs, &["a".to_string(), "b".to_string()]));
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
        let map = build_type_alias_map(&files, &no_features());
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
        let type_alias_map =
            build_type_alias_map(&[(path.to_path_buf(), file.clone())], &no_features());
        let impl_map = build_impl_map(&[(path.to_path_buf(), file.clone())], &no_features());
        let fields_map = StructFieldsMap::new();
        let attrs_map = PyclassAttrsMap::new();
        let cx = make_cx(&config, &impl_map, &fields_map, &type_alias_map, &attrs_map);
        let modules = extract_modules_from_file(&file, path, &pyclass_map, cx);
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

    /// `[type_map] PyBbox = "BBox"` must apply inside `Vec<PyBbox>`: lookup uses only `Vec` as key,
    /// and expanding the alias to a tuple would drop `PyBbox` before `map_type` runs.
    #[test]
    fn type_map_for_alias_used_under_vec_preserves_name_and_maps_inner() {
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
        let mut config = Config::default();
        config
            .type_map
            .insert("PyBbox".to_string(), "BBox".to_string());
        let pyclass_map = HashMap::new();
        let type_alias_map =
            build_type_alias_map(&[(path.to_path_buf(), file.clone())], &no_features());
        let impl_map = build_impl_map(&[(path.to_path_buf(), file.clone())], &no_features());
        let fields_map = StructFieldsMap::new();
        let attrs_map = PyclassAttrsMap::new();
        let cx = make_cx(&config, &impl_map, &fields_map, &type_alias_map, &attrs_map);
        let modules = extract_modules_from_file(&file, path, &pyclass_map, cx);
        let func = match &modules[0].items[0] {
            PyItem::Function(f) => f,
            other => panic!("expected PyItem::Function, got {other:?}"),
        };
        let policy = RenderPolicy::from_version(3, 10);
        let mut merged = HashMap::new();
        merged.insert("PyBbox".to_string(), "BBox".to_string());
        let mapping =
            crate::type_map::map_type(&func.return_type.rust_type, &policy, None, &merged);
        assert_eq!(mapping.py_type, "list[BBox]");
    }

    /// When two `[type_map]` keys share a last segment but map to different Python types, the alias
    /// name is not treated as preserved — it expands so stubs stay consistent with an ambiguous config.
    #[test]
    fn type_map_ambiguous_last_segment_expands_alias() {
        let file = syn::parse_file(
            r#"
type Foo = (i32, i32);

#[pymodule]
mod my_mod {
    #[pyfunction]
    fn f() -> Vec<Foo> { vec![] }
}
"#,
        )
        .unwrap();
        let path = Path::new("lib.rs");
        let mut config = Config::default();
        config
            .type_map
            .insert("a::Foo".to_string(), "X".to_string());
        config
            .type_map
            .insert("b::Foo".to_string(), "Y".to_string());
        let pyclass_map = HashMap::new();
        let type_alias_map =
            build_type_alias_map(&[(path.to_path_buf(), file.clone())], &no_features());
        let impl_map = build_impl_map(&[(path.to_path_buf(), file.clone())], &no_features());
        let fields_map = StructFieldsMap::new();
        let attrs_map = PyclassAttrsMap::new();
        let cx = make_cx(&config, &impl_map, &fields_map, &type_alias_map, &attrs_map);
        let modules = extract_modules_from_file(&file, path, &pyclass_map, cx);
        let func = match &modules[0].items[0] {
            PyItem::Function(f) => f,
            other => panic!("expected PyItem::Function, got {other:?}"),
        };
        let policy = RenderPolicy::from_version(3, 10);
        let mapping =
            crate::type_map::map_type(&func.return_type.rust_type, &policy, None, &HashMap::new());
        assert_eq!(mapping.py_type, "list[tuple[int, int]]");
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
        let type_alias_map =
            build_type_alias_map(&[(path.to_path_buf(), file.clone())], &no_features());
        let impl_map = build_impl_map(&[(path.to_path_buf(), file.clone())], &no_features());
        let fields_map = StructFieldsMap::new();
        let attrs_map = PyclassAttrsMap::new();
        let cx = make_cx(&config, &impl_map, &fields_map, &type_alias_map, &attrs_map);
        let modules = extract_modules_from_file(&file, path, &pyclass_map, cx);
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
        let map = build_pyclass_name_map(&files, &no_features());
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
        let impl_map = build_impl_map(&[(path.to_path_buf(), file.clone())], &no_features());
        let fields_map = StructFieldsMap::new();
        let attrs_map = PyclassAttrsMap::new();
        let cx = make_cx(&config, &impl_map, &fields_map, &type_alias_map, &attrs_map);
        let modules = extract_modules_from_file(&file, path, &map, cx);
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
        let impl_map = build_impl_map(&[(path.to_path_buf(), file.clone())], &no_features());
        let fields_map = StructFieldsMap::new();
        let attrs_map = PyclassAttrsMap::new();
        let cx = make_cx(&config, &impl_map, &fields_map, &type_alias_map, &attrs_map);
        let modules = extract_modules_from_file(&file, path, &map, cx);
        assert_eq!(modules.len(), 1);
        match &modules[0].items[0] {
            PyItem::Class(c) => assert_eq!(c.name, "MyRustType"),
            other => panic!("expected PyItem::Class, got {:?}", other),
        }
    }

    /// Style B: docstring on the struct/enum is looked up via pyclass_attrs_map and
    /// appears on the collected class (fixes docstring loss when class is added via m.add_class::<T>()).
    #[test]
    fn style_b_class_docstring_from_pyclass_attrs_map() {
        let file = syn::parse_file(
            r#"
/// Python-exposed TableCellValue for to_list().
#[pyclass(name = "TableCellValue")]
struct PyTableCellValue {
    #[pyo3(get)]
    pub text: Option<String>,
}

#[pymodule]
fn my_mod(m: &pyo3::Bound<'_, pyo3::PyModule>) -> pyo3::PyResult<()> {
    m.add_class::<PyTableCellValue>()?;
    Ok(())
}
"#,
        )
        .unwrap();
        let path = Path::new("lib.rs");
        let path_buf = path.to_path_buf();
        let config = Config::default();
        let mut pyclass_name_map = HashMap::new();
        pyclass_name_map.insert("PyTableCellValue".to_string(), "TableCellValue".to_string());

        let type_alias_map = HashMap::new();
        let impl_map = build_impl_map(&[(path_buf.clone(), file.clone())], &no_features());
        let struct_fields_map =
            build_struct_fields_map(&[(path_buf.clone(), file.clone())], &no_features());
        let pyclass_attrs_map =
            build_pyclass_attrs_map(&[(path_buf, file.clone())], &no_features());

        let cx = make_cx(
            &config,
            &impl_map,
            &struct_fields_map,
            &type_alias_map,
            &pyclass_attrs_map,
        );
        let modules = extract_modules_from_file(&file, path, &pyclass_name_map, cx);

        assert_eq!(modules.len(), 1);
        let class = match &modules[0].items[0] {
            PyItem::Class(c) => c,
            other => panic!("expected PyItem::Class, got {:?}", other),
        };
        assert_eq!(class.name, "TableCellValue");
        assert!(
            !class.doc.is_empty(),
            "Style B class must get doc from pyclass_attrs_map, got doc: {:?}",
            class.doc
        );
        assert_eq!(
            class.doc[0].trim(),
            "Python-exposed TableCellValue for to_list().",
            "doc line should match struct doc comment"
        );
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
        let func = parse_pyfunction(
            &item,
            dummy_path(),
            &config,
            &HashMap::new(),
            &EMPTY_TYPE_MAP_PRESERVE,
        )
        .unwrap();
        assert_eq!(func.name, "find_all_cells_bboxes");
    }

    #[test]
    fn pyfunction_without_rename_uses_rust_name() {
        let item: syn::ItemFn = syn::parse_quote! {
            #[pyfunction]
            fn sum_as_string(a: usize, b: usize) -> String { String::new() }
        };
        let config = Config::default();
        let func = parse_pyfunction(
            &item,
            dummy_path(),
            &config,
            &HashMap::new(),
            &EMPTY_TYPE_MAP_PRESERVE,
        )
        .unwrap();
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
        let fields_map = StructFieldsMap::new();
        let name = extract_pyo3_name(&item.attrs).unwrap_or_else(|| item.ident.to_string());
        let rust_name = item.ident.to_string();
        let attrs_map = PyclassAttrsMap::new();
        let cx = make_cx(&config, &impl_map, &fields_map, &type_alias_map, &attrs_map);
        let class = parse_pyclass_struct(&name, &rust_name, &item.attrs, dummy_path(), cx, false);
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
        let fields_map = StructFieldsMap::new();
        let name = extract_pyo3_name(&item.attrs).unwrap_or_else(|| item.ident.to_string());
        let rust_name = item.ident.to_string();
        let attrs_map = PyclassAttrsMap::new();
        let cx = make_cx(&config, &impl_map, &fields_map, &type_alias_map, &attrs_map);
        let class = parse_pyclass_struct(&name, &rust_name, &item.attrs, dummy_path(), cx, false);
        assert_eq!(class.name, "MyType");
    }

    #[test]
    fn extract_pyclass_subclass_from_pyclass_and_pyo3() {
        let s1: syn::ItemStruct = syn::parse_quote! {
            #[pyclass(subclass)]
            struct Base {}
        };
        assert!(extract_pyclass_subclass(&s1.attrs));
        let s2: syn::ItemStruct = syn::parse_quote! {
            #[pyclass]
            #[pyo3(subclass)]
            struct Base2 {}
        };
        assert!(extract_pyclass_subclass(&s2.attrs));
    }
    #[test]
    fn extract_pyclass_extends_pyo3_yields_builtin_dict() {
        let item: syn::ItemStruct = syn::parse_quote! {
            #[pyclass]
            #[pyo3(extends = PyDict)]
            struct MyDict {}
        };
        let ty = extract_pyclass_extends_type(&item.attrs, None).expect("extends");
        assert_eq!(
            extends_spec_from_syn_type(&ty),
            Some(ExtendsSpec::Builtin("dict"))
        );
    }

    #[test]
    fn extract_pyclass_extends_unparseable_records_warning() {
        let item: syn::ItemStruct = syn::parse_quote! {
            #[pyclass(extends = 1 + 2)]
            struct Bad {}
        };
        let warnings = RefCell::new(Vec::new());
        let ty = extract_pyclass_extends_type(&item.attrs, Some(&warnings));
        assert!(ty.is_none());
        assert!(
            warnings
                .borrow()
                .iter()
                .any(|w| w.contains("could not parse `extends`")),
            "expected parse warning, got {:?}",
            warnings.borrow()
        );
    }

    #[test]
    fn parse_pyclass_struct_records_extends_rust_base() {
        let item: syn::ItemStruct = syn::parse_quote! {
            #[pyclass(extends = BaseClass)]
            struct SubClass {}
        };
        let impl_map = ImplMap::default();
        let config = Config::default();
        let type_alias_map = HashMap::new();
        let fields_map = StructFieldsMap::new();
        let attrs_map = PyclassAttrsMap::new();
        let cx = make_cx(&config, &impl_map, &fields_map, &type_alias_map, &attrs_map);
        let name = "SubClass".to_string();
        let rust_name = item.ident.to_string();
        let class = parse_pyclass_struct(&name, &rust_name, &item.attrs, dummy_path(), cx, false);
        assert_eq!(
            class.extends,
            Some(ExtendsSpec::PyClassRustName("BaseClass".to_string()))
        );
        assert!(!class.allows_python_subclass);
        assert!(!class.is_enum);
    }

    #[test]
    fn parse_pyclass_struct_subclass_flag_disables_final_semantics_in_model() {
        let item: syn::ItemStruct = syn::parse_quote! {
            #[pyclass(subclass)]
            struct OpenBase {}
        };
        let impl_map = ImplMap::default();
        let config = Config::default();
        let type_alias_map = HashMap::new();
        let fields_map = StructFieldsMap::new();
        let attrs_map = PyclassAttrsMap::new();
        let cx = make_cx(&config, &impl_map, &fields_map, &type_alias_map, &attrs_map);
        let rust_name = item.ident.to_string();
        let class =
            parse_pyclass_struct(&rust_name, &rust_name, &item.attrs, dummy_path(), cx, false);
        assert!(class.allows_python_subclass);
    }

    #[test]
    fn parse_pyclass_enum_ignores_extends_and_warns() {
        let item: syn::ItemEnum = syn::parse_quote! {
            #[pyclass(extends = SomeBase)]
            enum Bad {
                A,
            }
        };
        let impl_map = ImplMap::default();
        let config = Config::default();
        let type_alias_map = HashMap::new();
        let fields_map = StructFieldsMap::new();
        let attrs_map = PyclassAttrsMap::new();
        let warnings = RefCell::new(Vec::new());
        let type_map_preserve_idents =
            crate::config::type_map_preserve_alias_idents(&config.type_map);
        let cx = ParseContext {
            config: &config,
            impl_map: &impl_map,
            struct_fields_map: &fields_map,
            type_alias_map: &type_alias_map,
            pyclass_attrs_map: &attrs_map,
            pyclass_enum_rust_names: None,
            parse_warnings: Some(&warnings),
            type_map_preserve_idents: &type_map_preserve_idents,
            pyfunction_map: None,
        };
        let rust_name = item.ident.to_string();
        let class =
            parse_pyclass_struct(&rust_name, &rust_name, &item.attrs, dummy_path(), cx, true);
        assert!(class.is_enum);
        assert!(class.extends.is_none());
        assert!(
            warnings
                .borrow()
                .iter()
                .any(|w| w.contains("enum cannot inherit")),
            "expected warning about enum extends, got {:?}",
            warnings.borrow()
        );
    }

    #[test]
    fn build_pyclass_enum_rust_names_marks_enums_only() {
        let file = syn::parse_file(
            r#"
#[pyclass]
struct S {}
#[pyclass]
enum E { V }
"#,
        )
        .unwrap();
        let path = Path::new("lib.rs");
        let set = build_pyclass_enum_rust_names(&[(path.to_path_buf(), file)], &no_features());
        assert!(set.contains("E"));
        assert!(!set.contains("S"));
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
        let preserve = crate::config::type_map_preserve_alias_idents(&config.type_map);
        let func =
            parse_pyfunction(&item, dummy_path(), &config, &type_alias_map, &preserve).unwrap();
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
        let func = parse_pyfunction(
            &item,
            dummy_path(),
            &config,
            &HashMap::new(),
            &EMPTY_TYPE_MAP_PRESERVE,
        )
        .unwrap();
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
        let func = parse_pyfunction(
            &item,
            dummy_path(),
            &config,
            &HashMap::new(),
            &EMPTY_TYPE_MAP_PRESERVE,
        )
        .unwrap();
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
        let func = parse_pyfunction(
            &item,
            dummy_path(),
            &config,
            &HashMap::new(),
            &EMPTY_TYPE_MAP_PRESERVE,
        )
        .unwrap();
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
        let func = parse_pyfunction(
            &item,
            dummy_path(),
            &config,
            &HashMap::new(),
            &EMPTY_TYPE_MAP_PRESERVE,
        )
        .unwrap();
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
        let impl_map = build_impl_map(
            &[(std::path::PathBuf::from("lib.rs"), file.clone())],
            &no_features(),
        );
        let fields_map = StructFieldsMap::new();
        let type_alias_map = HashMap::new();
        let attrs_map = PyclassAttrsMap::new();
        let cx = make_cx(&config, &impl_map, &fields_map, &type_alias_map, &attrs_map);
        let modules = extract_modules_from_file(&file, Path::new("lib.rs"), &map, cx);
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
        let impl_map = build_impl_map(
            &[(std::path::PathBuf::from("lib.rs"), file.clone())],
            &no_features(),
        );
        let fields_map = StructFieldsMap::new();
        let type_alias_map = HashMap::new();
        let attrs_map = PyclassAttrsMap::new();
        let cx = make_cx(&config, &impl_map, &fields_map, &type_alias_map, &attrs_map);
        let modules = extract_modules_from_file(&file, Path::new("lib.rs"), &map, cx);
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
        let impl_map = build_impl_map(
            &[(std::path::PathBuf::from("lib.rs"), file.clone())],
            &no_features(),
        );
        let fields_map = StructFieldsMap::new();
        let type_alias_map = HashMap::new();
        let attrs_map = PyclassAttrsMap::new();
        let cx = make_cx(&config, &impl_map, &fields_map, &type_alias_map, &attrs_map);
        let modules = extract_modules_from_file(&file, Path::new("lib.rs"), &map, cx);
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
        let impl_map = build_impl_map(
            &[(std::path::PathBuf::from("lib.rs"), file.clone())],
            &no_features(),
        );
        let fields_map = StructFieldsMap::new();
        let type_alias_map = HashMap::new();
        let attrs_map = PyclassAttrsMap::new();
        let cx = make_cx(&config, &impl_map, &fields_map, &type_alias_map, &attrs_map);
        let modules = extract_modules_from_file(&file, Path::new("lib.rs"), &map, cx);
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
        let impl_map = build_impl_map(
            &[(std::path::PathBuf::from("lib.rs"), file.clone())],
            &no_features(),
        );
        let fields_map = StructFieldsMap::new();
        let type_alias_map = HashMap::new();
        let attrs_map = PyclassAttrsMap::new();
        let cx = make_cx(&config, &impl_map, &fields_map, &type_alias_map, &attrs_map);
        let modules = extract_modules_from_file(&file, Path::new("lib.rs"), &map, cx);
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
        let impl_map = build_impl_map(&[(path.to_path_buf(), file.clone())], &no_features());
        let fields_map = StructFieldsMap::new();
        let attrs_map = PyclassAttrsMap::new();
        let cx = make_cx(&config, &impl_map, &fields_map, &type_alias_map, &attrs_map);
        let modules = extract_modules_from_file(&file, path, &pyclass_map, cx);
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
fn abcd(m: &pyo3::Bound<'_, pyo3::PyModule>) -> pyo3::PyResult<()> {
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
        let impl_map = build_impl_map(&[(path.to_path_buf(), file.clone())], &no_features());
        let fields_map = StructFieldsMap::new();
        let attrs_map = PyclassAttrsMap::new();
        let cx = make_cx(&config, &impl_map, &fields_map, &type_alias_map, &attrs_map);
        let modules = extract_modules_from_file(&file, path, &pyclass_map, cx);
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
        let impl_map = build_impl_map(&[(path.to_path_buf(), file.clone())], &no_features());
        let fields_map = StructFieldsMap::new();
        let attrs_map = PyclassAttrsMap::new();
        let cx = make_cx(&config, &impl_map, &fields_map, &type_alias_map, &attrs_map);
        let modules = extract_modules_from_file(&file, path, &pyclass_map, cx);
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

    /// The primary scenario this module change was designed to fix:
    /// `#[pyclass]` lives in `lib.rs` and its `#[pymethods]` impl block lives in a
    /// separate file (`impl.rs`).  Both files are passed to `build_impl_map` together,
    /// so the methods must still appear on the collected class.
    #[test]
    fn pymethods_in_separate_file_are_resolved() {
        let lib_file = syn::parse_file(
            r#"
#[pyclass]
struct Edge {}

#[pymodule]
fn my_mod(m: &pyo3::Bound<'_, pyo3::PyModule>) -> pyo3::PyResult<()> {
    m.add_class::<Edge>()?;
    Ok(())
}
"#,
        )
        .unwrap();

        let impl_file = syn::parse_file(
            r#"
#[pymethods]
impl Edge {
    fn weight(&self) -> f64 { 0.0 }
    fn label(&self) -> String { String::new() }
}
"#,
        )
        .unwrap();

        let lib_path = std::path::PathBuf::from("lib.rs");
        let impl_path = std::path::PathBuf::from("impl.rs");

        let files = vec![(lib_path.clone(), lib_file.clone()), (impl_path, impl_file)];

        let config = Config::default();
        let impl_map = build_impl_map(&files, &no_features());
        let fields_map = StructFieldsMap::new();
        let type_alias_map = HashMap::new();
        let attrs_map = PyclassAttrsMap::new();
        let cx = make_cx(&config, &impl_map, &fields_map, &type_alias_map, &attrs_map);
        let modules = extract_modules_from_file(&lib_file, lib_path.as_path(), &HashMap::new(), cx);

        assert_eq!(modules.len(), 1, "one pymodule");
        let class = match &modules[0].items[0] {
            PyItem::Class(c) => c,
            other => panic!("expected PyItem::Class, got {other:?}"),
        };
        assert_eq!(class.name, "Edge");
        let method_names: Vec<&str> = class.methods.iter().map(|m| m.name.as_str()).collect();
        assert!(
            method_names.contains(&"weight"),
            "weight method from separate file missing: {method_names:?}"
        );
        assert!(
            method_names.contains(&"label"),
            "label method from separate file missing: {method_names:?}"
        );
    }

    // ── detect_method_kind ───────────────────────────────────────────────────

    #[test]
    fn method_kind_new_detected() {
        let item: syn::ImplItemFn = syn::parse_quote! {
            #[new]
            fn new() -> Self { Self {} }
        };
        assert!(
            matches!(detect_method_kind(&item.attrs, "new"), MethodKind::New),
            "expected MethodKind::New"
        );
    }

    #[test]
    fn method_kind_getter_uses_fn_name_by_default() {
        let item: syn::ImplItemFn = syn::parse_quote! {
            #[getter]
            fn value(&self) -> i32 { 0 }
        };
        assert_eq!(
            detect_method_kind(&item.attrs, "value"),
            MethodKind::Getter("value".to_string())
        );
    }

    #[test]
    fn method_kind_getter_uses_rename_arg() {
        let item: syn::ImplItemFn = syn::parse_quote! {
            #[getter(count)]
            fn internal_count(&self) -> i32 { 0 }
        };
        assert_eq!(
            detect_method_kind(&item.attrs, "internal_count"),
            MethodKind::Getter("count".to_string())
        );
    }

    #[test]
    fn method_kind_setter_uses_fn_name_by_default() {
        let item: syn::ImplItemFn = syn::parse_quote! {
            #[setter]
            fn value(&mut self, v: i32) {}
        };
        assert_eq!(
            detect_method_kind(&item.attrs, "value"),
            MethodKind::Setter("value".to_string())
        );
    }

    #[test]
    fn method_kind_setter_strips_set_prefix_like_pyo3() {
        let item: syn::ImplItemFn = syn::parse_quote! {
            #[setter]
            fn set_num(&mut self, number: f64) {}
        };
        assert_eq!(
            detect_method_kind(&item.attrs, "set_num"),
            MethodKind::Setter("num".to_string())
        );
    }

    #[test]
    fn method_kind_setter_uses_rename_arg() {
        let item: syn::ImplItemFn = syn::parse_quote! {
            #[setter(count)]
            fn set_internal(&mut self, v: i32) {}
        };
        assert_eq!(
            detect_method_kind(&item.attrs, "set_internal"),
            MethodKind::Setter("count".to_string())
        );
    }

    #[test]
    fn method_kind_classmethod_detected() {
        let item: syn::ImplItemFn = syn::parse_quote! {
            #[classmethod]
            fn create(cls: &pyo3::Bound<'_, pyo3::types::PyType>) -> Self { Self {} }
        };
        assert!(
            matches!(detect_method_kind(&item.attrs, "create"), MethodKind::Class),
            "expected MethodKind::Class"
        );
    }

    // ── extract_doc ──────────────────────────────────────────────────────────

    #[test]
    fn extract_doc_single_line_strips_leading_space() {
        let item: syn::ItemFn = syn::parse_quote! {
            /// Hello, world!
            fn foo() {}
        };
        let doc = extract_doc(&item.attrs);
        assert_eq!(doc, vec!["Hello, world!"]);
    }

    #[test]
    fn extract_doc_multi_line_collects_all_lines() {
        let item: syn::ItemFn = syn::parse_quote! {
            /// First line.
            /// Second line.
            /// Third line.
            fn foo() {}
        };
        let doc = extract_doc(&item.attrs);
        assert_eq!(doc, vec!["First line.", "Second line.", "Third line."]);
    }

    #[test]
    fn extract_doc_empty_when_no_doc_comment() {
        let item: syn::ItemFn = syn::parse_quote! {
            fn foo() {}
        };
        assert_eq!(extract_doc(&item.attrs), Vec::<String>::new());
    }

    // ── nested #[pymodule] in Style A ────────────────────────────────────────

    #[test]
    fn nested_pymodule_in_style_a_becomes_submodule_item() {
        let file = syn::parse_file(
            r#"
#[pymodule]
mod outer {
    #[pymodule]
    mod inner {
        #[pyfunction]
        fn greet() -> String { String::new() }
    }
}
"#,
        )
        .unwrap();
        let path = dummy_path();
        let config = Config::default();
        let impl_map = build_impl_map(&[(path.to_path_buf(), file.clone())], &no_features());
        let fields_map = StructFieldsMap::new();
        let type_alias_map = HashMap::new();
        let attrs_map = PyclassAttrsMap::new();
        let cx = make_cx(&config, &impl_map, &fields_map, &type_alias_map, &attrs_map);
        let modules = extract_modules_from_file(&file, path, &HashMap::new(), cx);

        assert_eq!(modules.len(), 1, "one top-level module");
        assert_eq!(modules[0].name, "outer");
        assert_eq!(modules[0].items.len(), 1, "one item: the nested module");
        match &modules[0].items[0] {
            PyItem::Module(sub) => {
                assert_eq!(sub.name, "inner");
                assert_eq!(sub.items.len(), 1, "inner has one function");
            }
            other => panic!("expected PyItem::Module, got {other:?}"),
        }
    }

    // ── Style B: m.add("name", value) → module-level constant ──────────────
    #[test]
    fn style_b_add_constant_infers_str_from_env_macro() {
        let file = syn::parse_file(
            r#"
#[pymodule]
fn my_mod(m: &pyo3::Bound<'_, pyo3::PyModule>) -> pyo3::PyResult<()> {
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}
"#,
        )
        .unwrap();
        let path = dummy_path();
        let config = Config::default();
        let impl_map = build_impl_map(&[(path.to_path_buf(), file.clone())], &no_features());
        let fields_map = StructFieldsMap::new();
        let type_alias_map = HashMap::new();
        let attrs_map = PyclassAttrsMap::new();
        let cx = make_cx(&config, &impl_map, &fields_map, &type_alias_map, &attrs_map);
        let modules = extract_modules_from_file(&file, path, &HashMap::new(), cx);
        assert_eq!(modules.len(), 1);
        assert_eq!(modules[0].items.len(), 1);
        match &modules[0].items[0] {
            PyItem::Constant(c) => {
                assert_eq!(c.name, "__version__");
                assert_eq!(c.py_type, "str");
            }
            other => panic!("expected PyItem::Constant, got {:?}", other),
        }
    }

    #[test]
    fn style_b_add_constant_infers_type_from_literal() {
        let file = syn::parse_file(
            r#"
#[pymodule]
fn my_mod(m: &pyo3::Bound<'_, pyo3::PyModule>) -> pyo3::PyResult<()> {
    m.add("count", 42)?;
    m.add("pi", 3.14)?;
    m.add("flag", true)?;
    Ok(())
}
"#,
        )
        .unwrap();
        let path = dummy_path();
        let config = Config::default();
        let impl_map = build_impl_map(&[(path.to_path_buf(), file.clone())], &no_features());
        let fields_map = StructFieldsMap::new();
        let type_alias_map = HashMap::new();
        let attrs_map = PyclassAttrsMap::new();
        let cx = make_cx(&config, &impl_map, &fields_map, &type_alias_map, &attrs_map);
        let modules = extract_modules_from_file(&file, path, &HashMap::new(), cx);
        assert_eq!(modules.len(), 1);
        assert_eq!(modules[0].items.len(), 3);
        match &modules[0].items[0] {
            PyItem::Constant(c) => {
                assert_eq!(c.name, "count");
                assert_eq!(c.py_type, "int");
            }
            other => panic!("expected Constant count, got {:?}", other),
        }
        match &modules[0].items[1] {
            PyItem::Constant(c) => {
                assert_eq!(c.name, "pi");
                assert_eq!(c.py_type, "float");
            }
            other => panic!("expected Constant pi, got {:?}", other),
        }
        match &modules[0].items[2] {
            PyItem::Constant(c) => {
                assert_eq!(c.name, "flag");
                assert_eq!(c.py_type, "bool");
            }
            other => panic!("expected Constant flag, got {:?}", other),
        }
    }

    // ── Style B: add_function with missing function ──────────────────────────

    #[test]
    fn style_b_add_function_for_missing_fn_is_skipped() {
        let file = syn::parse_file(
            r#"
#[pymodule]
fn my_mod(m: &pyo3::Bound<'_, pyo3::PyModule>) -> pyo3::PyResult<()> {
    m.add_function(pyo3::wrap_pyfunction!(missing_fn, m)?)?;
    Ok(())
}
"#,
        )
        .unwrap();
        let path = dummy_path();
        let config = Config::default();
        let impl_map = build_impl_map(&[(path.to_path_buf(), file.clone())], &no_features());
        let fields_map = StructFieldsMap::new();
        let type_alias_map = HashMap::new();
        let attrs_map = PyclassAttrsMap::new();
        let cx = make_cx(&config, &impl_map, &fields_map, &type_alias_map, &attrs_map);
        let modules = extract_modules_from_file(&file, path, &HashMap::new(), cx);
        assert_eq!(modules.len(), 1, "module should still be collected");
        assert_eq!(
            modules[0].items.len(),
            0,
            "missing function should be silently skipped"
        );
    }

    // ── build_type_alias_map: nested mod ────────────────────────────────────

    #[test]
    fn build_type_alias_map_inside_nested_mod() {
        let file = syn::parse_file(
            r#"
mod inner {
    type Coord = (f32, f32);
}
"#,
        )
        .unwrap();
        let path = std::path::PathBuf::from("lib.rs");
        let map = build_type_alias_map(&[(path, file)], &no_features());
        assert!(
            map.contains_key("Coord"),
            "type alias inside nested mod should be collected"
        );
    }

    // ── #[pyo3(get)] / #[pyo3(set)] struct field properties ─────────────────

    /// A `#[pyclass]` struct with `#[pyo3(get)]` fields must produce `@property` stubs.
    #[test]
    fn pyo3_get_fields_produce_property_stubs() {
        let file = syn::parse_file(
            r#"
#[pyclass(name = "TableCellValue")]
struct PyTableCellValue {
    #[pyo3(get)]
    pub text: Option<String>,
    #[pyo3(get)]
    pub merged_left: bool,
    #[pyo3(get)]
    pub merged_top: bool,
}

#[pymodule]
fn my_mod(m: &pyo3::Bound<'_, pyo3::PyModule>) -> pyo3::PyResult<()> {
    m.add_class::<PyTableCellValue>()?;
    Ok(())
}
"#,
        )
        .unwrap();
        let path = Path::new("lib.rs");
        let config = Config::default();
        let mut pyclass_map = HashMap::new();
        pyclass_map.insert("PyTableCellValue".to_string(), "TableCellValue".to_string());
        let impl_map = build_impl_map(&[(path.to_path_buf(), file.clone())], &no_features());
        let struct_fields_map =
            build_struct_fields_map(&[(path.to_path_buf(), file.clone())], &no_features());
        let type_alias_map = HashMap::new();
        let attrs_map = PyclassAttrsMap::new();
        let cx = make_cx(
            &config,
            &impl_map,
            &struct_fields_map,
            &type_alias_map,
            &attrs_map,
        );
        let modules = extract_modules_from_file(&file, path, &pyclass_map, cx);

        let class = match &modules[0].items[0] {
            PyItem::Class(c) => c,
            other => panic!("expected PyItem::Class, got {other:?}"),
        };
        assert_eq!(class.name, "TableCellValue");

        let prop_names: Vec<&str> = class
            .methods
            .iter()
            .filter_map(|m| match &m.kind {
                MethodKind::Getter(p) => Some(p.as_str()),
                _ => None,
            })
            .collect();
        assert!(
            prop_names.contains(&"text"),
            "text getter missing: {prop_names:?}"
        );
        assert!(
            prop_names.contains(&"merged_left"),
            "merged_left getter missing: {prop_names:?}"
        );
        assert!(
            prop_names.contains(&"merged_top"),
            "merged_top getter missing: {prop_names:?}"
        );
    }

    /// `#[pyo3(get, set)]` produces both a getter and a setter for the same property.
    #[test]
    fn pyo3_get_set_field_produces_getter_and_setter() {
        let file = syn::parse_file(
            r#"
#[pyclass]
struct Counter {
    #[pyo3(get, set)]
    pub value: i32,
}

#[pymodule]
fn my_mod(m: &pyo3::Bound<'_, pyo3::PyModule>) -> pyo3::PyResult<()> {
    m.add_class::<Counter>()?;
    Ok(())
}
"#,
        )
        .unwrap();
        let path = Path::new("lib.rs");
        let config = Config::default();
        let impl_map = build_impl_map(&[(path.to_path_buf(), file.clone())], &no_features());
        let struct_fields_map =
            build_struct_fields_map(&[(path.to_path_buf(), file.clone())], &no_features());
        let type_alias_map = HashMap::new();
        let attrs_map = PyclassAttrsMap::new();
        let cx = make_cx(
            &config,
            &impl_map,
            &struct_fields_map,
            &type_alias_map,
            &attrs_map,
        );
        let modules = extract_modules_from_file(&file, path, &HashMap::new(), cx);

        let class = match &modules[0].items[0] {
            PyItem::Class(c) => c,
            other => panic!("expected PyItem::Class, got {other:?}"),
        };
        let has_getter = class
            .methods
            .iter()
            .any(|m| matches!(&m.kind, MethodKind::Getter(p) if p == "value"));
        let has_setter = class
            .methods
            .iter()
            .any(|m| matches!(&m.kind, MethodKind::Setter(p) if p == "value"));
        assert!(has_getter, "getter for `value` missing");
        assert!(has_setter, "setter for `value` missing");
    }

    /// `#[pyo3(get)]` fields appear before `#[pymethods]` methods in the collected list.
    #[test]
    fn pyo3_get_fields_appear_before_pymethods() {
        let file = syn::parse_file(
            r#"
#[pyclass]
struct Foo {
    #[pyo3(get)]
    pub x: i32,
}

#[pymethods]
impl Foo {
    fn do_thing(&self) -> i32 { self.x }
}

#[pymodule]
fn my_mod(m: &pyo3::Bound<'_, pyo3::PyModule>) -> pyo3::PyResult<()> {
    m.add_class::<Foo>()?;
    Ok(())
}
"#,
        )
        .unwrap();
        let path = Path::new("lib.rs");
        let config = Config::default();
        let impl_map = build_impl_map(&[(path.to_path_buf(), file.clone())], &no_features());
        let struct_fields_map =
            build_struct_fields_map(&[(path.to_path_buf(), file.clone())], &no_features());
        let type_alias_map = HashMap::new();
        let attrs_map = PyclassAttrsMap::new();
        let cx = make_cx(
            &config,
            &impl_map,
            &struct_fields_map,
            &type_alias_map,
            &attrs_map,
        );
        let modules = extract_modules_from_file(&file, path, &HashMap::new(), cx);
        let class = match &modules[0].items[0] {
            PyItem::Class(c) => c,
            other => panic!("expected PyItem::Class, got {other:?}"),
        };
        assert!(
            class.methods.len() >= 2,
            "expected at least getter + do_thing"
        );
        assert!(
            matches!(&class.methods[0].kind, MethodKind::Getter(p) if p == "x"),
            "first method should be getter for x, got: {:?}",
            class.methods[0].kind
        );
    }

    /// `#[pyo3(get)]` fields on a struct defined in a *different* file from the
    /// `#[pymodule]` are still resolved via [`build_struct_fields_map`].
    /// This is the primary motivation for the fourth crate-wide pass.
    #[test]
    fn pyo3_get_fields_resolved_across_files() {
        let struct_src = r#"
#[pyclass]
pub struct Sensor {
    #[pyo3(get)]
    pub temperature: f64,
    #[pyo3(get, set)]
    pub label: String,
}
"#;
        let mod_src = r#"
#[pymodule]
fn my_mod(m: &pyo3::Bound<'_, pyo3::PyModule>) -> pyo3::PyResult<()> {
    m.add_class::<Sensor>()?;
    Ok(())
}
"#;
        let struct_path = std::path::PathBuf::from("sensors.rs");
        let mod_path = std::path::PathBuf::from("lib.rs");
        let struct_file = syn::parse_file(struct_src).unwrap();
        let mod_file = syn::parse_file(mod_src).unwrap();

        let files = vec![
            (struct_path.clone(), struct_file.clone()),
            (mod_path.clone(), mod_file.clone()),
        ];

        let config = Config::default();
        let impl_map = build_impl_map(&files, &no_features());
        let struct_fields_map = build_struct_fields_map(&files, &no_features());
        let type_alias_map = HashMap::new();
        let attrs_map = PyclassAttrsMap::new();
        let cx = make_cx(
            &config,
            &impl_map,
            &struct_fields_map,
            &type_alias_map,
            &attrs_map,
        );

        // Parse only the module file — struct definition lives in a separate file.
        let modules = extract_modules_from_file(&mod_file, mod_path.as_path(), &HashMap::new(), cx);

        assert_eq!(modules.len(), 1, "one pymodule");
        let class = match &modules[0].items[0] {
            PyItem::Class(c) => c,
            other => panic!("expected PyItem::Class, got {other:?}"),
        };
        assert_eq!(class.name, "Sensor");

        let getter_names: Vec<&str> = class
            .methods
            .iter()
            .filter_map(|m| match &m.kind {
                MethodKind::Getter(p) => Some(p.as_str()),
                _ => None,
            })
            .collect();
        assert!(
            getter_names.contains(&"temperature"),
            "temperature getter missing; got: {getter_names:?}"
        );
        assert!(
            getter_names.contains(&"label"),
            "label getter missing; got: {getter_names:?}"
        );

        let has_label_setter = class
            .methods
            .iter()
            .any(|m| matches!(&m.kind, MethodKind::Setter(p) if p == "label"));
        assert!(has_label_setter, "label setter missing");
    }

    // ── build_pyclass_name_map: #[pyclass] enum ─────────────────────────────

    #[test]
    fn build_pyclass_name_map_collects_enum() {
        let file = syn::parse_file(
            r#"
#[pyclass(name = "Color")]
enum PyColor { Red, Green, Blue }
"#,
        )
        .unwrap();
        let path = std::path::PathBuf::from("lib.rs");
        let map = build_pyclass_name_map(&[(path, file)], &no_features());
        assert_eq!(
            map.get("PyColor"),
            Some(&"Color".to_string()),
            "renamed enum should appear in pyclass name map"
        );
    }

    // ── extract_wrap_pyfunction_name ─────────────────────────────────────────

    #[test]
    fn extract_wrap_pyfunction_name_simple() {
        let expr: syn::Expr = syn::parse_quote! {
            wrap_pyfunction!(foo, m)
        };
        assert_eq!(
            extract_wrap_pyfunction_name(Some(&expr)),
            Some("foo".to_string())
        );
    }

    #[test]
    fn extract_wrap_pyfunction_name_full_path() {
        let expr: syn::Expr = syn::parse_quote! {
            wrap_pyfunction!(crate::module::foo, m)
        };
        assert_eq!(
            extract_wrap_pyfunction_name(Some(&expr)),
            Some("crate::module::foo".to_string())
        );
    }

    #[test]
    fn extract_wrap_pyfunction_name_without_module_arg() {
        let expr: syn::Expr = syn::parse_quote! {
            wrap_pyfunction!(foo)
        };
        assert_eq!(
            extract_wrap_pyfunction_name(Some(&expr)),
            Some("foo".to_string())
        );
    }

    #[test]
    fn extract_wrap_pyfunction_name_nested_path() {
        let expr: syn::Expr = syn::parse_quote! {
            wrap_pyfunction!(crate::a::b::c::function_name, m)
        };
        assert_eq!(
            extract_wrap_pyfunction_name(Some(&expr)),
            Some("crate::a::b::c::function_name".to_string())
        );
    }

    #[test]
    fn extract_wrap_pyfunction_name_try_expr() {
        let expr: syn::Expr = syn::parse_quote! {
            wrap_pyfunction!(foo)?
        };
        assert_eq!(
            extract_wrap_pyfunction_name(Some(&expr)),
            Some("foo".to_string())
        );
    }

    #[test]
    fn extract_wrap_pyfunction_name_none_for_other_macro() {
        let expr: syn::Expr = syn::parse_quote! {
            other_macro!(foo)
        };
        assert_eq!(extract_wrap_pyfunction_name(Some(&expr)), None);
    }

    #[test]
    fn extract_wrap_pyfunction_name_none_for_non_macro() {
        let expr: syn::Expr = syn::parse_quote! {
            foo
        };
        assert_eq!(extract_wrap_pyfunction_name(Some(&expr)), None);
    }

    // ── build_pyfunction_map ────────────────────────────────────────────────────

    #[test]
    fn build_pyfunction_map_collects_single_function() {
        let file = syn::parse_file(
            r#"
#[pyfunction]
fn foo(x: i32) -> i32 { x + 1 }
"#,
        )
        .unwrap();
        let path = std::path::PathBuf::from("lib.rs");
        let map = build_pyfunction_map(&[(path, file)], &no_features());

        assert_eq!(map.len(), 1);
        assert!(map.contains_key("foo"));
        let func = map.get("foo").unwrap();
        assert_eq!(func.rust_name, "foo");
    }

    #[test]
    fn build_pyfunction_map_collects_multiple_functions() {
        let file = syn::parse_file(
            r#"
#[pyfunction]
fn foo(x: i32) -> i32 { x + 1 }

#[pyfunction]
fn bar(y: String) -> String { y }
"#,
        )
        .unwrap();
        let path = std::path::PathBuf::from("lib.rs");
        let map = build_pyfunction_map(&[(path, file)], &no_features());

        assert_eq!(map.len(), 2);
        assert!(map.contains_key("foo"));
        assert!(map.contains_key("bar"));
    }

    #[test]
    fn build_pyfunction_map_collects_from_multiple_files() {
        let file1 = syn::parse_file(
            r#"
#[pyfunction]
fn foo(x: i32) -> i32 { x + 1 }
"#,
        )
        .unwrap();
        let file2 = syn::parse_file(
            r#"
#[pyfunction]
fn bar(y: String) -> String { y }
"#,
        )
        .unwrap();
        let path1 = std::path::PathBuf::from("foo.rs");
        let path2 = std::path::PathBuf::from("bar.rs");
        let map = build_pyfunction_map(&[(path1, file1), (path2, file2)], &no_features());

        assert_eq!(map.len(), 2);
        assert!(map.contains_key("foo"));
        assert!(map.contains_key("bar"));
    }

    #[test]
    fn build_pyfunction_map_respects_cfg_feature() {
        let file = syn::parse_file(
            r#"
#[cfg(feature = "enabled")]
#[pyfunction]
fn enabled_func() -> i32 { 1 }

#[cfg(feature = "disabled")]
#[pyfunction]
fn disabled_func() -> i32 { 0 }
"#,
        )
        .unwrap();
        let path = std::path::PathBuf::from("lib.rs");
        let enabled_features = vec!["enabled".to_string()];
        let map = build_pyfunction_map(&[(path, file)], &enabled_features);

        assert_eq!(map.len(), 1);
        assert!(map.contains_key("enabled_func"));
        assert!(!map.contains_key("disabled_func"));
    }

    #[test]
    fn build_pyfunction_map_collects_from_nested_modules() {
        let file = syn::parse_file(
            r#"
mod inner {
    #[pyfunction]
    pub fn inner_func(x: i32) -> i32 { x + 1 }
}

#[pyfunction]
fn outer_func() -> i32 { 0 }
"#,
        )
        .unwrap();
        let path = std::path::PathBuf::from("lib.rs");
        let map = build_pyfunction_map(&[(path, file)], &no_features());

        assert_eq!(map.len(), 2);
        assert!(map.contains_key("inner_func"));
        assert!(map.contains_key("outer_func"));
    }

    #[test]
    fn build_pyfunction_map_preserves_pyfunction_with_name() {
        let file = syn::parse_file(
            r#"
#[pyfunction]
#[pyo3(name = "python_name")]
fn rust_name(x: i32) -> i32 { x + 1 }
"#,
        )
        .unwrap();
        let path = std::path::PathBuf::from("lib.rs");
        let map = build_pyfunction_map(&[(path, file)], &no_features());

        assert_eq!(map.len(), 1);
        let func = map.get("rust_name").unwrap();
        assert_eq!(func.rust_name, "rust_name");
        assert_eq!(func.name, "python_name");
    }

    #[test]
    fn build_pyfunction_map_ignores_non_pyfunction() {
        let file = syn::parse_file(
            r#"
#[pyfunction]
fn is_pyfunction() -> i32 { 1 }

fn not_pyfunction() -> i32 { 0 }

#[pyclass]
struct MyClass {}
"#,
        )
        .unwrap();
        let path = std::path::PathBuf::from("lib.rs");
        let map = build_pyfunction_map(&[(path, file)], &no_features());

        assert_eq!(map.len(), 1);
        assert!(map.contains_key("is_pyfunction"));
        assert!(!map.contains_key("not_pyfunction"));
    }
}
