//! PyO3 `async fn` (experimental-async) example for Rylai.
//!
//! Demonstrates the three async surfaces Rylai must render as Python `async def`:
//!   - a module-level `#[pyfunction] async fn`
//!   - an async instance / static method on a `#[pyclass]`
//!   - a `#[pyo3(cancel_handle)]` parameter, which pyo3 injects for cooperative
//!     cancellation and is invisible on the Python side (Rylai excludes it).
//!
//! Requires the `experimental-async` Cargo feature of pyo3 (see `Cargo.toml`). Build/run it
//! standalone; Rylai itself only parses this source statically.

use pyo3::coroutine::CancelHandle;
use pyo3::prelude::*;

#[pymodule]
mod async_await_sample {
    use super::*;

    /// A `#[pyclass]` exposing async methods.
    #[pyclass]
    pub struct AsyncWorker {
        #[pyo3(get)]
        factor: f64,
    }

    #[pymethods]
    impl AsyncWorker {
        /// `#[new]` stays synchronous: `def __init__(self, factor: float) -> None`.
        #[new]
        fn new(factor: f64) -> Self {
            Self { factor }
        }

        /// Async instance method -> `async def scale(self, n: int) -> float`.
        async fn scale(&self, n: i64) -> PyResult<f64> {
            Ok(self.factor * n as f64)
        }

        /// Async staticmethod -> `async def unit() -> AsyncWorker` (under `@staticmethod`).
        #[staticmethod]
        async fn unit() -> PyResult<Self> {
            Ok(Self { factor: 1.0 })
        }
    }

    /// Sleep for the given number of seconds, then return the value unchanged.
    ///
    /// Mirrors the pyo3 async-await guide: an `async fn` decorated with `#[pyfunction]` exposes a
    /// coroutine to Python, so the stub renders `async def sleep(seconds: float, value: int) -> int`.
    #[pyfunction]
    async fn sleep(seconds: f64, value: i64) -> PyResult<i64> {
        let duration = std::time::Duration::from_secs_f64(seconds);
        tokio::time::sleep(duration).await;
        Ok(value)
    }

    /// A cancellable coroutine.
    ///
    /// `#[pyo3(cancel_handle)]` injects a [`CancelHandle`] that the Rust body polls to cooperate
    /// with Python-side cancellation. It is NOT a Python-visible parameter, so Rylai drops it from
    /// the stub and the result is `async def cancellable() -> None:`.
    #[pyfunction]
    async fn cancellable(#[pyo3(cancel_handle)] mut cancel: CancelHandle) -> PyResult<()> {
        cancel.cancelled().await;
        Ok(())
    }
}
