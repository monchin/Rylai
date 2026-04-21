/// Collected representation of everything pyo3-exposed in a crate.
///
/// This is the intermediate format between raw AST and .pyi generation.
use std::path::PathBuf;

// ── Public item types ────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct PyModule {
    pub name: String,
    pub doc: Vec<String>,
    pub items: Vec<PyItem>,
    /// Source file for diagnostics (reserved for future use)
    #[allow(dead_code)]
    pub source_file: PathBuf,
}

#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum PyItem {
    Function(PyFunction),
    /// Module-level constant from `m.add("name", value)` (e.g. `__version__` from `env!(...)`).
    Constant(PyConstant),
    Class(PyClass),
    Module(PyModule),
}

/// A module-level constant added via `m.add("name", value)` in the pymodule.
#[derive(Debug, Clone)]
pub struct PyConstant {
    /// Python attribute name (e.g. `__version__`).
    pub name: String,
    /// Python type string for the .pyi (e.g. `str`, `int`).
    pub py_type: String,
}

#[derive(Debug, Clone)]
pub struct PyFunction {
    /// Python-visible name (`#[pyo3(name = "...")]` or the Rust `fn` ident when omitted).
    pub name: String,
    /// Rust `fn` identifier in source (e.g. `py_foo` while `name` is `foo`). Used for `[[override]]` matching.
    pub rust_name: String,
    pub doc: Vec<String>,
    /// If `#[pyo3(signature = (...))]` is present, this overrides params.
    pub signature_override: Option<String>,
    pub params: Vec<PyParam>,
    pub return_type: PyType,
    /// Source file for diagnostics (reserved for future use)
    #[allow(dead_code)]
    pub source_file: PathBuf,
}

#[derive(Debug, Clone)]
pub struct PyParam {
    pub name: String,
    pub ty: PyType,
    pub default: Option<String>,
    pub kind: ParamKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParamKind {
    Regular,
    #[allow(dead_code)]
    Args, // *args (reserved for Phase 3+)
    #[allow(dead_code)]
    Kwargs, // **kwargs (reserved for Phase 3+)
}

/// Base class for stubs: `#[pyclass(extends = ...)]`, or `create_exception!` (see [`ExtendsSpec::CreateExceptionBase`]).
///
/// Values are either a PyO3 builtin mapping (Python type name), a `create_exception!` base (see
/// [`ExtendsSpec::CreateExceptionBase`]), or another `#[pyclass]` in the crate (Rust struct name
/// for lookup in `known_classes`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtendsSpec {
    Builtin(&'static str),
    /// Python exception base name for `create_exception!` stubs (`class Name(` … `):`).
    ///
    /// Known `pyo3::exceptions::Py*` segments map to the stdlib name (e.g. `PyValueError` →
    /// `ValueError`). User-defined bases (chained `create_exception!(m, Child, BaseError)`) use
    /// the last segment verbatim (e.g. `BaseError`).
    CreateExceptionBase(String),
    PyClassRustName(String),
}

#[derive(Debug, Clone)]
pub struct PyClass {
    /// Python-visible class name (from `#[pyclass(name = "...")]` or the Rust struct name).
    pub name: String,
    /// Rust struct/enum identifier as written in the source (e.g. `PyPageIterator`).
    /// This is the name that appears in function return-type signatures and is needed
    /// to look up the Python name when the two differ.
    pub rust_name: String,
    /// If present, from `#[pyclass(module = "...")]`; used to emit the class into a separate .pyi (e.g. abcd.efg).
    pub module: Option<String>,
    /// True when `#[pyclass(subclass)]` / `#[pyo3(subclass)]` is set (structs only; PyO3 ignores for enums).
    pub allows_python_subclass: bool,
    /// `#[pyclass(extends = Base)]` — structs only; `None` for enums and when omitted (implicit `object`).
    pub extends: Option<ExtendsSpec>,
    /// PyO3 `#[pyclass]` on an enum — cannot subclass or extend in Python/Rust per PyO3 rules.
    pub is_enum: bool,
    pub doc: Vec<String>,
    pub methods: Vec<PyMethod>,
    /// Source file for diagnostics (reserved for future use)
    #[allow(dead_code)]
    pub source_file: PathBuf,
}

#[derive(Debug, Clone)]
pub struct PyMethod {
    /// Rust identifier from the `impl` item (`fn py_new` → `"py_new"`). Used for `[[override]]`
    /// keys; may differ from [`Self::name`] when `#[pyo3(name = "...")]` is set.
    pub rust_ident: String,
    pub name: String,
    pub doc: Vec<String>,
    pub kind: MethodKind,
    /// If `#[pyo3(signature = (...))]` is present, this overrides params (e.g. `**kwargs` for `#[new]`).
    pub signature_override: Option<String>,
    pub params: Vec<PyParam>,
    pub return_type: PyType,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MethodKind {
    Instance,
    Static,
    Class,
    New,            // #[new] → __init__
    Getter(String), // #[getter] → @property
    Setter(String), // #[setter]
}

/// A raw Rust type extracted from the AST, not yet mapped to Python.
/// We store the original syn::Type and also an optional custom override
/// from rylai.toml.
#[derive(Debug, Clone)]
pub struct PyType {
    /// The syn-parsed Rust type, kept for mapping in the generator step.
    pub rust_type: syn::Type,
    /// If the user provided a manual override in rylai.toml [type_map], store it here.
    pub override_str: Option<String>,
}
