use crate::collector::{
    ExtendsSpec, MethodKind, ParamKind, PyClass, PyConstant, PyFunction, PyItem, PyMethod,
    PyModule, PyParam, PyType,
};
use crate::config::{
    Config, FallbackStrategy, OverrideEntry, RenderPolicy, merge_type_map_into_known_classes,
};
use crate::stub_constants::{AUTO_GENERATED_BANNER, TYPING_IMPORT_LINE};
use crate::type_map::{self, TypeMapping};
use anyhow::{Result, bail};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use syn::{ReturnType, Type, TypeParamBound};

// ── Public entry point ───────────────────────────────────────────────────────

/// Generate .pyi content for the given modules using a pre-computed Rust→Python class name map.
/// Used when emitting multiple stubs so that cross-module type references resolve correctly.
///
/// When `cross_import` is `Some((current_stub_module, rust_class_to_defining_module))`, types that
/// reference `#[pyclass]` types defined in another Python module cause `from ... import ...` lines
/// to be prepended after `import typing as t` and optional `from pathlib import Path` when needed.
pub fn generate_with_known_classes(
    modules: &[PyModule],
    config: &Config,
    known_classes: &HashMap<String, String>,
    pre_warnings: &[String],
    cross_import: Option<(&str, &HashMap<String, String>)>,
) -> Result<String> {
    let policy = config.render_policy();
    let logical_module = cross_import.map(|(s, _)| s.to_string()).unwrap_or_default();
    let (known_classes, type_map_warnings) =
        merge_type_map_into_known_classes(known_classes, &config.type_map);
    let mut warnings = pre_warnings.to_vec();
    warnings.extend(type_map_warnings);
    let mut ctx = GenCtx {
        config,
        policy,
        needs_any: false,
        needs_optional: false,
        needs_self_import: false,
        needs_path_import: false,
        needs_union: false,
        warnings,
        current_self_type: None,
        known_classes,
        current_stub_module: cross_import.map(|(s, _)| s.to_string()),
        class_rust_to_module: cross_import.map(|(_, m)| (*m).clone()),
        cross_module_imports: BTreeMap::new(),
        logical_module,
    };

    let mut body = String::new();
    for module in modules {
        if ctx.current_stub_module.is_none() {
            ctx.logical_module = module.name.clone();
        }
        ctx.gen_module(module, &mut body, 0)?;
    }

    let mut out = String::new();

    // Header
    if config.output.add_header {
        out.push_str(AUTO_GENERATED_BANNER);
    }

    // `from __future__ import annotations` enables lazy annotation evaluation, which
    // is required when py < 3.11: class names are used as forward references in return
    // types (e.g. `def from_bytes(...) -> PdfDocument`).
    if ctx.policy.future_annotations {
        out.push_str("from __future__ import annotations\n\n");
    }

    // Always emit `import typing as t` so `[[add_content]]` with `after-import-typing` has a
    // stable anchor; users may run `ruff check --select F401 --fix` to drop unused imports.
    out.push_str(TYPING_IMPORT_LINE);
    out.push_str("\n\n");

    if ctx.needs_path_import {
        out.push_str("from pathlib import Path\n\n");
    }

    // Same-package imports so referenced classes from other .pyi files resolve under Pyright/mypy.
    if !ctx.cross_module_imports.is_empty() {
        for (mod_path, names) in &ctx.cross_module_imports {
            let mut names: Vec<_> = names.iter().cloned().collect();
            names.sort();
            out.push_str(&format!(
                "from {} import {}\n\n",
                mod_path,
                names.join(", ")
            ));
        }
    }

    out.push_str(&body);

    // Print warnings to stderr
    for w in &ctx.warnings {
        eprintln!("warning: {w}");
    }

    Ok(out)
}

/// Test-only helper: [`collect_class_names`] then [`generate_with_known_classes`] without cross-import context.
///
/// Must live in `generator` (not in `mod tests`): `collector::parse` tests need `crate::generator::generate`,
/// and a nested `tests` module is private to this file.
#[cfg(test)]
pub(crate) fn generate(modules: &[PyModule], config: &Config) -> Result<String> {
    let (known_classes, pre_warnings) = collect_class_names(modules);
    generate_with_known_classes(modules, config, &known_classes, &pre_warnings, None)
}

// ── Generation context ───────────────────────────────────────────────────────

/// Restores `GenCtx::current_self_type` to `None` when dropped so that early returns
/// (e.g. via `?`) from `gen_method` do not leave the context in a stale state.
struct RestoreCurrentSelfTypeGuard(*mut Option<String>);

impl Drop for RestoreCurrentSelfTypeGuard {
    fn drop(&mut self) {
        // SAFETY: the pointer is taken from `&mut self.current_self_type` in `gen_method`
        // and is only used while that method is running; the guard is dropped on return.
        unsafe {
            *self.0 = None;
        }
    }
}

struct GenCtx<'a> {
    config: &'a Config,
    /// Version-specific rendering decisions derived from config at the start of generation.
    policy: RenderPolicy,
    needs_any: bool,
    needs_optional: bool,
    /// Whether any mapping emitted the `Self` keyword (py ≥ 3.11, PEP 673).
    needs_self_import: bool,
    /// Whether any mapping used pathlib.Path (Rust PathBuf/Path → Path | str).
    needs_path_import: bool,
    /// Whether any mapping used `t.Union[...]` (py < 3.10 style, e.g. t.Union[Path, str]).
    needs_union: bool,
    warnings: Vec<String>,
    /// Set to the Python class name while generating methods for that class,
    /// so that Rust `Self` return types resolve correctly.
    current_self_type: Option<String>,
    /// Maps each `#[pyclass]` Rust struct name to its Python-visible class name, collected
    /// before generation starts.  This allows return types like `-> PyResult<PyPageIterator>`
    /// (where the Rust struct is `PyPageIterator` but the Python name is `PageIterator`) to
    /// resolve correctly instead of falling back to `Any`.
    known_classes: HashMap<String, String>,
    /// Dotted Python module for this stub file (e.g. `abcd.ee`); set when emitting a split layout.
    current_stub_module: Option<String>,
    /// Rust struct name → dotted module where that class is defined.
    class_rust_to_module: Option<HashMap<String, String>>,
    /// Dotted defining module → Python class names to import into this stub.
    cross_module_imports: BTreeMap<String, BTreeSet<String>>,
    /// Dotted Python module name for **this generated stub file** — same as [`Self::current_stub_module`]
    /// when the CLI passes cross-import context (always true for normal runs). It is the
    /// [`PyModule::name`] of the slice passed to [`generate_with_known_classes`]: the `#[pymodule]`
    /// root (e.g. `pkg`) for `pkg.pyi`, or the **exact** `#[pyclass(module = "...")]` string (e.g.
    /// `pkg.abc`) when that class is emitted into a separate stub. It is not “always the pymodule”:
    /// use whichever module owns the `.pyi` where the class appears. When [`Self::current_stub_module`]
    /// is `None` (tests), this is set from each top-level [`PyModule::name`].
    logical_module: String,
}

impl<'a> GenCtx<'a> {
    fn resolve_type(&mut self, py_type: &PyType, location: &str) -> Result<String> {
        // User override takes priority
        if let Some(ov) = &py_type.override_str {
            return Ok(ov.clone());
        }

        let mapping = type_map::map_type(
            &py_type.rust_type,
            &self.policy,
            self.current_self_type.as_deref(),
            &self.known_classes,
        );
        self.absorb_mapping(&mapping, location)?;
        if let (Some(cur), Some(loc_map)) = (&self.current_stub_module, &self.class_rust_to_module)
        {
            collect_pyclass_refs_in_type(
                &py_type.rust_type,
                &self.known_classes,
                loc_map,
                cur,
                &mut self.cross_module_imports,
            );
        }
        Ok(mapping.py_type)
    }

    fn absorb_mapping(&mut self, m: &TypeMapping, location: &str) -> Result<()> {
        if m.needs_any {
            self.needs_any = true;
        }
        if m.needs_optional {
            self.needs_optional = true;
        }
        if m.needs_self_import {
            self.needs_self_import = true;
        }
        if m.needs_path_import {
            self.needs_path_import = true;
        }
        if m.needs_union {
            self.needs_union = true;
        }
        if m.is_unknown {
            match self.config.fallback.strategy {
                FallbackStrategy::Any => {
                    self.warnings.push(format!(
                        "unknown type at `{location}` — falling back to `Any`"
                    ));
                }
                FallbackStrategy::Error => {
                    bail!("unknown type at `{location}` and fallback strategy is `error`");
                }
                FallbackStrategy::Skip => {
                    // Caller checks is_unknown and skips the item
                }
            }
        }
        Ok(())
    }

    /// Remove optional trailing stub ellipsis (`...`) from an override line (repeat while suffix matches).
    fn normalize_override_header_line(stub: &str) -> String {
        let mut s = stub.trim().to_string();
        loop {
            let trimmed_end = s.trim_end();
            if let Some(prefix) = trimmed_end.strip_suffix("...") {
                s = prefix.trim_end().to_string();
            } else {
                break;
            }
        }
        s
    }

    /// Emit `[[override]]` for a module item.
    ///
    /// For a **single-line** `def ...:` or `class ...:` override, Rust `///` docs on that item are
    /// emitted as a Python docstring under the header; if there is no doc, `...` is inserted so the
    /// stub stays valid for formatters. Trailing `...` on the override line itself is stripped.
    ///
    /// **Multiline** overrides (or constants / nested modules) are written almost verbatim
    /// (trimmed, trailing `...` stripped from the whole block only).
    fn emit_override(
        &self,
        ov: &OverrideEntry,
        item: &PyItem,
        out: &mut String,
        indent: usize,
    ) -> Result<()> {
        let merge_rust_doc_on_single_line = matches!(item, PyItem::Function(_) | PyItem::Class(_));
        let doc: &[String] = match item {
            PyItem::Function(f) => &f.doc,
            PyItem::Class(c) => &c.doc,
            _ => &[],
        };
        self.emit_override_stub(ov, doc, out, indent, merge_rust_doc_on_single_line)
    }

