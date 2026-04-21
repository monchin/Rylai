use pyo3::prelude::*;

#[pymodule]
mod override_sample {
    use pyo3::prelude::*;
    use pyo3::types::PyDict;

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

    /// Multiply two values.
    /// Demonstrates overriding individual parameter types and return type.
    #[pyfunction]
    fn multiply(a: i64, b: i64) -> i64 {
        a.wrapping_mul(b)
    }
}
