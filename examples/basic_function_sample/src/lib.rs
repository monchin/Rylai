use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;

pyo3::create_exception!(basic_function_sample, SampleError, PyValueError);

#[pymodule]
mod basic_function_sample {
    use pyo3::prelude::*;

    #[pymodule_export]
    use super::SampleError;

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