    /// Shared `[[override]]` body emission for top-level items and class methods.
    ///
    /// When `merge_rust_doc_on_single_line` is true and `stub` is a single line, Rust `///` docs are
    /// merged under the header; otherwise the stub block is written verbatim (after normalizing
    /// trailing `...`).
    fn emit_override_stub(
        &self,
        ov: &OverrideEntry,
        doc: &[String],
        out: &mut String,
        indent: usize,
        merge_rust_doc_on_single_line: bool,
    ) -> Result<()> {
        let pad = "    ".repeat(indent);
        let trimmed = ov.stub.trim();
        let rich = merge_rust_doc_on_single_line && !trimmed.contains('\n');

        if rich {
            let header = Self::normalize_override_header_line(trimmed);
            out.push_str(&pad);
            out.push_str(&header);
            out.push('\n');

            if !doc.is_empty() {
                self.gen_docstring(doc, out, indent + 1);
            } else {
                out.push_str(&format!("{pad}    ...\n"));
            }
            out.push('\n');
        } else {
            let body = Self::normalize_override_header_line(trimmed);
            out.push_str(&format!("{pad}{}\n\n", body));
        }
        Ok(())
    }

    /// Whether `[[override]]` `item` targets this `#[pymethods]` entry (any kind: instance, static,
    /// class, getter/setter, or `#[new]`).
    fn method_override_entry_matches(
        o_item: &str,
        logical_mod: &str,
        class: &PyClass,
        method: &PyMethod,
    ) -> bool {
        let matches_pair = |cls: &str, meth: &str| {
            let full = format!("{logical_mod}::{cls}::{meth}");
            o_item == full || o_item.ends_with(&format!("::{cls}::{meth}"))
        };
        if matches_pair(&class.rust_name, &method.rust_ident) {
            return true;
        }
        if matches_pair(&class.name, &method.name) {
            return true;
        }
        if matches!(method.kind, MethodKind::New) {
            if matches_pair(&class.rust_name, "__init__") {
                return true;
            }
            if matches_pair(&class.name, "__init__") {
                return true;
            }
        }
        false
    }

    fn find_method_override<'b>(
        &'b self,
        class: &PyClass,
        method: &PyMethod,
    ) -> Option<&'b OverrideEntry> {
        self.config.overrides.iter().find(|o| {
            Self::method_override_entry_matches(&o.item, &self.logical_module, class, method)
        })
    }

    // ── Module ───────────────────────────────────────────────────────────────

    fn gen_module(&mut self, module: &PyModule, out: &mut String, indent: usize) -> Result<()> {
        let pad = "    ".repeat(indent);

        if !module.doc.is_empty() {
            out.push_str(&format!("{pad}# Module: {}\n", module.name));
        }

        for item in &module.items {
            // Check for manual override
            let override_stub = self
                .config
                .overrides
                .iter()
                .find(|o| module_level_override_matches(&o.item, item));
            if let Some(ov) = override_stub {
                self.emit_override(ov, item, out, indent)?;
                continue;
            }

            match item {
                PyItem::Constant(c) => self.gen_constant(c, out, indent)?,
                PyItem::Function(f) => self.gen_function(f, out, indent)?,
                PyItem::Class(c) => self.gen_class(c, out, indent)?,
                PyItem::Module(m) => {
                    // Submodule: emit a class stub as a namespace approximation
                    out.push_str(&format!("{pad}class {}:\n", m.name));
                    if !m.doc.is_empty() {
                        self.gen_docstring(&m.doc, out, indent + 1);
                    }
                    self.gen_module(m, out, indent + 1)?;
                    out.push('\n');
                }
            }
        }
        Ok(())
    }

    // ── Module-level constant (m.add("name", value)) ─────────────────────────

    /// Emits `name: t.Final[py_type]` (`import typing as t` is always emitted in the stub header).
    fn gen_constant(&mut self, c: &PyConstant, out: &mut String, indent: usize) -> Result<()> {
        let pad = "    ".repeat(indent);
        out.push_str(&format!("{pad}{}: t.Final[{}]\n", c.name, c.py_type));
        Ok(())
    }

    // ── Function ─────────────────────────────────────────────────────────────

    fn gen_function(&mut self, f: &PyFunction, out: &mut String, indent: usize) -> Result<()> {
        let pad = "    ".repeat(indent);
        let ret = self.resolve_type(&f.return_type, &format!("{}::return", f.name))?;

        let params_str = if let Some(sig) = &f.signature_override {
            // #[pyo3(signature = (...))] is present: merge signature defaults with Rust types
            self.merge_sig_with_types(sig, &f.params, &f.name)?
        } else {
            self.gen_params(&f.params, &f.name, false)?
        };

        out.push_str(&format!("{pad}def {}({params_str}) -> {ret}:\n", f.name));

        if !f.doc.is_empty() {
            self.gen_docstring(&f.doc, out, indent + 1);
        } else {
            out.push_str(&format!("{pad}    ...\n"));
        }
        out.push('\n');
        Ok(())
    }

    // ── Class ────────────────────────────────────────────────────────────────

    fn gen_class(&mut self, c: &PyClass, out: &mut String, indent: usize) -> Result<()> {
        let pad = "    ".repeat(indent);

        let base_name: Option<String> = if c.is_enum {
            None
        } else {
            match &c.extends {
                None => None,
                Some(ExtendsSpec::Builtin(b)) => Some((*b).to_string()),
                Some(ExtendsSpec::PyClassRustName(rust_base)) => {
                    if let Some(py) = self.known_classes.get(rust_base) {
                        if let (Some(cur), Some(map)) =
                            (&self.current_stub_module, &self.class_rust_to_module)
                            && let Some(def_mod) = map.get(rust_base.as_str())
                            && def_mod != cur
                        {
                            self.cross_module_imports
                                .entry(def_mod.clone())
                                .or_default()
                                .insert(py.clone());
                        }
                        Some(py.clone())
                    } else {
                        self.warnings.push(format!(
                            "extends base `{rust_base}` is not a known #[pyclass] — omitting base in stub for `{}`",
                            c.name
                        ));
                        None
                    }
                }
            }
        };

        let emit_final = c.is_enum || !c.allows_python_subclass;
        if emit_final {
            out.push_str(&format!("{pad}@t.final\n"));
        }

        let class_line = match &base_name {
            Some(b) => format!("{pad}class {}({b}):\n", c.name),
            None => format!("{pad}class {}:\n", c.name),
        };
        out.push_str(&class_line);

        if !c.doc.is_empty() {
            self.gen_docstring(&c.doc, out, indent + 1);
        }

        if c.methods.is_empty() {
            out.push_str(&format!("{pad}    ...\n"));
        } else {
            for method in &c.methods {
                self.gen_method(method, c, out, indent + 1)?;
            }
        }
        out.push('\n');
        Ok(())
    }

    fn gen_method(
        &mut self,
        m: &PyMethod,
        class: &PyClass,
        out: &mut String,
        indent: usize,
    ) -> Result<()> {
        self.current_self_type = Some(class.name.clone());
        let _guard =
            RestoreCurrentSelfTypeGuard(&mut self.current_self_type as *mut Option<String>);
        let pad = "    ".repeat(indent);

        if let Some(ov) = self.find_method_override(class, m) {
            self.emit_override_stub(ov, &m.doc, out, indent, true)?;
            return Ok(());
        }

        let location = format!("{}::{}", class.name, m.name);
        let ret = self.resolve_type(&m.return_type, &format!("{location}::return"))?;

        match &m.kind {
            MethodKind::New => {
                let params = self.method_params(m, &location, true)?;
                out.push_str(&format!("{pad}def __init__({params}) -> None:\n"));
            }
            MethodKind::Static => {
                out.push_str(&format!("{pad}@staticmethod\n"));
                let params = self.method_params(m, &location, false)?;
                out.push_str(&format!("{pad}def {}({params}) -> {ret}:\n", m.name));
            }
            MethodKind::Class => {
                out.push_str(&format!("{pad}@classmethod\n"));
                let params = self.method_params(m, &location, true)?;
                out.push_str(&format!("{pad}def {}({params}) -> {ret}:\n", m.name));
            }
            MethodKind::Getter(prop) => {
                out.push_str(&format!("{pad}@property\n"));
                out.push_str(&format!("{pad}def {prop}(self) -> {ret}:\n"));
            }
            MethodKind::Setter(prop) => {
                // setter takes one value param
                let val_type = m
                    .params
                    .first()
                    .map(|p| self.resolve_type(&p.ty, &location))
                    .transpose()?
                    .unwrap_or_else(|| "t.Any".to_string());
                if val_type == "t.Any" {
                    self.needs_any = true;
                }
                out.push_str(&format!("{pad}@{prop}.setter\n"));
                out.push_str(&format!(
                    "{pad}def {prop}(self, value: {val_type}) -> None:\n"
                ));
            }
            MethodKind::Instance => {
                let params = self.method_params(m, &location, true)?;
                out.push_str(&format!("{pad}def {}({params}) -> {ret}:\n", m.name));
            }
        }

        if !m.doc.is_empty() {
            self.gen_docstring(&m.doc, out, indent + 1);
        } else {
            out.push_str(&format!("{pad}    ...\n"));
        }
        out.push('\n');
        Ok(())
    }

    /// Build the parameter list for a method: uses `signature_override` when present,
    /// otherwise `gen_params`. When `with_self` is true, the result includes a leading `self`.
    fn method_params(&mut self, m: &PyMethod, location: &str, with_self: bool) -> Result<String> {
        if let Some(sig) = &m.signature_override {
            let merged = self.merge_sig_with_types(sig, &m.params, location)?;
            Ok(if with_self {
                format!("self, {merged}")
            } else {
                merged
            })
        } else {
            self.gen_params(&m.params, location, with_self)
        }
    }

    // ── Signature merge ──────────────────────────────────────────────────────

    /// Combine a `#[pyo3(signature = (...))]` string with the Rust param types.
    ///
    /// `sig` is the inner content of the signature attribute with outer parens already
    /// stripped, e.g. `"page=None, clip=None, tf_settings=None, **kwargs"`.
    /// For every name found in `params` we attach the mapped Python type; entries that
    /// have no Rust counterpart (e.g. a sentinel `/`) are emitted verbatim.
    fn merge_sig_with_types(
        &mut self,
        sig: &str,
        params: &[PyParam],
        fn_name: &str,
    ) -> Result<String> {
        // Build name → PyParam lookup
        let param_by_name: HashMap<&str, &PyParam> =
            params.iter().map(|p| (p.name.as_str(), p)).collect();

        let raw_parts = split_at_top_level_commas(sig);
        let mut out_parts: Vec<String> = Vec::new();

        for raw in raw_parts {
            let token = raw.trim();
            if token.is_empty() {
                continue;
            }

            if let Some(name) = token.strip_prefix("**") {
                let name = name.trim(); // proc-macro may emit "** kwargs" with a space
                // **kwargs are unpacked; no type (Unpack[TypedDict] etc. not supported yet).
                out_parts.push(format!("**{name}"));
            } else if let Some(name) = token.strip_prefix('*') {
                let name = name.trim();
                if name.is_empty() {
                    // bare `*` — keyword-only argument separator
                    out_parts.push("*".to_string());
                } else {
                    // *args are unpacked; no type.
                    out_parts.push(format!("*{name}"));
                }
            } else {
                let (name, default_opt) = split_name_default(token);
                if let Some(p) = param_by_name.get(name) {
                    let ty = self.resolve_type(&p.ty, &format!("{fn_name}::{name}"))?;
                    match default_opt {
                        Some(default) => out_parts.push(format!("{name}: {ty} = {default}")),
                        None => out_parts.push(format!("{name}: {ty}")),
                    }
                } else {
                    // No Rust type available — keep the signature token as-is
                    out_parts.push(token.to_string());
                }
            }
        }

        Ok(out_parts.join(", "))
    }

    // ── Params ───────────────────────────────────────────────────────────────

    fn gen_params(
        &mut self,
        params: &[PyParam],
        location: &str,
        with_self: bool,
    ) -> Result<String> {
        let mut parts: Vec<String> = Vec::new();
        if with_self {
            parts.push("self".to_string());
        }
        for p in params {
            let ty = self.resolve_type(&p.ty, &format!("{location}::{}", p.name))?;
            let part = match p.kind {
                ParamKind::Args => format!("*{}: {ty}", p.name),
                ParamKind::Kwargs => format!("**{}: {ty}", p.name),
                ParamKind::Regular => {
                    if let Some(default) = &p.default {
                        format!("{}: {} = {}", p.name, ty, default)
                    } else {
                        format!("{}: {ty}", p.name)
                    }
                }
            };
            parts.push(part);
        }
        Ok(parts.join(", "))
    }

    // ── Docstring ────────────────────────────────────────────────────────────

    fn gen_docstring(&self, lines: &[String], out: &mut String, indent: usize) {
        let pad = "    ".repeat(indent);
        if lines.len() == 1 {
            out.push_str(&format!("{pad}\"\"\"{}\"\"\"\n", lines[0]));
        } else {
            out.push_str(&format!("{pad}\"\"\"\n"));
            for line in lines {
                out.push_str(&format!("{pad}{line}\n"));
            }
            out.push_str(&format!("{pad}\"\"\"\n"));
        }
    }
}

