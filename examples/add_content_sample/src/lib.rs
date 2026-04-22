use pyo3::prelude::*;

#[pymodule]
mod add_content_sample {
    use pyo3::prelude::*;

    /// Calculate the Euclidean distance between two 2D points.
    #[pyfunction]
    fn distance(x1: f64, y1: f64, x2: f64, y2: f64) -> f64 {
        let dx = x1 - x2;
        let dy = y1 - y2;
        (dx * dx + dy * dy).sqrt()
    }
}
