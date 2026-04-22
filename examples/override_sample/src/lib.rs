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

    /// Apply a decision policy to predefined items.
    /// Demonstrates overriding parameter and return types for richer Python type hints.
    #[pyfunction]
    fn apply_policy(py: Python<'_>, mode: &str) -> PyResult<Py<PyDict>> {
        let items = ["config_a", "config_b", "config_c"];
        let valid = ["accept", "deny", "auto"];
        if !valid.contains(&mode) {
            return Err(PyErr::new::<pyo3::exceptions::PyValueError, _>(format!(
                "Invalid mode: {mode}. Expected one of: accept, deny, auto"
            )));
        }
        let dict = PyDict::new(py);
        for item in &items {
            dict.set_item(*item, mode)?;
        }
        Ok(dict.unbind())
    }
}
