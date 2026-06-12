## Why

Rylai 的 `is_pyo3_injected_param` 函数（`src/collector/parse.rs`）未能识别三种 PyO3 receiver 模式——`Py<Self>`、`&Bound<'_, Self>` 和 `Bound<'_, Self>`——作为隐式的 `self` 参数。当 `#[pymethods]` 方法使用这些 receiver 时，参数名（如 `slf`、`slf_handle`）会泄漏到生成的 `.pyi` stub 中，与已注入的 `self` 并列，产生错误签名。这导致 mypy/pyright 对正确的运行时调用报类型错误。魔术方法（`__iter__`、`__getitem__` 等）尤其受影响，因为 PyO3 禁止在它们上使用 `#[pyo3(signature = (...))]`，没有 workaround。

## What Changes

- 扩展 `is_pyo3_injected_param` 以识别 `Py<Self>` 作为注入的 receiver（需检查泛型参数是 `Self`，而非任意 `T`）。
- 扩展 `is_pyo3_injected_param` 以识别 `Bound<'_, Self>` 和 `&Bound<'_, Self>` 作为注入的 receiver。
- 添加共享辅助函数 `generic_arg_is_self`，检查类型的泛型参数是否包含 `Self`。
- 添加单元测试覆盖所有三个新识别的 receiver 模式，包括带额外参数和魔术方法的情况。

## Capabilities

### New Capabilities

- `self-receiver-detection`：覆盖全部六种 PyO3 receiver 类型（`&self`、`&mut self`、`PyRef<'_, Self>`、`PyRefMut<'_, Self>`、`Py<Self>`、`&Bound<'_, Self>` / `Bound<'_, Self>`）在生成的 Python stub 中的正确排除。

### Modified Capabilities

_（无——没有已有 spec）_

## Non-goals

- 处理解析为 `Py<Self>` 的类型别名（如 `type Handle = Py<Self>`）。用户可通过 `#[pyo3(signature = (...))]` workaround。
- 处理 `Py<ConcreteClassName>`，其中 `ConcreteClassName` 是显式写出的类名而非 `Self` 关键字。`Self` 是惯用且足够的写法。
- 对 CLI、config、generator 或 output layout 层的修改。修复仅限于 collector/parser 层。

## Impact

- **Parser**（`src/collector/parse.rs`）：修改 `is_pyo3_injected_param` 函数；新增辅助函数。
- **Tests**（`src/collector/parse.rs` 测试模块）：为 `Py<Self>`、`Bound<'_, Self>` 和 `&Bound<'_, Self>` receiver 添加新测试用例。
- **下游影响**：无破坏性变更——stub 只是变得更正确了。
