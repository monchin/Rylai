# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Support for `#[pyclass(module = "...")]`: when any class declares a Python submodule, Rylai emits multiple `.pyi` files under `-o` instead of a single flat stub. Layout treats the top-level `#[pymodule]` name as the first segment of the module path (sibling stubs such as `efg.pyi` for `pkg.efg`, with rules for nested paths and merging when a submodule maps to the same file as the root stub). Root stub may be empty except for the pymodule docstring when all classes are routed to submodules.
- `#[pymodule]` name and `#[pyclass(module = "...")]` may differ (e.g. internal extension module vs public package). Stub paths under `-o` use hybrid rules: when `module` starts with `{pymodule}.`, behavior matches the usual layout; otherwise the leading public package segment is dropped and the remainder is mirrored as files and directories (e.g. `pkg.abc` → `abc.pyi`, `pkg.cba.foo` → `cba/foo.pyi`).
- Absolute `from ... import ...` lines for cross-stub references: when a signature references a `#[pyclass]` emitted in another generated submodule, the stub prepends the import so Pyright/mypy resolve the type. Cross-module reference collection walks arrays, pointers, `impl Trait` bounds, and common generic wrappers (`Option`, `Vec`, `PyResult`, `Py`/`Bound`, maps/sets, etc.).
- Style A `#[pymodule]` modules: collect `m.add` / `m.add_function` / `m.add_class` from `#[pymodule_init]` bodies and from `Expr::Block` wrappers around those calls.
- `#[pyclass(get_all)]`, `#[pyclass(set_all)]`, and `#[pyclass(rename_all = "...")]` on struct classes: struct fields generate `@property` / setter stubs with PyO3-compatible renaming (camelCase, snake_case, kebab-case, etc.). Unknown `rename_all` literals keep Rust field names and emit a one-time warning.

### Changed

- Generated stubs now use `import typing as t` and qualified annotations (`t.Any`, `t.Optional[...]`, `t.Union[...]`, `t.Self`, `t.Final[...]`) instead of `from typing import ...`, so extending typing usage in emitted `.pyi` files stays straightforward.
- Example `examples/pyo3_sample` now uses a `python/` tree with Maturin `pyproject.toml`, submodule classes `A` / `B` with cross-stub imports, and an unscoped class `C` on the root stub.

## [0.2.0] - 2026-03-14

### Added

- Support for reading config from `pyproject.toml` under `[tool.rylai]`. When both `rylai.toml` and `pyproject.toml` exist, configs are merged with `rylai.toml` taking precedence for duplicate keys.
- Optional `format` config: after generating `.pyi` files, run configured commands (e.g. `ruff format`, `black`) with the generated file paths appended. Commands must be executable; empty or whitespace-only entries are ignored. See README for security and usage notes.

## [0.1.0] - 2026-03-13

### Added

- Static generation of `.pyi` stubs from pyo3-annotated Rust source.
- Support for `#[pymodule]`, `#[pyfunction]`, `#[pyclass]` and `#[pymethods]`.
- Configurable behavior via `rylai.toml` (output, fallback, type_map, overrides).

[Unreleased]: https://github.com/monchin/Rylai/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/monchin/Rylai/releases/tag/v0.2.0
[0.1.0]: https://github.com/monchin/Rylai/releases/tag/v0.1.0
