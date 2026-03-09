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
    Class(PyClass),
    Module(PyModule),
}

#[derive(Debug, Clone)]
pub struct PyFunction {
    pub name: String,
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

#[derive(Debug, Clone)]
pub struct PyClass {
    pub name: String,
    pub doc: Vec<String>,
    pub methods: Vec<PyMethod>,
    /// Source file for diagnostics (reserved for future use)
    #[allow(dead_code)]
    pub source_file: PathBuf,
}

#[derive(Debug, Clone)]
pub struct PyMethod {
    pub name: String,
    pub doc: Vec<String>,
    pub kind: MethodKind,
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
