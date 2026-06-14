//! PyO3 initializer-mode example.
//!
//! See <https://pyo3.rs/main/class.html#initializer>: when a class declares both
//! `#[new]` and a method literally named `__init__`, PyO3 registers `#[new]` as
//! Python `__new__` and `fn __init__` as `__init__`. Rylai accordingly emits
//! `__new__(cls, ...) -> Self` and `__init__(self, ...) -> None` separately,
//! rather than collapsing both into `__init__`.
//!
//! Real-world initializer scenarios typically extend a native type (e.g.
//! `#[pyclass(extends = PyDict)]`) to take over the initialization flow; this
//! sample omits the base class to keep the build stable and only demonstrates
//! the coexistence of `#[new]` and `__init__`, the trigger condition for the
//! Rylai fork.

use pyo3::prelude::*;

#[pymodule]
mod initializer_mode_sample {
    use pyo3::prelude::*;

    /// A class that carries both `#[new]` and an explicit `fn __init__` (PyO3 initializer mode).
    #[pyclass]
    pub struct WithInit;

    #[pymethods]
    impl WithInit {
        #[new]
        fn new(value: i32) -> Self {
            let _ = value;
            Self
        }

        fn __init__(&self, value: i32) -> PyResult<()> {
            let _ = value;
            Ok(())
        }
    }
}
