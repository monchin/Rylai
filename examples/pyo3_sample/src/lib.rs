use pyo3::prelude::*;

/// A Python module implemented in Rust.
#[pymodule]
mod pyo3_sample {
    use pyo3::prelude::*;

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
}
