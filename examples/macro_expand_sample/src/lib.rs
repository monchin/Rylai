use pyo3::prelude::*;

/// Auto-discovered macro: Rylai finds `macro_rules! register_classes` in source and extracts
/// the pattern/body automatically.  A `let` binding avoids using the non-repeating `$m`
/// metavariable inside the `$(...)*` block (a `macro_rules_rt` requirement).
macro_rules! register_classes {
    ($m:expr, $($cls:ty),* $(,)?) => {
        { let m = $m; $(m.add_class::<$cls>()?;)* }
    };
}

/// A simple function so the module has a callable too.
#[pyfunction]
fn add(a: i64, b: i64) -> i64 {
    a + b
}

#[pyclass]
pub struct Foo;

#[pymethods]
impl Foo {
    #[new]
    fn new() -> Self {
        Self
    }

    fn foo(&self) -> &str {
        "foo"
    }
}

#[pyclass]
pub struct Bar;

#[pymethods]
impl Bar {
    #[new]
    fn new() -> Self {
        Self
    }

    fn bar(&self) -> &str {
        "bar"
    }
}

/// Explicit-mode macro: the `from`/`to` are provided directly in `rylai.toml` rather than
/// auto-discovered from source.  This is useful when the macro is defined in an external
/// crate or has patterns that auto-discovery cannot handle.
macro_rules! register_fn {
    ($m:expr, $fn_name:ident) => {
        $m.add_function(wrap_pyfunction!($fn_name, $m)?)?;
    };
}

#[pyclass]
pub struct Baz;

#[pymethods]
impl Baz {
    #[new]
    fn new() -> Self {
        Self
    }

    fn baz(&self) -> &str {
        "baz"
    }
}

#[pyclass]
pub struct Qux;

#[pymethods]
impl Qux {
    #[new]
    fn new() -> Self {
        Self
    }

    fn qux(&self) -> &str {
        "qux"
    }
}

/// The module entry point uses the function-style `#[pymodule]` so that we can call
/// custom macros for registration.  After macro expansion, Rylai sees plain
/// `m.add_class::<…>()` and `m.add_function(wrap_pyfunction!(…))` calls.
#[pymodule]
fn macro_expand_sample(m: &Bound<'_, PyModule>) -> PyResult<()> {
    // Auto-discover: `register_classes!` is expanded by extracting the arms from the
    // `macro_rules! register_classes` definition above.
    register_classes!(m, Foo, Bar);

    // Explicit: `register_fn!` is expanded using the `from`/`to` given in rylai.toml.
    register_fn!(m, add);

    // These classes are registered normally (no macro).
    m.add_class::<Baz>()?;
    m.add_class::<Qux>()?;

    Ok(())
}