// ── Cross-module `from ... import ...` (split .pyi layout) ─────────────────────

fn segment_generic_args(seg: &syn::PathSegment) -> Vec<&Type> {
    match &seg.arguments {
        syn::PathArguments::AngleBracketed(ab) => ab
            .args
            .iter()
            .filter_map(|a| match a {
                syn::GenericArgument::Type(t) => Some(t),
                // `Iterator<Item = Layer>` — associated type binding, not a positional type arg.
                syn::GenericArgument::AssocType(at) => Some(&at.ty),
                _ => None,
            })
            .collect(),
        _ => vec![],
    }
}

/// Walk a Rust type and record cross-module `#[pyclass]` references for import lines.
fn collect_pyclass_refs_in_type(
    ty: &Type,
    known_classes: &HashMap<String, String>,
    class_rust_to_module: &HashMap<String, String>,
    current_stub_module: &str,
    out: &mut BTreeMap<String, BTreeSet<String>>,
) {
    match ty {
        Type::Path(tp) => collect_from_type_path(
            tp,
            known_classes,
            class_rust_to_module,
            current_stub_module,
            out,
        ),
        Type::Reference(r) => collect_pyclass_refs_in_type(
            &r.elem,
            known_classes,
            class_rust_to_module,
            current_stub_module,
            out,
        ),
        Type::Slice(s) => collect_pyclass_refs_in_type(
            &s.elem,
            known_classes,
            class_rust_to_module,
            current_stub_module,
            out,
        ),
        Type::Tuple(t) => {
            for e in &t.elems {
                collect_pyclass_refs_in_type(
                    e,
                    known_classes,
                    class_rust_to_module,
                    current_stub_module,
                    out,
                );
            }
        }
        Type::Paren(p) => collect_pyclass_refs_in_type(
            &p.elem,
            known_classes,
            class_rust_to_module,
            current_stub_module,
            out,
        ),
        Type::Group(g) => collect_pyclass_refs_in_type(
            &g.elem,
            known_classes,
            class_rust_to_module,
            current_stub_module,
            out,
        ),
        Type::Array(a) => collect_pyclass_refs_in_type(
            &a.elem,
            known_classes,
            class_rust_to_module,
            current_stub_module,
            out,
        ),
        Type::Ptr(p) => collect_pyclass_refs_in_type(
            &p.elem,
            known_classes,
            class_rust_to_module,
            current_stub_module,
            out,
        ),
        Type::BareFn(b) => {
            for arg in &b.inputs {
                collect_pyclass_refs_in_type(
                    &arg.ty,
                    known_classes,
                    class_rust_to_module,
                    current_stub_module,
                    out,
                );
            }
            match &b.output {
                ReturnType::Default => {}
                ReturnType::Type(_, ret_ty) => collect_pyclass_refs_in_type(
                    ret_ty,
                    known_classes,
                    class_rust_to_module,
                    current_stub_module,
                    out,
                ),
            }
        }
        Type::ImplTrait(it) => collect_pyclass_refs_in_type_param_bounds(
            &it.bounds,
            known_classes,
            class_rust_to_module,
            current_stub_module,
            out,
        ),
        Type::TraitObject(to) => collect_pyclass_refs_in_type_param_bounds(
            &to.bounds,
            known_classes,
            class_rust_to_module,
            current_stub_module,
            out,
        ),
        // No nested `Type` inside these variants; `Macro` / `Verbatim` are opaque to static analysis.
        Type::Never(_) | Type::Infer(_) | Type::Macro(_) | Type::Verbatim(_) => {}
        // `syn::Type` is `#[non_exhaustive]`
        _ => {}
    }
}

fn collect_pyclass_refs_in_type_param_bounds(
    bounds: &syn::punctuated::Punctuated<TypeParamBound, syn::token::Plus>,
    known_classes: &HashMap<String, String>,
    class_rust_to_module: &HashMap<String, String>,
    current_stub_module: &str,
    out: &mut BTreeMap<String, BTreeSet<String>>,
) {
    for bound in bounds {
        match bound {
            TypeParamBound::Trait(tb) => {
                let tp = syn::TypePath {
                    qself: None,
                    path: tb.path.clone(),
                };
                collect_from_type_path(
                    &tp,
                    known_classes,
                    class_rust_to_module,
                    current_stub_module,
                    out,
                );
            }
            TypeParamBound::Lifetime(_) | TypeParamBound::Verbatim(_) => {}
            // `TypeParamBound` is `#[non_exhaustive]`
            _ => {}
        }
    }
}

