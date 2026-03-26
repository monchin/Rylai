# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- `[[override]]` / `[[tool.rylai.override]]`: optional `param_types` and/or `return_type` (mutually exclusive with `stub`) to override parameter and/or return annotations on generated `def` lines; omitted parts still come from Rust. Keys normalize (`kwargs` / `**kwargs`). Applies to module-level functions and class methods; `#[new]` / property setters keep `-> None` regardless of `return_type`.

- `#[pyclass(extends = ...)]` / `#[pyo3(extends = ...)]`: stub `class` lines inherit from mapped PyO3 builtins (e.g. `PyDict` → `dict`) or from another `#[pyclass]` in the same crate (Python-visible name; adds `from ... import` when the base lives in another generated submodule). Unknown Rust bases emit a warning and omit the base class in the stub.
- `@t.final` on generated `#[pyclass]` stubs by default; omitted when the PyO3 **`subclass` flag** is set (`#[pyclass(..., subclass)]` or `#[pyo3(subclass)]`; see [PyO3 `pyclass` docs](https://docs.rs/pyo3/latest/pyo3/attr.pyclass.html)). `#[pyclass]` enums are always emitted as final. Cross-crate `extends` targets are not resolved; use simple type names for same-crate bases (matching is by the last path segment of the `extends` type, not a full module path).
- `[[add_content]]` / `[[tool.rylai.add_content]]`: inject raw Python into generated `.pyi` files by output path relative to `-o` (`file`), with `location` = `head` (after the auto-generated banner, or at file start if the banner is off), `after-import-typing`, or `tail`. Every configured `file` must match a stub path produced in the same run (otherwise Rylai errors).
- Support for `#[pyclass(module = "...")]`: when any class declares a Python submodule, Rylai emits multiple `.pyi` files under `-o` instead of a single flat stub. Layout treats the top-level `#[pymodule]` name as the first segment of the module path (sibling stubs such as `efg.pyi` for `pkg.efg`, with rules for nested paths and merging when a submodule maps to the same file as the root stub). Root stub may be empty except for the pymodule docstring when all classes are routed to submodules.
- `#[pymodule]` name and `#[pyclass(module = "...")]` may differ (e.g. internal extension module vs public package). Stub paths under `-o` use hybrid rules: when `module` starts with `{pymodule}.`, behavior matches the usual layout; otherwise the leading public package segment is dropped and the remainder is mirrored as files and directories (e.g. `pkg.abc` → `abc.pyi`, `pkg.cba.foo` → `cba/foo.pyi`).
- Absolute `from ... import ...` lines for cross-stub references: when a signature references a `#[pyclass]` emitted in another generated submodule, the stub prepends the import so Pyright/mypy resolve the type. Cross-module reference collection walks arrays, pointers, `impl Trait` bounds, and common generic wrappers (`Option`, `Vec`, `PyResult`, `Py`/`Bound`, maps/sets, etc.).
- Style A `#[pymodule]` modules: collect `m.add` / `m.add_function` / `m.add_class` from `#[pymodule_init]` bodies and from `Expr::Block` wrappers around those calls.
- `#[pyclass(get_all)]`, `#[pyclass(set_all)]`, and `#[pyclass(rename_all = "...")]` on struct classes: struct fields generate `@property` / setter stubs with PyO3-compatible renaming (camelCase, snake_case, kebab-case, etc.). Unknown `rename_all` literals keep Rust field names and emit a one-time warning.

### Changed

- `[[override]]` for a single-line top-level `def` or `class`: Rust doc comments on that item are emitted as the `.pyi` docstring; trailing `...` on the override line is stripped; when there is no Rust doc, Rylai appends `...` as the stub body so formatters stay happy. Multiline overrides and non-function/class items stay mostly verbatim (trimmed, trailing `...` still stripped as a suffix). **Migration:** if you relied on a single-line `def`/`class` override being pasted verbatim (including any `...` body on the same line), re-check those stubs after upgrading—behavior is now “header + doc/body” as above.
- Every generated stub now includes `import typing as t` (even when the body does not reference `t`), so `add_content` with `location = "after-import-typing"` always has a stable anchor. Remove unused imports with your formatter/linter if desired (e.g. `ruff check --select F401 --fix` — `ruff format` alone does not remove them).
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
