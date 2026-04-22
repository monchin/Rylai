use pyo3::prelude::*;

#[pymodule]
mod cross_module_sample {
    use pyo3::prelude::*;

    #[pyclass(module = "cross_module_sample.aa")]
    pub struct A;

    #[pymethods]
    impl A {
        #[new]
        fn new() -> Self {
            Self
        }

        /// Returns a `B` from `cross_module_sample.bb` (Rylai emits `from cross_module_sample.bb import B` in `aa` stub).
        fn make_b(&self) -> B {
            B::new()
        }
    }

    #[pyclass(module = "cross_module_sample.bb")]
    pub struct B;

    #[pymethods]
    impl B {
        #[new]
        fn new() -> Self {
            Self
        }
    }
}