fn collect_from_type_path(
    tp: &syn::TypePath,
    known_classes: &HashMap<String, String>,
    class_rust_to_module: &HashMap<String, String>,
    current_stub_module: &str,
    out: &mut BTreeMap<String, BTreeSet<String>>,
) {
    let Some(last_seg) = tp.path.segments.last() else {
        return;
    };
    let last_ident = last_seg.ident.to_string();
    let args = segment_generic_args(last_seg);

    match last_ident.as_str() {
        "Self" => {}
        "PyResult" | "Result" => {
            if let Some(t) = args.first() {
                collect_pyclass_refs_in_type(
                    t,
                    known_classes,
                    class_rust_to_module,
                    current_stub_module,
                    out,
                );
            }
        }
        "Option" => {
            if let Some(t) = args.first() {
                collect_pyclass_refs_in_type(
                    t,
                    known_classes,
                    class_rust_to_module,
                    current_stub_module,
                    out,
                );
            }
        }
        "Vec" => {
            if let Some(t) = args.first() {
                collect_pyclass_refs_in_type(
                    t,
                    known_classes,
                    class_rust_to_module,
                    current_stub_module,
                    out,
                );
            }
        }
        "HashMap" | "BTreeMap" | "IndexMap" => {
            if let Some(t) = args.first() {
                collect_pyclass_refs_in_type(
                    t,
                    known_classes,
                    class_rust_to_module,
                    current_stub_module,
                    out,
                );
            }
            if let Some(t) = args.get(1) {
                collect_pyclass_refs_in_type(
                    t,
                    known_classes,
                    class_rust_to_module,
                    current_stub_module,
                    out,
                );
            }
        }
        "HashSet" | "BTreeSet" => {
            if let Some(t) = args.first() {
                collect_pyclass_refs_in_type(
                    t,
                    known_classes,
                    class_rust_to_module,
                    current_stub_module,
                    out,
                );
            }
        }
        "PyRef" | "PyRefMut" => {
            if let Some(t) = args.first() {
                collect_pyclass_refs_in_type(
                    t,
                    known_classes,
                    class_rust_to_module,
                    current_stub_module,
                    out,
                );
            }
        }
        "Py" | "Bound" | "Borrowed" => {
            if let Some(t) = args.first() {
                collect_pyclass_refs_in_type(
                    t,
                    known_classes,
                    class_rust_to_module,
                    current_stub_module,
                    out,
                );
            }
        }
        _ => {
            if let Some(python_name) = known_classes.get(&last_ident)
                && let Some(def_mod) = class_rust_to_module.get(&last_ident)
                && def_mod != current_stub_module
            {
                out.entry(def_mod.clone())
                    .or_default()
                    .insert(python_name.clone());
            }
            for t in args {
                collect_pyclass_refs_in_type(
                    t,
                    known_classes,
                    class_rust_to_module,
                    current_stub_module,
                    out,
                );
            }
        }
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Recursively collect all `#[pyclass]` entries from a slice of modules, returning a map
/// from Rust struct/enum name → Python class name, plus any collision warnings.
///
/// When the Rust struct and the Python class share the same name (no `name = "..."` attr)
/// both key and value are identical.  When they differ (e.g. `PyPageIterator` → `PageIterator`)
/// the Rust name is the key so that type-signature lookups can resolve the correct Python name.
///
/// If two `#[pyclass]` items in different modules share the same Rust name but map to
/// different Python names, the first registration wins and a warning is emitted.
pub fn collect_class_names(modules: &[PyModule]) -> (HashMap<String, String>, Vec<String>) {
    let mut map = HashMap::new();
    let mut warnings = Vec::new();
    for m in modules {
        collect_class_names_from_module(m, &mut map, &mut warnings);
    }
    (map, warnings)
}

fn collect_class_names_from_module(
    m: &PyModule,
    map: &mut HashMap<String, String>,
    warnings: &mut Vec<String>,
) {
    for item in &m.items {
        match item {
            PyItem::Class(c) => {
                if let Some(existing) = map.get(&c.rust_name) {
                    if existing != &c.name {
                        warnings.push(format!(
                            "Rust type `{}` is registered as both `{}` and `{}` — \
                             using `{}`. Rename one struct or add `#[pyclass(name = \"...\")]`.",
                            c.rust_name, existing, c.name, existing
                        ));
                    }
                } else {
                    map.insert(c.rust_name.clone(), c.name.clone());
                }
            }
            PyItem::Module(sub) => collect_class_names_from_module(sub, map, warnings),
            PyItem::Function(_) | PyItem::Constant(_) => {}
        }
    }
}

fn item_name(item: &PyItem) -> &str {
    match item {
        PyItem::Constant(c) => &c.name,
        PyItem::Function(f) => &f.name,
        PyItem::Class(c) => &c.name,
        PyItem::Module(m) => &m.name,
    }
}

/// `[[override]]` `item` for a top-level pymodule member: `logical_mod::python_name`, suffix `::python_name`,
/// or bare `python_name`. For `#[pyfunction]`, `#[pyo3(name = "...")]` can differ from the Rust `fn` name,
/// so we also match `::rust_fn_ident` and bare `rust_fn_ident` (same as [`PyFunction::rust_name`]).
fn module_level_override_matches(o_item: &str, item: &PyItem) -> bool {
    match item {
        PyItem::Function(f) => {
            o_item.ends_with(&format!("::{}", f.name))
                || o_item == f.name.as_str()
                || o_item.ends_with(&format!("::{}", f.rust_name))
                || o_item == f.rust_name.as_str()
        }
        _ => {
            let n = item_name(item);
            o_item.ends_with(&format!("::{n}")) || o_item == n
        }
    }
}

/// Split a parameter-list string at top-level commas, respecting nested brackets.
///
/// For example, `"a, b=(1,2), **kwargs"` yields `["a", " b=(1,2)", " **kwargs"]`.
///
/// Unbalanced brackets (malformed input) are tolerated: depth uses saturating_sub
/// so the function continues and produces best-effort output rather than panicking.
fn split_at_top_level_commas(s: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth: usize = 0;
    let mut start = 0;
    for (i, ch) in s.char_indices() {
        match ch {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                parts.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&s[start..]);
    parts
}

/// Split a signature token such as `"page=None"` into `("page", Some("None"))`.
/// Tokens without a `=` return `(token, None)`.
/// Only the **first** `=` is used as the split point.
fn split_name_default(token: &str) -> (&str, Option<&str>) {
    match token.find('=') {
        Some(pos) => (token[..pos].trim(), Some(token[pos + 1..].trim())),
        None => (token.trim(), None),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collector::{
        ExtendsSpec, MethodKind, ParamKind, PyClass, PyConstant, PyFunction, PyItem, PyMethod,
        PyModule, PyParam, PyType,
    };
    use crate::config::Config;
    use crate::stub_constants::{AUTO_GENERATED_BANNER, TYPING_IMPORT_LINE};
    use std::path::PathBuf;

    // ── Test-data builders ───────────────────────────────────────────────────

    fn dummy_path() -> PathBuf {
        PathBuf::from("test.rs")
    }

    fn make_module(items: Vec<PyItem>) -> PyModule {
        PyModule {
            name: "m".to_string(),
            doc: vec![],
            items,
            source_file: dummy_path(),
        }
    }

    fn make_fn(name: &str, sig: Option<&str>, params: Vec<PyParam>, ret: syn::Type) -> PyFunction {
        PyFunction {
            name: name.to_string(),
            rust_name: name.to_string(),
            doc: vec![],
            signature_override: sig.map(str::to_string),
            params,
            return_type: PyType {
                rust_type: ret,
                override_str: None,
            },
            source_file: dummy_path(),
        }
    }

    fn make_param(name: &str, rust_type: syn::Type) -> PyParam {
        PyParam {
            name: name.to_string(),
            ty: PyType {
                rust_type,
                override_str: None,
            },
            default: None,
            kind: ParamKind::Regular,
        }
    }

    /// Build a class with one static method that returns PyResult<Self> (e.g. from_bytes).
    fn make_class_with_self_return(class_name: &str, method_name: &str) -> PyClass {
        PyClass {
            name: class_name.to_string(),
            rust_name: class_name.to_string(),
            module: None,
            allows_python_subclass: false,
            extends: None,
            is_enum: false,
            doc: vec![],
            methods: vec![PyMethod {
                rust_ident: method_name.to_string(),
                name: method_name.to_string(),
                doc: vec![],
                kind: MethodKind::Static,
                signature_override: None,
                params: vec![make_param("data", syn::parse_quote! { &[u8] })],
                return_type: PyType {
                    rust_type: syn::parse_quote! { pyo3::PyResult<Self> },
                    override_str: None,
                },
            }],
            source_file: dummy_path(),
        }
    }

    fn stub_for(items: Vec<PyItem>) -> String {
        let config = Config::default();
        generate(&[make_module(items)], &config).unwrap()
    }

    fn config_with_python_version(version: &str) -> Config {
        let mut config = Config::default();
        config.output.python_version = version.to_string();
        config
    }

    fn stub_for_config(items: Vec<PyItem>, config: &Config) -> String {
        generate(&[make_module(items)], config).unwrap()
    }

    /// generate_with_known_classes resolves return types using the provided map,
    /// so a function in the root stub that returns a class from another stub (e.g. Layer)
    /// emits the class name, not Any.
    #[test]
    fn generate_with_known_classes_resolves_cross_module_class_in_return_type() {
        let root_module = make_module(vec![PyItem::Function(make_fn(
            "get_layer",
            None,
            vec![],
            syn::parse_quote! { pyo3::PyResult<Layer> },
        ))]);
        let mut known_classes = HashMap::new();
        known_classes.insert("Layer".to_string(), "Layer".to_string());
        let stub = generate_with_known_classes(
            &[root_module],
            &Config::default(),
            &known_classes,
            &[],
            None,
        )
        .unwrap();
        assert!(
            stub.contains("-> Layer"),
            "return type should resolve to Layer from known_classes, not Any; got:\n{stub}"
        );
    }

    /// When generating a stub for `abcd.ee` and the return type references a class defined in
    /// `abcd.ff`, emit `from abcd.ff import Layer`.
    #[test]
    fn generate_with_known_classes_emits_cross_module_import() {
        let ee_module = PyModule {
            name: "abcd.ee".to_string(),
            doc: vec![],
            items: vec![PyItem::Function(make_fn(
                "get_layer",
                None,
                vec![],
                syn::parse_quote! { pyo3::PyResult<Layer> },
            ))],
            source_file: dummy_path(),
        };
        let mut known_classes = HashMap::new();
        known_classes.insert("Layer".to_string(), "Layer".to_string());
        let mut class_mod = HashMap::new();
        class_mod.insert("Layer".to_string(), "abcd.ff".to_string());
        let stub = generate_with_known_classes(
            &[ee_module],
            &Config::default(),
            &known_classes,
            &[],
            Some(("abcd.ee", &class_mod)),
        )
        .unwrap();
        assert!(
            stub.contains("from abcd.ff import Layer"),
            "expected cross-module import; got:\n{stub}"
        );
        assert!(stub.contains("-> Layer"));
    }

    /// `PyResult<[Layer; N]>` still triggers `from abcd.ff import Layer` (element type walk).
    #[test]
    fn generate_with_known_classes_emits_cross_module_import_for_array_elem() {
        let ee_module = PyModule {
            name: "abcd.ee".to_string(),
            doc: vec![],
            items: vec![PyItem::Function(make_fn(
                "layers",
                None,
                vec![],
                syn::parse_quote! { pyo3::PyResult<[Layer; 4]> },
            ))],
            source_file: dummy_path(),
        };
        let mut known_classes = HashMap::new();
        known_classes.insert("Layer".to_string(), "Layer".to_string());
        let mut class_mod = HashMap::new();
        class_mod.insert("Layer".to_string(), "abcd.ff".to_string());
        let stub = generate_with_known_classes(
            &[ee_module],
            &Config::default(),
            &known_classes,
            &[],
            Some(("abcd.ee", &class_mod)),
        )
        .unwrap();
        assert!(
            stub.contains("from abcd.ff import Layer"),
            "expected cross-module import for array elem; got:\n{stub}"
        );
    }

    /// `impl Iterator<Item = Layer>` walks trait bounds and picks up `Layer` from associated type args.
    #[test]
    fn generate_with_known_classes_emits_cross_module_import_for_impl_trait() {
        let ee_module = PyModule {
            name: "abcd.ee".to_string(),
            doc: vec![],
            items: vec![PyItem::Function(make_fn(
                "iter_layers",
                None,
                vec![],
                syn::parse_quote! { impl Iterator<Item = Layer> },
            ))],
            source_file: dummy_path(),
        };
        let mut known_classes = HashMap::new();
        known_classes.insert("Layer".to_string(), "Layer".to_string());
        let mut class_mod = HashMap::new();
        class_mod.insert("Layer".to_string(), "abcd.ff".to_string());
        let stub = generate_with_known_classes(
            &[ee_module],
            &Config::default(),
            &known_classes,
            &[],
            Some(("abcd.ee", &class_mod)),
        )
        .unwrap();
        assert!(
            stub.contains("from abcd.ff import Layer"),
            "expected cross-module import for impl trait; got:\n{stub}"
        );
    }

    // ── split_at_top_level_commas ────────────────────────────────────────────

    #[test]
    fn split_simple_list() {
        assert_eq!(split_at_top_level_commas("a, b, c"), vec!["a", " b", " c"]);
    }

    #[test]
    fn split_respects_nested_parens() {
        // The comma inside (1,2) must not split the token
        let parts = split_at_top_level_commas("a, b=(1,2), c");
        assert_eq!(parts, vec!["a", " b=(1,2)", " c"]);
    }

    #[test]
    fn split_single_token() {
        assert_eq!(split_at_top_level_commas("page=None"), vec!["page=None"]);
    }

    #[test]
    fn split_with_kwargs() {
        let parts = split_at_top_level_commas("page=None, **kwargs");
        assert_eq!(parts, vec!["page=None", " **kwargs"]);
    }

    #[test]
    fn split_nested_brackets_and_braces() {
        let parts = split_at_top_level_commas("a, b=[1,2], c={3}");
        assert_eq!(parts, vec!["a", " b=[1,2]", " c={3}"]);
    }

    // ── split_name_default ───────────────────────────────────────────────────

    #[test]
    fn name_default_with_none() {
        assert_eq!(split_name_default("page=None"), ("page", Some("None")));
    }

    #[test]
    fn name_default_with_bool() {
        assert_eq!(
            split_name_default("extract_text=True"),
            ("extract_text", Some("True"))
        );
    }

    #[test]
    fn name_default_absent() {
        assert_eq!(split_name_default("cells"), ("cells", None));
    }

    #[test]
    fn name_default_trims_whitespace() {
        // Signature tokens extracted by pyo3 often have surrounding spaces
        assert_eq!(split_name_default(" page = None "), ("page", Some("None")));
    }

    #[test]
    fn name_default_tuple_default() {
        // Only the first `=` is used; value may contain `=` in a nested expression
        assert_eq!(split_name_default("clip=(1,2)"), ("clip", Some("(1,2)")));
    }

    // ── merge_sig_with_types (via generate) ──────────────────────────────────

    /// Regular params with defaults get their Rust types attached.
    #[test]
    fn merge_attaches_type_and_preserves_default() {
        let f = make_fn(
            "find_cells",
            Some("page=None, clip=None"),
            vec![
                make_param("page", syn::parse_quote! { Option<i32> }),
                make_param("clip", syn::parse_quote! { Option<String> }),
            ],
            syn::parse_quote! { Vec<i32> },
        );
        let stub = stub_for(vec![PyItem::Function(f)]);
        assert!(stub.contains("page: int | None = None"), "got:\n{stub}");
        assert!(stub.contains("clip: str | None = None"), "got:\n{stub}");
    }

    /// Required params (no default in signature) get typed but no `= ...`.
    #[test]
    fn merge_required_param_no_default() {
        let f = make_fn(
            "proc",
            Some("cells, extract_text"),
            vec![
                make_param("cells", syn::parse_quote! { Vec<i32> }),
                make_param("extract_text", syn::parse_quote! { bool }),
            ],
            syn::parse_quote! { () },
        );
        let stub = stub_for(vec![PyItem::Function(f)]);
        assert!(stub.contains("cells: list[int]"), "got:\n{stub}");
        assert!(stub.contains("extract_text: bool"), "got:\n{stub}");
        // Must not gain a spurious default
        assert!(!stub.contains("cells: list[int] ="), "got:\n{stub}");
    }

    /// `**kwargs` is emitted without a type (no Unpack[TypedDict] etc. for now).
    #[test]
    fn merge_kwargs_no_type() {
        let f = make_fn(
            "my_fn",
            Some("page=None, **kwargs"),
            vec![
                make_param("page", syn::parse_quote! { Option<i32> }),
                make_param("kwargs", syn::parse_quote! { Option<i32> }),
            ],
            syn::parse_quote! { () },
        );
        let stub = stub_for(vec![PyItem::Function(f)]);
        assert!(stub.contains("**kwargs"), "got:\n{stub}");
        assert!(
            !stub.contains("**kwargs:"),
            "**kwargs should have no type annotation, got:\n{stub}"
        );
    }

    /// When signature has regular params before *args and **kwargs, regular params keep their types.
    #[test]
    fn merge_mixed_args_kwargs_regular_params_typed() {
        let f = make_fn(
            "find_tables",
            Some("page=None, clip=None, *args, **kwargs"),
            vec![
                make_param("page", syn::parse_quote! { Option<i32> }),
                make_param("clip", syn::parse_quote! { Option<f64> }),
                make_param("args", syn::parse_quote! { Vec<String> }),
                make_param(
                    "kwargs",
                    syn::parse_quote! { Option<pyo3::Bound<'_, pyo3::types::PyDict>> },
                ),
            ],
            syn::parse_quote! { () },
        );
        let stub = stub_for(vec![PyItem::Function(f)]);
        assert!(stub.contains("page: "), "page must have type, got:\n{stub}");
        assert!(stub.contains("clip: "), "clip must have type, got:\n{stub}");
        assert!(
            stub.contains("page: int | None = None")
                || stub.contains("page: t.Optional[int] = None"),
            "got:\n{stub}"
        );
        assert!(
            stub.contains("clip: float | None = None")
                || stub.contains("clip: t.Optional[float] = None"),
            "got:\n{stub}"
        );
        assert!(stub.contains("*args"), "got:\n{stub}");
        assert!(
            !stub.contains("*args:"),
            "*args must have no type, got:\n{stub}"
        );
        assert!(stub.contains("**kwargs"), "got:\n{stub}");
        assert!(
            !stub.contains("**kwargs:"),
            "**kwargs must have no type, got:\n{stub}"
        );
    }

    /// A token in the signature with no matching Rust param is kept verbatim.
    /// This covers the positional-only sentinel `/` and future unknown tokens.
    #[test]
    fn merge_unknown_token_kept_verbatim() {
        let f = make_fn(
            "my_fn",
            Some("known=None, /"),
            vec![make_param("known", syn::parse_quote! { Option<i32> })],
            syn::parse_quote! { () },
        );
        let stub = stub_for(vec![PyItem::Function(f)]);
        assert!(
            stub.contains("known: int | None = None, /"),
            "positional-only '/' should be preserved with typed param, got:\n{stub}"
        );
    }

    /// Bare `*` (keyword-only separator) is emitted as-is without type annotation.
    /// `(a, *, b=None)` must not become `(a, *: Any, b: ...)`.
    #[test]
    fn merge_bare_star_keyword_only_separator() {
        let f = make_fn(
            "my_fn",
            Some("a, *, b=None"),
            vec![
                make_param("a", syn::parse_quote! { i32 }),
                make_param("b", syn::parse_quote! { Option<i32> }),
            ],
            syn::parse_quote! { () },
        );
        let stub = stub_for(vec![PyItem::Function(f)]);
        assert!(
            stub.contains("a: int, *, b: int | None = None"),
            "bare '*' should be kept as separator without type, got:\n{stub}"
        );
        assert!(
            !stub.contains("*:"),
            "bare '*' must not gain a type annotation, got:\n{stub}"
        );
    }

    /// `*args` gets the correct type from the Rust param (e.g. `Vec<i32>` → `list[int]`).
    /// `*args` is emitted without a type (unpacked positional args).
    #[test]
    fn merge_args_no_type() {
        let f = make_fn(
            "my_fn",
            Some("a, *args"),
            vec![
                make_param("a", syn::parse_quote! { i32 }),
                make_param("args", syn::parse_quote! { Vec<i32> }),
            ],
            syn::parse_quote! { () },
        );
        let stub = stub_for(vec![PyItem::Function(f)]);
        assert!(stub.contains("*args"), "got:\n{stub}");
        assert!(
            !stub.contains("*args:"),
            "*args should have no type annotation, got:\n{stub}"
        );
    }

    /// When no `signature_override` is set, `gen_params` is used unchanged (regression guard).
    #[test]
    fn no_signature_override_uses_rust_params_directly() {
        let f = make_fn(
            "add",
            None, // no override
            vec![
                make_param("a", syn::parse_quote! { i32 }),
                make_param("b", syn::parse_quote! { i32 }),
            ],
            syn::parse_quote! { i32 },
        );
        let stub = stub_for(vec![PyItem::Function(f)]);
        assert!(
            stub.contains("def add(a: int, b: int) -> int"),
            "got:\n{stub}"
        );
    }

    // ── RenderPolicy: python_version drives future_annotations, Self, Optional ─

    /// With python_version 3.9, stub must contain `from __future__ import annotations`
    /// and class method returning Self must show the class name (e.g. PdfDocument), not Self.
    #[test]
    fn render_policy_py39_emits_future_annotations_and_class_name_for_self() {
        let config = config_with_python_version("3.9");
        let class = make_class_with_self_return("PdfDocument", "from_bytes");
        let stub = stub_for_config(vec![PyItem::Class(class)], &config);
        assert!(
            stub.contains("from __future__ import annotations"),
            "py 3.9 must emit future annotations, got:\n{stub}"
        );
        assert!(
            stub.contains("-> PdfDocument:"),
            "py 3.9 must use class name as return type, got:\n{stub}"
        );
        assert!(
            stub.contains("data: bytes"),
            "from_bytes param must be bytes, got:\n{stub}"
        );
        assert!(
            !stub.contains("t.Self"),
            "py 3.9 should not use typing.Self, got:\n{stub}"
        );
    }

    /// With python_version 3.12 (native_self), stub must NOT add future_annotations,
    /// must add `import typing as t`, and return type must be `t.Self`.
    #[test]
    fn render_policy_py312_emits_native_self_and_no_future_annotations() {
        let config = config_with_python_version("3.12");
        let class = make_class_with_self_return("PdfDocument", "from_bytes");
        let stub = stub_for_config(vec![PyItem::Class(class)], &config);
        assert!(
            !stub.contains("from __future__ import annotations"),
            "py 3.12 with native Self should not emit future annotations, got:\n{stub}"
        );
        assert!(
            stub.contains(TYPING_IMPORT_LINE),
            "py 3.12 must import typing when Self is used, got:\n{stub}"
        );
        assert!(
            stub.contains("-> t.Self:") || stub.contains("-> t.Self :"),
            "py 3.12 must use t.Self as return type, got:\n{stub}"
        );
    }

    /// Option param with python_version 3.9 must use t.Optional[X] syntax.
    #[test]
    fn render_policy_py39_option_param_uses_optional_syntax() {
        let config = config_with_python_version("3.9");
        let f = make_fn(
            "foo",
            None,
            vec![make_param("x", syn::parse_quote! { Option<i32> })],
            syn::parse_quote! { () },
        );
        let stub = stub_for_config(vec![PyItem::Function(f)], &config);
        assert!(
            stub.contains("x: t.Optional[int]"),
            "py 3.9 must use t.Optional[int], got:\n{stub}"
        );
        assert!(stub.contains(TYPING_IMPORT_LINE), "got:\n{stub}");
    }

    /// Option param with python_version 3.10 must use X | None syntax (default config).
    #[test]
    fn render_policy_py310_option_param_uses_union_syntax() {
        let config = config_with_python_version("3.10");
        let f = make_fn(
            "foo",
            None,
            vec![make_param("x", syn::parse_quote! { Option<i32> })],
            syn::parse_quote! { () },
        );
        let stub = stub_for_config(vec![PyItem::Function(f)], &config);
        assert!(
            stub.contains("x: int | None"),
            "py 3.10 must use int | None, got:\n{stub}"
        );
    }

    // ── collect_class_names ──────────────────────────────────────────────────

    fn make_class(rust_name: &str, python_name: &str) -> PyClass {
        PyClass {
            name: python_name.to_string(),
            rust_name: rust_name.to_string(),
            module: None,
            allows_python_subclass: false,
            extends: None,
            is_enum: false,
            doc: vec![],
            methods: vec![],
            source_file: dummy_path(),
        }
    }

    /// Classes at the top level and inside nested sub-modules are all collected.
    #[test]
    fn collect_class_names_recurses_into_nested_modules() {
        let inner_module = PyModule {
            name: "inner".to_string(),
            doc: vec![],
            items: vec![PyItem::Class(make_class("PyInner", "Inner"))],
            source_file: dummy_path(),
        };
        let outer_module = PyModule {
            name: "outer".to_string(),
            doc: vec![],
            items: vec![
                PyItem::Class(make_class("PyOuter", "Outer")),
                PyItem::Module(inner_module),
            ],
            source_file: dummy_path(),
        };

        let (map, warnings) = collect_class_names(&[outer_module]);

        assert_eq!(map.get("PyOuter").map(String::as_str), Some("Outer"));
        assert_eq!(map.get("PyInner").map(String::as_str), Some("Inner"));
        assert!(warnings.is_empty(), "no collision expected");
    }

    /// When two classes share the same Rust name, the first registration wins
    /// and a warning is emitted — no silent overwrite.
    #[test]
    fn collect_class_names_warns_on_rust_name_collision() {
        let mod_a = PyModule {
            name: "mod_a".to_string(),
            doc: vec![],
            items: vec![PyItem::Class(make_class("Shared", "PythonA"))],
            source_file: dummy_path(),
        };
        let mod_b = PyModule {
            name: "mod_b".to_string(),
            doc: vec![],
            items: vec![PyItem::Class(make_class("Shared", "PythonB"))],
            source_file: dummy_path(),
        };

        let (map, warnings) = collect_class_names(&[mod_a, mod_b]);

        assert_eq!(
            map.get("Shared").map(String::as_str),
            Some("PythonA"),
            "first registration must win"
        );
        assert_eq!(warnings.len(), 1, "exactly one collision warning expected");
        assert!(
            warnings[0].contains("Shared"),
            "warning must mention the colliding Rust name"
        );
    }

    // ── FallbackStrategy ─────────────────────────────────────────────────────

    fn make_fn_unknown_return(name: &str) -> PyFunction {
        make_fn(name, None, vec![], syn::parse_quote! { SomeOpaqueType })
    }

    /// With `fallback.strategy = "error"`, an unknown type must cause generation to fail.
    #[test]
    fn fallback_strategy_error_returns_err_on_unknown_type() {
        let mut config = Config::default();
        config.fallback.strategy = FallbackStrategy::Error;
        let f = make_fn_unknown_return("risky");
        let result = generate(&[make_module(vec![PyItem::Function(f)])], &config);
        assert!(
            result.is_err(),
            "expected Err for unknown type with Error strategy"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("unknown type"),
            "error message should mention unknown type, got: {msg}"
        );
    }

    /// With `fallback.strategy = "skip"`, an unknown type must not cause an error;
    /// the item is still emitted with `Any` (current behaviour: skip == silent Any).
    #[test]
    fn fallback_strategy_skip_succeeds_on_unknown_type() {
        let mut config = Config::default();
        config.fallback.strategy = FallbackStrategy::Skip;
        let f = make_fn_unknown_return("quiet");
        let stub = generate(&[make_module(vec![PyItem::Function(f)])], &config)
            .expect("Skip strategy must not return an error");
        assert!(
            stub.contains("def quiet"),
            "function should still be emitted, got:\n{stub}"
        );
    }

    // ── config.overrides ─────────────────────────────────────────────────────

    /// A manual override entry matching a function name must replace the generated stub line.
    #[test]
    fn config_override_replaces_generated_stub() {
        use crate::config::OverrideEntry;
        let mut config = Config::default();
        config.overrides.push(OverrideEntry {
            item: "complex_fn".to_string(),
            stub: "def complex_fn(x: t.Any, **kwargs: t.Any) -> dict[str, t.Any]: ...".to_string(),
        });
        let f = make_fn("complex_fn", None, vec![], syn::parse_quote! { () });
        let stub = stub_for_config(vec![PyItem::Function(f)], &config);
        assert!(
            stub.contains("def complex_fn(x: t.Any, **kwargs: t.Any) -> dict[str, t.Any]:"),
            "override header must appear, got:\n{stub}"
        );
        assert!(
            stub.contains("    ...\n"),
            "no Rust doc → stub body must be ellipsis, got:\n{stub}"
        );
        assert!(
            !stub.contains("def complex_fn() ->"),
            "auto-generated version must not appear when overridden, got:\n{stub}"
        );
    }

    /// A fully-qualified override (`module::item`) must also match.
    #[test]
    fn config_override_qualified_path_matches() {
        use crate::config::OverrideEntry;
        let mut config = Config::default();
        config.overrides.push(OverrideEntry {
            item: "m::qualified_fn".to_string(),
            stub: "def qualified_fn() -> int:".to_string(),
        });
        let f = make_fn("qualified_fn", None, vec![], syn::parse_quote! { () });
        let stub = stub_for_config(vec![PyItem::Function(f)], &config);
        assert!(
            stub.contains("def qualified_fn() -> int:\n"),
            "qualified override header must appear, got:\n{stub}"
        );
        assert!(stub.contains("    ...\n"), "got:\n{stub}");
    }

    /// `#[pyfunction]` with `#[pyo3(name = "...")]` uses a Python name that differs from the Rust `fn` ident;
    /// `[[override]]` `item` may still use `module::rust_fn_ident`.
    #[test]
    fn config_override_module_fn_matches_rust_ident_when_python_name_differs() {
        use crate::config::OverrideEntry;
        let mut config = Config::default();
        config.overrides.push(OverrideEntry {
            item: "m::py_get_intersections_from_edges".to_string(),
            stub: "def get_intersections_from_edges() -> int: ...".to_string(),
        });
        let mut f = make_fn(
            "get_intersections_from_edges",
            None,
            vec![],
            syn::parse_quote! { () },
        );
        f.rust_name = "py_get_intersections_from_edges".to_string();
        let stub = stub_for_config(vec![PyItem::Function(f)], &config);
        assert!(
            stub.contains("def get_intersections_from_edges() -> int:"),
            "override matched by rust_name must appear, got:\n{stub}"
        );
    }

    /// Override without trailing `...` in TOML: Rust `///` doc becomes the `.pyi` docstring.
    #[test]
    fn config_override_emits_rust_doc_when_stub_has_no_ellipsis() {
        use crate::config::OverrideEntry;
        let mut config = Config::default();
        config.overrides.push(OverrideEntry {
            item: "g".to_string(),
            stub: "def g() -> int:".to_string(),
        });
        let mut f = make_fn("g", None, vec![], syn::parse_quote! { () });
        f.doc = vec!["From Rust docs.".to_string()];
        let stub = stub_for_config(vec![PyItem::Function(f)], &config);
        assert!(stub.contains("def g() -> int:\n"), "got:\n{stub}");
        assert!(
            stub.contains("\"\"\"From Rust docs.\"\"\""),
            "Rust doc must appear in stub, got:\n{stub}"
        );
        assert!(
            !stub.contains("def g() -> int:\n    ..."),
            "doc present → no bare ellipsis body, got:\n{stub}"
        );
    }

    /// `[[override]]` for `#[new]` matches `module::RustClass::rust_fn` and replaces `__init__`.
    #[test]
    fn config_override_class_method_new_by_rust_fn_name() {
        use crate::config::OverrideEntry;
        let mut config = Config::default();
        config.overrides.push(OverrideEntry {
            item: "m::TfSettings::py_new".to_string(),
            stub: "def __init__(self, **kwargs: t.Any) -> None:".to_string(),
        });
        let m = PyMethod {
            rust_ident: "py_new".to_string(),
            name: "py_new".to_string(),
            doc: vec![],
            kind: MethodKind::New,
            signature_override: None,
            params: vec![],
            return_type: PyType {
                rust_type: syn::parse_quote! { () },
                override_str: None,
            },
        };
        let class = PyClass {
            name: "TfSettings".to_string(),
            rust_name: "TfSettings".to_string(),
            module: None,
            allows_python_subclass: false,
            extends: None,
            is_enum: false,
            doc: vec![],
            methods: vec![m],
            source_file: dummy_path(),
        };
        let stub = stub_for_config(vec![PyItem::Class(class)], &config);
        assert!(
            stub.contains("def __init__(self, **kwargs: t.Any) -> None:"),
            "method override stub must appear, got:\n{stub}"
        );
        assert!(
            !stub.contains("def __init__(self) -> None:"),
            "default #[new] with no params must be replaced, got:\n{stub}"
        );
    }

    /// `#[new]` overrides also match `...::__init__` on the class segment.
    #[test]
    fn config_override_class_method_new_init_alias() {
        use crate::config::OverrideEntry;
        let mut config = Config::default();
        config.overrides.push(OverrideEntry {
            item: "m::Counter::__init__".to_string(),
            stub: "def __init__(self, x: int) -> None:".to_string(),
        });
        let m = make_method(
            "new",
            MethodKind::New,
            vec![make_param("x", syn::parse_quote! { i32 })],
            syn::parse_quote! { () },
        );
        let class = make_class_with_methods("Counter", vec![m]);
        let stub = stub_for_config(vec![PyItem::Class(class)], &config);
        assert!(
            stub.contains("def __init__(self, x: int) -> None:"),
            "expected __init__ alias match, got:\n{stub}"
        );
    }

    /// Suffix-only item path: must end with `::Class::method`.
    #[test]
    fn config_override_class_method_qualified_suffix_matches() {
        use crate::config::OverrideEntry;
        let mut config = Config::default();
        config.overrides.push(OverrideEntry {
            item: "crate::tablers::TfSettings::py_new".to_string(),
            stub: "def __init__(self) -> None:".to_string(),
        });
        let m = PyMethod {
            rust_ident: "py_new".to_string(),
            name: "py_new".to_string(),
            doc: vec![],
            kind: MethodKind::New,
            signature_override: None,
            params: vec![],
            return_type: PyType {
                rust_type: syn::parse_quote! { () },
                override_str: None,
            },
        };
        let class = make_class_with_methods("TfSettings", vec![m]);
        let stub = stub_for_config(vec![PyItem::Class(class)], &config);
        assert!(
            stub.contains("def __init__(self) -> None:"),
            "suffix-qualified override must match, got:\n{stub}"
        );
    }

    /// When #[pyclass(name = "...")] differs from the Rust struct, both names are valid class keys.
    #[test]
    fn config_override_class_method_matches_python_class_name() {
        use crate::config::OverrideEntry;
        let mut config = Config::default();
        config.overrides.push(OverrideEntry {
            item: "m::Visible::do_thing".to_string(),
            stub: "def do_thing(self) -> str:".to_string(),
        });
        let m = PyMethod {
            rust_ident: "do_thing".to_string(),
            name: "do_thing".to_string(),
            doc: vec![],
            kind: MethodKind::Instance,
            signature_override: None,
            params: vec![],
            return_type: PyType {
                rust_type: syn::parse_quote! { &'static str },
                override_str: None,
            },
        };
        let class = PyClass {
            name: "Visible".to_string(),
            rust_name: "Hidden".to_string(),
            module: None,
            allows_python_subclass: false,
            extends: None,
            is_enum: false,
            doc: vec![],
            methods: vec![m],
            source_file: dummy_path(),
        };
        let stub = stub_for_config(vec![PyItem::Class(class)], &config);
        assert!(
            stub.contains("def do_thing(self) -> str:"),
            "override by Python class name must match, got:\n{stub}"
        );
    }

    #[test]
    fn config_override_class_method_matches_rust_struct_name() {
        use crate::config::OverrideEntry;
        let mut config = Config::default();
        config.overrides.push(OverrideEntry {
            item: "m::Hidden::do_thing".to_string(),
            stub: "def do_thing(self) -> bytes:".to_string(),
        });
        let m = PyMethod {
            rust_ident: "do_thing".to_string(),
            name: "do_thing".to_string(),
            doc: vec![],
            kind: MethodKind::Instance,
            signature_override: None,
            params: vec![],
            return_type: PyType {
                rust_type: syn::parse_quote! { &'static str },
                override_str: None,
            },
        };
        let class = PyClass {
            name: "Visible".to_string(),
            rust_name: "Hidden".to_string(),
            module: None,
            allows_python_subclass: false,
            extends: None,
            is_enum: false,
            doc: vec![],
            methods: vec![m],
            source_file: dummy_path(),
        };
        let stub = stub_for_config(vec![PyItem::Class(class)], &config);
        assert!(
            stub.contains("def do_thing(self) -> bytes:"),
            "override by Rust struct name must match, got:\n{stub}"
        );
    }

    // ── add_header ───────────────────────────────────────────────────────────

    #[test]
    fn add_header_true_prepends_comment() {
        let mut config = Config::default();
        config.output.add_header = true;
        let f = make_fn("foo", None, vec![], syn::parse_quote! { () });
        let stub = stub_for_config(vec![PyItem::Function(f)], &config);
        assert!(
            stub.starts_with(AUTO_GENERATED_BANNER),
            "header should match stub_constants banner, got:\n{stub}"
        );
    }

    // ── Module-level constant (m.add → Final[...]) ───────────────────────────

    #[test]
    fn constant_emits_final_annotation_and_typing_import() {
        let c = PyConstant {
            name: "__version__".to_string(),
            py_type: "str".to_string(),
        };
        let stub = stub_for(vec![PyItem::Constant(c)]);
        assert!(
            stub.contains("__version__: t.Final[str]"),
            "stub should contain t.Final[str] annotation, got:\n{stub}"
        );
        assert!(
            stub.contains(TYPING_IMPORT_LINE),
            "stub should import typing when constant is present, got:\n{stub}"
        );
    }

    #[test]
    fn constant_with_other_typing_imports_includes_final() {
        let c = PyConstant {
            name: "VERSION".to_string(),
            py_type: "str".to_string(),
        };
        let f = make_fn("foo", None, vec![], syn::parse_quote! { () });
        let mut config = Config::default();
        config.output.python_version = "3.9".to_string();
        let stub = stub_for_config(vec![PyItem::Constant(c), PyItem::Function(f)], &config);
        assert!(stub.contains("VERSION: t.Final[str]"), "got:\n{stub}");
        assert!(
            stub.contains("t.Final") && stub.contains(TYPING_IMPORT_LINE),
            "stub should use t.Final and import typing, got:\n{stub}"
        );
    }

    #[test]
    fn add_header_false_omits_comment() {
        let mut config = Config::default();
        config.output.add_header = false;
        let f = make_fn("foo", None, vec![], syn::parse_quote! { () });
        let stub = stub_for_config(vec![PyItem::Function(f)], &config);
        assert!(
            !stub.starts_with(AUTO_GENERATED_BANNER),
            "header must be absent when add_header = false, got:\n{stub}"
        );
    }

    // ── gen_method: special MethodKinds ─────────────────────────────────────

    fn make_method(name: &str, kind: MethodKind, params: Vec<PyParam>, ret: syn::Type) -> PyMethod {
        PyMethod {
            rust_ident: name.to_string(),
            name: name.to_string(),
            doc: vec![],
            kind,
            signature_override: None,
            params,
            return_type: PyType {
                rust_type: ret,
                override_str: None,
            },
        }
    }

    fn make_class_with_methods(class_name: &str, methods: Vec<PyMethod>) -> PyClass {
        PyClass {
            name: class_name.to_string(),
            rust_name: class_name.to_string(),
            module: None,
            allows_python_subclass: false,
            extends: None,
            is_enum: false,
            doc: vec![],
            methods,
            source_file: dummy_path(),
        }
    }

    #[test]
    fn new_method_generates_init() {
        let m = make_method(
            "new",
            MethodKind::New,
            vec![make_param("x", syn::parse_quote! { i32 })],
            syn::parse_quote! { () },
        );
        let class = make_class_with_methods("Counter", vec![m]);
        let stub = stub_for(vec![PyItem::Class(class)]);
        assert!(
            stub.contains("@t.final"),
            "non-subclass pyclass must emit @t.final, got:\n{stub}"
        );
        assert!(
            stub.contains("def __init__(self, x: int) -> None:"),
            "#[new] must emit __init__, got:\n{stub}"
        );
    }

    #[test]
    fn gen_class_emits_extends_builtin_dict() {
        let class = PyClass {
            name: "MyDict".to_string(),
            rust_name: "MyDict".to_string(),
            module: None,
            allows_python_subclass: false,
            extends: Some(ExtendsSpec::Builtin("dict")),
            is_enum: false,
            doc: vec![],
            methods: vec![],
            source_file: dummy_path(),
        };
        let stub = stub_for(vec![PyItem::Class(class)]);
        assert!(stub.contains("@t.final"), "got:\n{stub}");
        assert!(stub.contains("class MyDict(dict):"), "got:\n{stub}");
    }

    #[test]
    fn gen_class_allows_python_subclass_omits_final() {
        let class = PyClass {
            name: "Base".to_string(),
            rust_name: "Base".to_string(),
            module: None,
            allows_python_subclass: true,
            extends: None,
            is_enum: false,
            doc: vec![],
            methods: vec![],
            source_file: dummy_path(),
        };
        let stub = stub_for(vec![PyItem::Class(class)]);
        assert!(
            !stub.contains("@t.final"),
            "subclass-enabled pyclass must not use @t.final, got:\n{stub}"
        );
        assert!(stub.contains("class Base:"), "got:\n{stub}");
    }

    #[test]
    fn gen_class_extends_known_pyclass_emits_base() {
        let class = PyClass {
            name: "Sub".to_string(),
            rust_name: "Sub".to_string(),
            module: None,
            allows_python_subclass: false,
            extends: Some(ExtendsSpec::PyClassRustName("Base".to_string())),
            is_enum: false,
            doc: vec![],
            methods: vec![],
            source_file: dummy_path(),
        };
        let mut known_classes = HashMap::new();
        known_classes.insert("Base".to_string(), "Base".to_string());
        known_classes.insert("Sub".to_string(), "Sub".to_string());
        let stub = generate_with_known_classes(
            &[make_module(vec![PyItem::Class(class)])],
            &Config::default(),
            &known_classes,
            &[],
            None,
        )
        .unwrap();
        assert!(stub.contains("class Sub(Base):"), "got:\n{stub}");
    }

    #[test]
    fn gen_class_extends_base_in_other_stub_emits_import() {
        let class = PyClass {
            name: "Sub".to_string(),
            rust_name: "Sub".to_string(),
            module: None,
            allows_python_subclass: false,
            extends: Some(ExtendsSpec::PyClassRustName("Base".to_string())),
            is_enum: false,
            doc: vec![],
            methods: vec![],
            source_file: dummy_path(),
        };
        let mut known_classes = HashMap::new();
        known_classes.insert("Base".to_string(), "Base".to_string());
        known_classes.insert("Sub".to_string(), "Sub".to_string());
        let mut class_mod = HashMap::new();
        class_mod.insert("Sub".to_string(), "pkg.sub".to_string());
        class_mod.insert("Base".to_string(), "pkg.base".to_string());
        let stub = generate_with_known_classes(
            &[make_module(vec![PyItem::Class(class)])],
            &Config::default(),
            &known_classes,
            &[],
            Some(("pkg.sub", &class_mod)),
        )
        .unwrap();
        assert!(
            stub.contains("from pkg.base import Base"),
            "expected import for extends base; got:\n{stub}"
        );
        assert!(stub.contains("class Sub(Base):"), "got:\n{stub}");
    }

    #[test]
    fn gen_class_enum_emits_final_without_base() {
        let class = PyClass {
            name: "Color".to_string(),
            rust_name: "Color".to_string(),
            module: None,
            allows_python_subclass: false,
            extends: None,
            is_enum: true,
            doc: vec![],
            methods: vec![],
            source_file: dummy_path(),
        };
        let stub = stub_for(vec![PyItem::Class(class)]);
        assert!(stub.contains("@t.final"), "got:\n{stub}");
        assert!(stub.contains("class Color:"), "got:\n{stub}");
    }

    #[test]
    fn new_method_with_signature_override_emits_kwargs() {
        // #[new] with #[pyo3(signature = (**kwargs))] — **kwargs are unpacked, so type is Any not dict.
        let mut m = make_method(
            "new",
            MethodKind::New,
            vec![make_param(
                "kwargs",
                syn::parse_quote! { Option<&pyo3::Bound<'_, pyo3::types::PyDict>> },
            )],
            syn::parse_quote! { () },
        );
        m.signature_override = Some("**kwargs".to_string());
        let class = make_class_with_methods("TfSettings", vec![m]);
        let stub = stub_for(vec![PyItem::Class(class)]);
        assert!(
            stub.contains("def __init__(self, **kwargs) -> None:"),
            "#[new] with signature (**kwargs) must emit __init__(self, **kwargs), got:\n{stub}"
        );
    }

    #[test]
    fn getter_method_generates_property() {
        let m = make_method(
            "value",
            MethodKind::Getter("value".to_string()),
            vec![],
            syn::parse_quote! { i32 },
        );
        let class = make_class_with_methods("Counter", vec![m]);
        let stub = stub_for(vec![PyItem::Class(class)]);
        assert!(
            stub.contains("@property"),
            "getter must emit @property, got:\n{stub}"
        );
        assert!(
            stub.contains("def value(self) -> int:"),
            "getter must emit def value(self), got:\n{stub}"
        );
    }

    #[test]
    fn getter_with_rename_uses_property_name() {
        let m = make_method(
            "get_count",
            MethodKind::Getter("count".to_string()),
            vec![],
            syn::parse_quote! { i32 },
        );
        let class = make_class_with_methods("Counter", vec![m]);
        let stub = stub_for(vec![PyItem::Class(class)]);
        assert!(
            stub.contains("def count(self) -> int:"),
            "renamed getter must use property name, got:\n{stub}"
        );
    }

    #[test]
    fn setter_method_generates_setter_decorator() {
        let m = make_method(
            "set_value",
            MethodKind::Setter("value".to_string()),
            vec![make_param("v", syn::parse_quote! { i32 })],
            syn::parse_quote! { () },
        );
        let class = make_class_with_methods("Counter", vec![m]);
        let stub = stub_for(vec![PyItem::Class(class)]);
        assert!(
            stub.contains("@value.setter"),
            "setter must emit @value.setter, got:\n{stub}"
        );
        assert!(
            stub.contains("def value(self, value: int) -> None:"),
            "setter must emit def value(self, value: T), got:\n{stub}"
        );
    }

    #[test]
    fn classmethod_generates_classmethod_decorator() {
        let m = make_method(
            "create",
            MethodKind::Class,
            vec![],
            syn::parse_quote! { Self },
        );
        let class = make_class_with_methods("MyClass", vec![m]);
        let stub = stub_for(vec![PyItem::Class(class)]);
        assert!(
            stub.contains("@classmethod"),
            "@classmethod decorator must appear, got:\n{stub}"
        );
        assert!(
            stub.contains("def create(self)"),
            "classmethod must include self param, got:\n{stub}"
        );
    }

    // ── gen_module: nested submodule ─────────────────────────────────────────

    #[test]
    fn nested_submodule_generates_class_namespace() {
        let inner = PyModule {
            name: "utils".to_string(),
            doc: vec![],
            items: vec![PyItem::Function(make_fn(
                "helper",
                None,
                vec![],
                syn::parse_quote! { i32 },
            ))],
            source_file: dummy_path(),
        };
        let stub = stub_for(vec![PyItem::Module(inner)]);
        assert!(
            stub.contains("class utils:"),
            "submodule must be emitted as class, got:\n{stub}"
        );
        // Functions inside a sub-namespace module are emitted as regular `def` (no `self`).
        assert!(
            stub.contains("def helper() -> int:"),
            "inner function must appear inside namespace, got:\n{stub}"
        );
    }

    // ── gen_docstring ─────────────────────────────────────────────────────────

    #[test]
    fn single_line_docstring_inline_triple_quotes() {
        let mut f = make_fn("described", None, vec![], syn::parse_quote! { () });
        f.doc = vec!["A brief description.".to_string()];
        let stub = stub_for(vec![PyItem::Function(f)]);
        assert!(
            stub.contains("\"\"\"A brief description.\"\"\""),
            "single-line doc must use inline triple quotes, got:\n{stub}"
        );
    }

    #[test]
    fn multi_line_docstring_uses_block_format() {
        let mut f = make_fn("described", None, vec![], syn::parse_quote! { () });
        f.doc = vec!["First line.".to_string(), "Second line.".to_string()];
        let stub = stub_for(vec![PyItem::Function(f)]);
        assert!(
            stub.contains("\"\"\""),
            "multi-line doc must start with triple quotes, got:\n{stub}"
        );
        assert!(
            stub.contains("First line."),
            "first doc line must appear, got:\n{stub}"
        );
        assert!(
            stub.contains("Second line."),
            "second doc line must appear, got:\n{stub}"
        );
        assert!(
            !stub.contains("\"\"\"First line.\"\"\""),
            "multi-line must not use inline format, got:\n{stub}"
        );
    }
}
