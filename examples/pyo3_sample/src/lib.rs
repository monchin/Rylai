use pyo3::prelude::*;

/// A Python module implemented in Rust.
#[pymodule]
mod pyo3_sample {
    use pyo3::prelude::*;
    use pyo3::types::PyDict;

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

    /// Example of using "add_content" and "override" in rylai.toml to show the types of kwargs.
    #[pyfunction]
    #[pyo3(signature = (**kwargs))]
    fn show_kwargs(py: Python<'_>, kwargs: Option<&Bound<'_, PyDict>>) -> PyResult<()> {
        let Some(kwargs) = kwargs else {
            return Ok(());
        };
        let print_fn = py.import("builtins")?.getattr("print")?;
        for (key, value) in kwargs.iter() {
            let key = key.to_string();
            let line = match key.as_str() {
                "a" => {
                    let a: i64 = value.extract()?;
                    format!("a: {a}")
                }
                "b" => {
                    let b: String = value.extract()?;
                    format!("b: {b}")
                }
                "c" => {
                    let c: (String, bool) = value.extract()?;
                    format!("c: {c:?}")
                }
                _ => format!("unknown key: {key}"),
            };
            print_fn.call1((line,))?;
        }
        Ok(())
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
