# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Support for reading config from `pyproject.toml` under `[tool.rylai]`. When both `rylai.toml` and `pyproject.toml` exist, configs are merged with `rylai.toml` taking precedence for duplicate keys.
- Optional `format` config: after generating `.pyi` files, run configured commands (e.g. `ruff format`, `black`) with the generated file paths appended. Commands must be executable; empty or whitespace-only entries are ignored. See README for security and usage notes.

## [0.1.0] - 2026-03-13

### Added

- Static generation of `.pyi` stubs from pyo3-annotated Rust source.
- Support for `#[pymodule]`, `#[pyfunction]`, `#[pyclass]` and `#[pymethods]`.
- Configurable behavior via `rylai.toml` (output, fallback, type_map, overrides).

[Unreleased]: https://github.com/monchin/Rylai/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/monchin/Rylai/releases/tag/v0.1.0
