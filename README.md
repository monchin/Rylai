# Rylai

[![CI](https://github.com/monchin/Rylai/actions/workflows/ci.yml/badge.svg)](https://github.com/monchin/Rylai/actions/workflows/ci.yml)

Generate Python `.pyi` stub files from [pyo3](https://github.com/PyO3/pyo3)-annotated Rust source code — **statically, without compilation**.

## Features

- Parses `#[pymodule]`, `#[pyfunction]`, and `#[pyclass]` annotations directly from Rust source
- Maps Rust types to Python types automatically (`i32` → `int`, `Vec<T>` → `list[T]`, `Option<T>` → `T | None`, etc.)
- Extracts doc comments and emits them as Python docstrings
- Generates one `.pyi` file per top-level `#[pymodule]`; when classes use `#[pyclass(module = "...")]`, emits additional sibling `.pyi` files under `-o` (first module segment is implicit) so type checkers and runtime `__module__` agree
- Python-version-aware output (`T | None` for ≥ 3.10, `t.Optional[T]` for older; `t.Self` for ≥ 3.11; stubs always include `import typing as t` for consistent `[[add_content]]` placement)
- Optional `[[add_content]]`: inject extra Python (e.g. version branches, shared type aliases) into specific generated `.pyi` files by path under `-o`
- Zero-config by default; optionally configured via `rylai.toml`

## Why Rylai?

Compared with other tools that generate `.pyi` stubs for PyO3 projects, Rylai offers:

- **No compilation** — Rylai parses Rust source code directly (via [syn](https://github.com/dtolnay/syn)). You don’t need to build the crate or depend on compiled artifacts, so stub generation is fast and works even when the project doesn’t compile (e.g. missing native deps or wrong toolchain).
- **No code changes** — No need to add build scripts, `#[cfg]` blocks, or extra annotations to your Rust code. Point Rylai at your crate root and it reads existing `#[pymodule]` / `#[pyfunction]` / `#[pyclass]` as-is.
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
# Default: writes .pyi files into the example crate root (examples/pyo3_sample/)
cargo run -- examples/pyo3_sample

# Recommended for this repo: stubs next to the Python package (Maturin `python-source` layout)
cargo run -- examples/pyo3_sample --output examples/pyo3_sample/python/pyo3_sample

# Show help
cargo run -- --help
```

Anything after `--` is forwarded to the `rylai` binary.

### Example

The crate under `examples/pyo3_sample/` has two `#[pyclass]` types in submodules `pyo3_sample.aa` and `pyo3_sample.bb`, with methods that return the other type (to exercise cross-stub imports). After:

```bash
cargo run -- examples/pyo3_sample --output examples/pyo3_sample/python/pyo3_sample
```

you get `pyo3_sample.pyi` (top-level functions) together with `aa.pyi` and `bb.pyi` in the output directory. Submodule stubs use absolute imports such as `from pyo3_sample.bb import B` so Pyright/mypy resolve `pyo3_sample.aa` / `pyo3_sample.bb`. For example `aa.pyi` contains:

```python
from pyo3_sample.bb import B

class A:
    def make_b(self) -> B: ...
```

See `examples/pyo3_sample/src/lib.rs` for the full Rust source.

### Multi-module stubs (`#[pyclass(module = "...")]`)

If you annotate a class with PyO3’s `module` attribute, e.g. `#[pyclass(module = "abcd.efg")]`, Rylai will emit **multiple** `.pyi` files instead of a single flat stub. **`-o` is treated as the first segment of the Python module path** (the top-level `#[pymodule]` name). So for `-o stubs/` and pymodule `abcd`, submodule `abcd.efg` is written to **`stubs/efg.pyi`**, not `stubs/abcd/efg.pyi`. The root stub is **`stubs/abcd.pyi`**. Only deeper paths add folders after that first segment (e.g. `abcd.abcd.ff` → `abcd/ff.pyi` under `-o`; `abcd.abcd.abcd.gg` → `abcd/abcd/gg.pyi`). If `#[pyclass(module = "pkg.pkg")]` resolves to the same file as the root stub (`pkg.pyi`), those classes are merged into **`pkg.pyi`**. If no class has `module` set, behavior is unchanged: one `{name}.pyi` per top-level `#[pymodule]`. If everything is routed to submodules so the root stub would be empty, Rylai still writes `abcd.pyi` (possibly empty, but still carrying the pymodule docstring when present).

When `#[pyclass(module = "...")]` does **not** start with `{pymodule}.` (for example pymodule `_pkg` and `module = "pkg.abc"`), the extension name and the public Python package can differ: Rylai still collects items from that `#[pymodule]`, but emits those classes by dropping the **first** dotted segment of `module` (the public top-level package) and mirroring the rest under `-o` — e.g. `stubs/abc.pyi`, or `stubs/cba/foo.pyi` for `pkg.cba.foo`. `#[pyfunction]` and classes without `module` stay in `{pymodule}.pyi` (e.g. `stubs/_pkg.pyi`).

When using `--output` / `-o`, you can point either at a **parent** directory (e.g. `stubs/` → `stubs/abcd.pyi` and `stubs/efg.pyi`) or directly at the **package directory** whose name matches the top-level `#[pymodule]` (e.g. `-o python/abcd` → `python/abcd/abcd.pyi` and `python/abcd/efg.pyi`). If two different layout paths still resolve to the same output file, Rylai reports a duplicate-path error; point `-o` at a parent directory so distinct stubs land on different paths.

Runtime `__module__` for `pyfunction` / `m.add` may follow Maturin’s `module-name` (e.g. `abcd.abcd`), but Rylai still emits those symbols into the **top-level** `#[pymodule]` stub (`abcd.pyi`) together with any `#[pyclass]` that has **no** `module = "..."` attribute. Only classes with an explicit `#[pyclass(module = "...")]` are written to the matching submodule stub.

When a stub references a `#[pyclass]` type that is emitted in **another** submodule (e.g. a return type uses a class defined under `abcd.ff` while generating `ee.pyi`), Rylai prepends absolute imports such as `from abcd.ff import SomeClass` (after `typing` / `pathlib` imports) so Pyright and mypy can resolve the name.

## Configuration

You can configure rylai in either (or both) of these places:

- **`rylai.toml`** in the crate root
- **`[tool.rylai]`** in `pyproject.toml`

When both exist, duplicate keys are resolved in favor of `rylai.toml`; all other options from both files apply. Array tables (e.g. `[[override]]`, `[[add_content]]`, and their `[[tool.rylai.*]]` forms) are replaced as a whole by the same key in `rylai.toml`, not merged item-by-item. All sections are optional.

Example `rylai.toml`:

```toml
# Root-level keys (e.g. format) should appear before any [section] or [[array]] to avoid being parsed as part of a table.
# After generating .pyi files, run these commands with the generated .pyi paths appended.
# Only use when you trust this config file — commands are executed as configured.
# Each command must be executable (on PATH or use a full path); rylai will error if it cannot be run.
# Empty or whitespace-only entries are ignored.
# You may need "uvx ruff" or "uv/pdm run ruff" instead of "ruff"
format = ["ruff format", "ruff check --select I --fix"]

[output]
# Target Python version — affects t.Optional[T] vs T | None syntax (default: "3.10")
python_version = "3.10"

# Prepend auto-generated header comment (default: true)
add_header = true

[fallback]
# What to emit when a type cannot be resolved statically:
#   "any"   — emit t.Any and print a warning (default)
#   "error" — abort with an error
#   "skip"  — silently omit the item
strategy = "any"

[features]
# cfg features to treat as active during parsing
enabled = ["some_feature"]

[type_map]
# Custom Rust type → Python type overrides
"numpy::PyReadonlyArray1" = "numpy.ndarray"
"numpy::PyReadonlyArray2" = "numpy.ndarray"

[[override]]
# Single-line def/class header for a top-level item (Rust `///` doc is copied into the .pyi when present).
# You do not need a trailing `...`; Rylai adds `    ...` as the body when there is no Rust doc.
item = "my_module::complex_function"
stub = "def complex_function(x: t.Any, **kwargs: t.Any) -> dict[str, t.Any]:"

# #[pyclass] / #[pymethods] methods: same `[[override]]` mechanism as top-level items.
# `item` is `{logical_module}::{class}::{method}` — instance methods, @staticmethod, @classmethod,
# @property / setter, and #[new] when you want a custom __init__ line in the .pyi.
#
# `logical_module` (first segment) is the **Python module for the .pyi file where that class is
# emitted**, not “always the #[pymodule] name”. It matches how Rylai splits output:
#   - Classes without #[pyclass(module = "...")] (and all module-level functions/constants) use the
#     top-level #[pymodule] name, e.g. `pkg` for `pkg.pyi`.
#   - A class with #[pyclass(module = "pkg.abc")] is emitted into that submodule’s stub; use the
#     **exact** `module = "..."` string as the first segment, e.g. `pkg.abc::MyClass::method`,
#     not `pkg::...` (unless layout merges that stub into the root file — then the root module name
#     applies).
# `class` may be the Rust struct name or the Python #[pyclass(name = "...")] name. `method` is the
# Rust fn ident or #[pyo3(name = "...")]. For #[new], Rylai already emits `def __init__(...)`;
# override when you need a different signature (e.g. **kwargs: Unpack[...]). You may match with
# `...::__init__` as the method segment for #[new] only.
# Suffix form: `item` may end with `::Class::method` (no bare method name — ambiguous).
[[override]]
item = "my_module::Widget::reload"
stub = "def reload(self, force: bool = False) -> None:"

# Optional: splice raw Python into a generated file (path is relative to -o, use /; must match a stub emitted in this run)
[[add_content]]
file = "my_package/sub.pyi"
location = "after-import-typing" # head | tail | after-import-typing
content = """
from my_package._internal import KwargsItems
"""
```

For **class-method** overrides, the first segment of `item` is the logical Python module of the **stub file that contains that class**: usually the `#[pymodule]` name for the root `.pyi`, or the full `#[pyclass(module = "...")]` string (e.g. `pkg.abc`) when Rylai emits a separate submodule stub — not the pymodule name in that case.

`location`:

- `head` — inserted right after the `# Auto-generated by rylai...` banner (and its following blank line), suitable for file-level notes or comments. If `output.add_header` is false so that banner is absent, the snippet is inserted at the very beginning of the file.
- `after-import-typing` — inserted immediately after the first line `import typing as t` (every stub includes this line; use `ruff check --select F401 --fix` if you need to drop an unused import — `ruff format` does not remove imports).
- `tail` — appended at end of file.

If `file` does not match any `.pyi` path produced in that run, Rylai exits with an error. Omit the `.pyi` suffix only when the last path segment has no extension; otherwise use the same relative names as under `-o` (e.g. `pkg/aaa.pyi`).

For each `[[add_content]]` entry, if `content` does not already end with a newline, Rylai appends one so you do not need to write a trailing `\n` in TOML (you may still include it if you prefer).

The same options can be set in `pyproject.toml` under `[tool.rylai]`:

```toml
[tool.rylai.output]
python_version = "3.10"

[tool.rylai.fallback]
strategy = "any"

[tool.rylai.type_map]
"numpy::PyReadonlyArray1" = "numpy.ndarray"

[[tool.rylai.override]]
item = "my_module::complex_function"
stub = "def complex_function(x: t.Any, **kwargs: t.Any) -> dict[str, t.Any]:"

[[tool.rylai.add_content]]
file = "mymod.pyi"
location = "tail"
content = "X: t.TypeAlias = int"

[tool.rylai]
format = ["isort", "black"]
```

## Supported Type Mappings

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
| `Vec<T>` | `list[T]` |
| `(T1, T2, ...)` (non-empty tuple) | `tuple[T1, T2, ...]` |
| `HashMap<K,V>`, `BTreeMap<K,V>`, `IndexMap<K,V>` | `dict[K, V]` |
| `HashSet<T>`, `BTreeSet<T>` | `set[T]` |
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

Rylai is **purely static**: it parses Rust source for `#[pymodule]`, `#[pyfunction]`, `#[pyclass]`, etc., and does not run the compiler. It is therefore **not a good fit** for cases where concrete information is only known after compilation (e.g. declarative macros that generate Python bindings at compile time, or types/signatures that only exist in compiled artifacts).

For more complex projects, or when you rely on type/signature information that only exists after a build, a build-based approach is a better choice — for example [pyo3-stub-gen](https://github.com/jij-inc/pyo3-stub-gen), which compiles the extension first and then generates stubs from runtime/compilation artifacts.

For relatively simple projects where PyO3 bindings are mostly hand-annotated with straightforward types, Rylai’s **speed, no-compile workflow, and zero intrusion** are strong advantages. This is especially true when you have **Python version requirements** (e.g. supporting versions below 3.10 which pyo3-stub-gen does not support). In that case, avoid Python-related PyO3 code that is hard to analyze without compilation — for example **declarative macros** that expand at compile time and inject `#[pyfunction]` / `#[pyclass]`; Rylai cannot see those statically and may not generate the corresponding stubs correctly.

Related support is planned, but Rylai cannot generate stubs for all possible PyO3 code.

## Contributing

Before committing, run the pre-commit checks with [prek](https://github.com/j178/prek). See [CONTRIBUTING.md](CONTRIBUTING.md) for details.

## License

[LICENSE](LICENSE)
