# Rylai

[![CI](https://github.com/monchin/Rylai/actions/workflows/ci.yml/badge.svg)](https://github.com/monchin/Rylai/actions/workflows/ci.yml)

Generate Python `.pyi` stub files from [pyo3](https://github.com/PyO3/pyo3)-annotated Rust source code — **statically, without compilation**.

## Features

- Parses `#[pymodule]`, `#[pyfunction]`, and `#[pyclass]` annotations directly from Rust source
- Maps Rust types to Python types automatically (`i32` → `int`, `Vec<T>` → `list[T]` or `t.List[T]` depending on `python_version`, `Option<T>` → `T | None` or `t.Optional[T]`, etc.)
- Extracts doc comments and emits them as Python docstrings
- Generates one `.pyi` file per top-level `#[pymodule]`; when classes use `#[pyclass(module = "...")]`, emits additional sibling `.pyi` files under `-o` (first module segment is implicit) so type checkers and runtime `__module__` agree
- Python-version-aware output (`T | None` for ≥ 3.10, `t.Optional[T]` for older; PEP 585 built-in generics `list[T]` / `dict[...]` / … for ≥ 3.9, `t.List` / `t.Dict` / … for 3.8; `t.Self` for ≥ 3.11; stubs always include `import typing as t` for consistent `[[add_content]]` placement)
- Generates `__all__` in every stub listing public top-level exports; names starting with `_` are excluded by default; configurable globally and per file via `[[all]]`
- Parses **`create_exception!`** / **`pyo3::create_exception!(module, Name, Base)`** ([PyO3 custom exceptions](https://pyo3.rs/main/exception)): emits matching `class Name(Base): ...` stubs in the pymodule file; the first macro argument must be the pymodule’s Python-visible name (`#[pymodule(name = "...")]`, `#[pyo3(name = "...")]`, or the Rust identifier). Builtin `pyo3::exceptions::Py*` bases map to stdlib exception names; chaining another `create_exception!` type uses that class name as the base
- Optional `[[add_content]]`: inject extra Python (e.g. version branches, shared type aliases) into specific generated `.pyi` files by path under `-o`; use `location = "file"` to create standalone `.pyi` files that don't correspond to any Rust module
- Optional `[[macro_expand]]`: expand configured `macro_rules!` invocations before AST parsing so wrapped `add_class` / `add_function` calls can be collected
- Zero-config by default; optionally configured via `rylai.toml`

## Why Rylai?

Compared with other tools that generate `.pyi` stubs for PyO3 projects, Rylai offers:

- **No compilation** — Rylai parses Rust source code directly (via [syn](https://github.com/dtolnay/syn)). You don’t need to build the crate or depend on compiled artifacts, so stub generation is fast and works even when the project doesn’t compile (e.g. missing native deps or wrong toolchain).
- **No code changes** — No need to add build scripts, `#[cfg]` blocks, or extra annotations to your Rust code. Point Rylai at your crate root and it reads existing `#[pymodule]` / `#[pyfunction]` / `#[pyclass]` and `create_exception!` macros as-is.
- **No Python version lock-in** — Stubs are plain text. You generate them once and use them with any Python version; there’s no dependency on a specific Python interpreter or ABI, so you avoid “built for Python 3.x” issues and cross-version workflows stay simple.

Together, this makes Rylai easy to integrate into CI, docs, or local dev without touching your PyO3 code or your Python environment.

## Installation

Choose one of the following:

| Method | Command | Notes |
|--------|---------|--------|
| **Cargo** | `cargo install rylai` | Build from source and install to `~/.cargo/bin` |
| **uv** | `uv tool install rylai` | Install to uv tools dir; requires [publish to PyPI](https://pypi.org/project/rylai/) first |
| **uvx** | `uvx rylai` | Run without installing (same as uv; requires PyPI release) |
| **crgx** | `crgx rylai` | Run pre-built binary without compiling; requires [crgx](https://github.com/yfedoseev/crgx) and a GitHub Release |

For local development:

```bash
cargo install --path .
```

## Usage

The path you pass is the **project root** — the folder that contains `Cargo.toml` (and usually a `src/` directory). Rylai scans all `.rs` files under that project’s `src/` and uses the root for `rylai.toml`, `pyproject.toml`, etc.

```bash
# Run in the current directory (must be the project root with Cargo.toml)
rylai

# Specify the project root explicitly (folder containing Cargo.toml)
rylai path/to/my_crate

# Write stubs to a custom output directory
rylai path/to/my_crate --output path/to/out/

# Use a custom config file
rylai --config path/to/rylai.toml
```

### For developers (this repo)

You don’t need to install the binary. Use **`cargo run`** and pass arguments after `--`:

```bash
# Run on a single example
cargo run -- examples/basic_function_sample --output examples/basic_function_sample/python/basic_function_sample

# Show help
cargo run -- --help
```

Anything after `--` is forwarded to the `rylai` binary.

### Examples

The `examples/` directory contains several self-contained sample projects, each demonstrating different Rylai features:

| Example | What it demonstrates |
|---|---|
| `add_content_sample` | `[[add_content]]` with `tail` location and `location = "file"` to create standalone `.pyi` files |
| `basic_function_sample` | `#[pyfunction]`, `#[pyo3(name = "...")]` rename, `#[pyclass]`, `create_exception!` |
| `cross_module_sample` | `#[pyclass(module = "...")]` with cross-module imports, `[[add_content]]` |
| `override_sample` | `[[override]]` via `stub` and via `param_types` / `return_type`, `[[add_content]]` |
| `macro_expand_sample` | `[[macro_expand]]` auto-discover and explicit modes |

#### Regenerating all example stubs

Install [just](https://github.com/casey/just), then run:

```bash
just gen-pyi-examples
```

This regenerates `.pyi` files for every example. CI checks that the committed stubs match the generated output; if they differ, CI fails.

### Multi-module stubs (`#[pyclass(module = "...")]`)

If you annotate a class with PyO3’s `module` attribute, e.g. `#[pyclass(module = "abcd.efg")]`, Rylai will emit **multiple** `.pyi` files instead of a single flat stub. **`-o` is treated as the first segment of the Python module path** (the top-level `#[pymodule]` name). So for `-o stubs/` and pymodule `abcd`, submodule `abcd.efg` is written to **`stubs/efg.pyi`**, not `stubs/abcd/efg.pyi`. The root stub is **`stubs/abcd.pyi`**. Only deeper paths add folders after that first segment (e.g. `abcd.abcd.ff` → `abcd/ff.pyi` under `-o`; `abcd.abcd.abcd.gg` → `abcd/abcd/gg.pyi`). If `#[pyclass(module = "pkg.pkg")]` resolves to the same file as the root stub (`pkg.pyi`), those classes are merged into **`pkg.pyi`**. If no class has `module` set, behavior is unchanged: one `{name}.pyi` per top-level `#[pymodule]`. If everything is routed to submodules so the root stub would be empty, Rylai still writes `abcd.pyi` (possibly empty, but still carrying the pymodule docstring when present).

When `#[pyclass(module = "...")]` does **not** start with `{pymodule}.` (for example pymodule `_pkg` and `module = "pkg.abc"`), the extension name and the public Python package can differ: Rylai still collects items from that `#[pymodule]`, but emits those classes by dropping the **first** dotted segment of `module` (the public top-level package) and mirroring the rest under `-o` — e.g. `stubs/abc.pyi`, or `stubs/cba/foo.pyi` for `pkg.cba.foo`. `#[pyfunction]` and classes without `module` stay in `{pymodule}.pyi` (e.g. `stubs/_pkg.pyi`).

When using `--output` / `-o`, you can point either at a **parent** directory (e.g. `stubs/` → `stubs/abcd.pyi` and `stubs/efg.pyi`) or directly at the **package directory** whose name matches the top-level `#[pymodule]` (e.g. `-o python/abcd` → `python/abcd/abcd.pyi` and `python/abcd/efg.pyi`). If two different layout paths still resolve to the same output file, Rylai reports a duplicate-path error; point `-o` at a parent directory so distinct stubs land on different paths.

Runtime `__module__` for `pyfunction` / `m.add` may follow Maturin’s `module-name` (e.g. `abcd.abcd`), but Rylai still emits those symbols into the **top-level** `#[pymodule]` stub (`abcd.pyi`) together with any `#[pyclass]` that has **no** `module = "..."` attribute. Only classes with an explicit `#[pyclass(module = "...")]` are written to the matching submodule stub.

When a stub references a `#[pyclass]` type that is emitted in **another** submodule (e.g. a return type uses a class defined under `abcd.ff` while generating `ee.pyi`), Rylai prepends absolute imports such as `from abcd.ff import SomeClass` (after `typing` / `pathlib` imports) so Pyright and mypy can resolve the name.

## Configuration

Configure via **`rylai.toml`** (crate root) or **`[tool.rylai]`** in `pyproject.toml`. When both exist, duplicate keys in `rylai.toml` take precedence; array tables (`[[override]]`, `[[add_content]]`, `[[all]]`, `[[macro_expand]]`) are replaced as a whole, not merged item-by-item. All sections are optional.

Root-level keys (e.g. `format`) should appear before any `[section]` or `[[array]]` to avoid being parsed as part of a table.

### `format`

Commands to run after generating `.pyi` files. Generated file paths are appended to each command. Only use when you trust this config file.

```toml
format = ["ruff format", "ruff check --select I --fix"]
```

Each command must be executable (on PATH or use a full path). Empty entries are ignored. You may need `"uvx ruff"` or `"uv run ruff"` instead of `"ruff"` depending on your setup.

See any `examples/*/rylai.toml` for a working example.

### `[output]`

| Key | Default | Description |
|-----|---------|-------------|
| `python_version` | `"3.10"` | Target Python version. Affects `t.Optional[T]` vs `T \| None` (3.10+), PEP 585 built-in generics (3.9+), `t.Self` vs class name (3.11+) |
| `add_header` | `true` | Prepend `# Auto-generated by rylai…` banner |
| `all_include_private` | `false` | Include `_`-prefixed names in `__all__`. Overridable per file via `[[all]]` |

### `[fallback]`

What to emit when a type cannot be resolved statically:

| Strategy | Behavior |
|----------|----------|
| `"any"` | Emit `t.Any` and print a warning *(default)* |
| `"error"` | Abort with an error |
| `"skip"` | Silently omit the item |

### `[features]`

`cfg` features to treat as active during parsing.

```toml
[features]
enabled = ["some_feature"]
```

### `[type_map]`

Custom Rust type → Python type overrides. Keys must be Rust **path types** — a single identifier (e.g. `PyBbox`) or a qualified path (e.g. `crate::types::MyHandle`).

```toml
[type_map]
"numpy::PyReadonlyArray1" = "numpy.ndarray"
"PyColor" = "Color"
```

**Limitations:**
- Anonymous tuple types (e.g. `(u8, u8, u8, u8)`) cannot be keys. Define a `type` alias in Rust, use it in signatures, then map the alias name.
- A Rust `type` alias listed here is preserved during expansion (not resolved), so nested uses like `Vec<ThatAlias>` still map correctly.
- Two keys sharing the same last path segment but different Python types cause a warning; the short-name lookup is skipped.

### `[[override]]`

Replace generated signatures with custom ones. Two modes:

**`stub` — full signature line:**

```toml
[[override]]
item = "my_module::complex_function"
stub = "def complex_function(x: t.Any, **kwargs: t.Any) -> dict[str, t.Any]:"
```

**`param_types` / `return_type` — partial override (mutually exclusive with `stub`):**

Rylai still builds `def ...` from Rust and `#[pyo3(signature)]`; you only override the specified parts.

```toml
[[override]]
item = "my_module::f"
param_types = { "**kwargs" = "Unpack[KwargsItems]" }
return_type = "dict[str, t.Any]"
```

**`item` path format:**

- Module-level: `{module}::{function}`
- Class method: `{module}::{class}::{method}`

`{module}` is the **logical Python module of the stub file** — the top-level `#[pymodule]` name for the root `.pyi`, or the full `#[pyclass(module = "...")]` string for submodule stubs (e.g. `pkg.abc::MyClass::method`). `class` may be the Rust struct name or `#[pyclass(name = "...")]`. `method` is the Rust `fn` ident or `#[pyo3(name = "...")]`. For `#[new]`, use `...::__init__` as the method segment.

See `examples/override_sample/` for a working example.

### `[[add_content]]`

Inject raw Python into generated `.pyi` files. `file` is relative to `-o`, use `/`.

| `location` | Behavior |
|-----------|----------|
| `head` | After the auto-generated banner (or file start if `add_header = false`) |
| `after-import-typing` | After the `import typing as t` line |
| `tail` | End of file |
| `file` | Write `content` as the **complete** file — no banner or imports added. Only one entry per file; can create standalone `.pyi` files |

```toml
[[add_content]]
file = "my_package/sub.pyi"
location = "after-import-typing"
content = """
from my_package._internal import KwargsItems
"""
```

For `head`, `after-import-typing`, and `tail`: `file` must match a `.pyi` path produced in that run. If `content` doesn't end with a newline, Rylai appends one.

See `examples/add_content_sample/` (tail + file), `examples/override_sample/` (after-import-typing), and `examples/cross_module_sample/` for working examples.

### `[[macro_expand]]`

Expand `macro_rules!` invocations before AST parsing so wrapped `add_class` / `add_function` calls can be collected.

**Mode A — explicit pattern/transcription:**

```toml
[[macro_expand]]
name = "add_pymodule"
from = '$py:expr, $parent:expr, $name:expr, [$($cls:ty),* $(,)?]'
to = '{ let sub = pyo3::types::PyModule::new($py, $name)?; $(sub.add_class::<$cls>()?;)* $parent.add_submodule(&sub)?; Ok::<_, pyo3::PyErr>(()) }'
```

**Mode B — auto-discover from Rust source:**

```toml
[[macro_expand]]
name = "register_classes"
```

Mode B searches source files for `macro_rules! {name}` and extracts the pattern/body automatically. Best-effort: duplicate macro names across files use the first match; unparsable `.rs` files are skipped.

See `examples/macro_expand_sample/` for a working example.

### `[[all]]`

Per-file `__all__` overrides. Paths relative to `-o`, same convention as `[[add_content]]`. Multiple entries for the same file are merged (`include`/`exclude` sets unioned; last `include_private` wins).

```toml
[[all]]
file = "my_package.pyi"
include_private = true
include = ["_special_export"]
exclude = ["InternalHelper"]
```

**Priority (highest first):**

1. Per-file `exclude` — always removed from `__all__`
2. Per-file `include` — force-included even if `_`-prefixed (only for symbols Rylai actually generated)
3. Per-file `include_private` — overrides global setting
4. Global `output.all_include_private`
5. Default (`false`): `_`-prefixed names excluded

Names added via `[[add_content]]` are **not** automatically included in `__all__`.

### `pyproject.toml`

All options are available under `[tool.rylai]`:

| `rylai.toml` | `pyproject.toml` |
|---|---|
| `format` | `[tool.rylai] format` |
| `[output]` | `[tool.rylai.output]` |
| `[fallback]` | `[tool.rylai.fallback]` |
| `[type_map]` | `[tool.rylai.type_map]` |
| `[[override]]` | `[[tool.rylai.override]]` |
| `[[add_content]]` | `[[tool.rylai.add_content]]` |
| `[[macro_expand]]` | `[[tool.rylai.macro_expand]]` |
| `[[all]]` | `[[tool.rylai.all]]` |

```toml
[tool.rylai]
format = ["ruff format", "ruff check --select I --fix"]

[tool.rylai.output]
python_version = "3.10"

[tool.rylai.type_map]
"numpy::PyReadonlyArray1" = "numpy.ndarray"

[[tool.rylai.override]]
item = "my_module::complex_function"
stub = "def complex_function(x: t.Any, **kwargs: t.Any) -> dict[str, t.Any]:"

[[tool.rylai.macro_expand]]
name = "register_classes"

[[tool.rylai.add_content]]
file = "mymod.pyi"
location = "tail"
content = "X: t.TypeAlias = int"

[[tool.rylai.all]]
file = "mymod.pyi"
exclude = ["InternalHelper"]
```

## Supported Type Mappings

For **`[output] python_version`**: generic containers follow [PEP 585](https://peps.python.org/pep-0585/) only on **Python ≥ 3.9** (`list[T]`, `dict[K, V]`, `tuple[...]`, `set[T]`). On **3.8**, Rylai emits the equivalent **`typing`** forms with the usual stub alias: `t.List[T]`, `t.Dict[K, V]`, `t.Tuple[...]`, `t.Set[T]` (stubs use `import typing as t`). Bare `list` / `dict` / `set` without type parameters stay as built-in names.

| Rust type | Python type |
|---|---|
| **Scalars** | |
| `i8` … `i128`, `u8` … `u128`, `isize`, `usize` | `int` |
| `f32`, `f64` | `float` |
| `bool` | `bool` |
| `str`, `String`, `char` | `str` |
| `()` | `None` |
| **Bytes** | |
| `&[u8]`, `[u8]` | `bytes` |
| `Vec<u8>` | `bytes` |
| **Path-like** | |
| `Path`, `PathBuf` (incl. `std::path::*`) | `Path \| str` / `t.Union[Path, str]` |
| **Containers** | |
| `Option<T>` | `T \| None` / `t.Optional[T]` |
| `Vec<T>` | `list[T]` (3.9+) / `t.List[T]` (3.8) |
| `(T1, T2, ...)` (non-empty tuple) | `tuple[...]` (3.9+) / `t.Tuple[...]` (3.8) |
| `HashMap<K,V>`, `BTreeMap<K,V>`, `IndexMap<K,V>` | `dict[K, V]` (3.9+) / `t.Dict[K, V]` (3.8) |
| `HashSet<T>`, `BTreeSet<T>` | `set[T]` (3.9+) / `t.Set[T]` (3.8) |
| **PyO3 types** | |
| `PyResult<T>`, `Result<T, E>` | `T` (errors become Python exceptions) |
| `Py<T>`, `Bound<T>`, `Borrowed<T>` | recurse into `T` |
| `PyRef<T>`, `PyRefMut<T>` | recurse into `T` |
| `PyBytes` | `bytes` |
| `PyByteArray` | `bytearray` |
| `PyString` | `str` |
| `PyDict`, `PyList`, `PyTuple`, `PySet` | `dict`, `list`, `tuple`, `set` |
| `PyAny`, `PyObject` | `t.Any` |
| **Other** | |
| `Self` (in `#[pymethods]`) | `t.Self` (py ≥ 3.11) or class name |
| `#[pyclass]` structs/enums | Python class name (from crate) |
| Unknown types | `t.Any` (configurable via `[fallback]`) |

## Limitation

Rylai is **purely static**: it parses Rust source for `#[pymodule]`, `#[pyfunction]`, `#[pyclass]`, etc., and does not run the compiler. It is therefore **not a good fit** for cases where concrete information is only known after compilation (e.g. procedural macros that generate Python bindings at compile time, or types/signatures that only exist in compiled artifacts).

### Current limitations

- **Same-name items.** Rylai keys most internal lookups (functions, classes, type aliases, impl blocks, struct fields) by bare identifier. If two items in different modules share the same Rust name (e.g. two `struct Foo`), the last one parsed silently wins — fields, methods, and attributes from earlier definitions are lost. Use unique names or `#[pyclass(name = "...")]` to disambiguate.
- **`use … as …` renamed imports.** Rylai does not resolve import aliases. Code like `use pyo3::prelude::PyResult as MyResult;` followed by `-> MyResult<T>` will cause `MyResult` to be treated as an unknown type (falling back to `t.Any`). Use the original name or add a `type` alias + `[type_map]` entry instead.
- **`create_exception!` parsing.** Parsing expects exactly three comma-separated macro arguments; if the exception base path contains generics (`<...>`), comma splitting may fail.
- **`[[macro_expand]]` repetition blocks.** Due to a `macro_rules_rt` limitation, `$(...)*` blocks can only contain repeating variables (e.g. `$cls`). Non-repeating metavariables inside a repetition are not expanded. The standard workaround is to bind non-repeating variables outside the repetition with a `let` (see the `macro_expand_sample` example).

### When to use a build-based tool instead

For more complex projects, or when you rely on type/signature information that only exists after a build, a build-based approach is a better choice — for example [pyo3-stub-gen](https://github.com/jij-inc/pyo3-stub-gen), which compiles the extension first and then generates stubs from runtime/compilation artifacts.

For relatively simple projects where PyO3 bindings are mostly hand-annotated with straightforward types, Rylai’s **speed, no-compile workflow, and zero intrusion** are strong advantages. This is especially true when you have **Python version requirements** (e.g. supporting versions below 3.10 which pyo3-stub-gen does not support). For declarative macros that wrap binding registration calls, you can now use `[[macro_expand]]` / `[[tool.rylai.macro_expand]]` to expand specific macros before parsing. Coverage is still opt-in and pattern-driven: unsupported macro patterns or unconfigured macros may still be missed.

Related support is planned, but Rylai cannot generate stubs for all possible PyO3 code.

## Contributing

Before committing, run the pre-commit checks with [prek](https://github.com/j178/prek). See [CONTRIBUTING.md](CONTRIBUTING.md) for details.

## License

[LICENSE](LICENSE)
