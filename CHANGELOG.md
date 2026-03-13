# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Static generation of `.pyi` stubs from pyo3-annotated Rust source.
- Support for `#[pymodule]`, `#[pyfunction]`, `#[pyclass]` and `#[pymethods]`.
- Configurable behavior via `rylai.toml` (output, fallback, type_map, overrides).
- Python-version-aware output (`T | None` vs `Optional[T]`).
