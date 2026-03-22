use pyo3::prelude::*;

/// A Python module implemented in Rust.
#[pymodule]
mod pyo3_sample {
    /// Formats the sum of two numbers as string.
    #[pyfunction]
    fn sum_as_string(a: usize, b: usize) -> PyResult<String> {
        Ok((a + b).to_string())
    }

    /// Renamed via #[pyo3(name = "...")]
    #[pyfunction]
    #[pyo3(name = "add")]
    fn rust_add(a: i64, b: i64) -> i64 {
        a + b
    }

    #[pyclass(module = "pyo3_sample.aa")]
    pub struct A;

    #[pymethods]
    impl A {
        #[new]
        fn new() -> Self {
            Self
        }

        /// Returns a `B` from `pyo3_sample.bb` (Rylai emits `from pyo3_sample.bb import B` in `aa` stub).
        fn make_b(&self) -> B {
            B::new()
        }
    }

    #[pyclass(module = "pyo3_sample.bb")]
    pub struct B;

    #[pymethods]
    impl B {
        #[new]
        fn new() -> Self {
            Self
        }
    }

    #[pyclass]
    pub struct C;

    #[pymethods]
    impl C {
        #[new]
        fn new() -> Self {
            Self
        }
    }
}
