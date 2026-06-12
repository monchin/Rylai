## Context

Rylai 的 `is_pyo3_injected_param` 函数（`src/collector/parse.rs:1940-1972`）决定 `#[pymethods]` 函数中的 `FnArg::Typed` 参数是否应从生成的 Python stub 中排除。当前它能识别 `Python<'_>`、`PyRef<'_, Self>`、`PyRefMut<'_, Self>`、`&PyModule` 和 `&Bound<'_, PyModule>`，但遗漏了三种 PyO3 receiver 模式：`Py<Self>`、`Bound<'_, Self>` 和 `&Bound<'_, Self>`。

现有代码有两个分支：
1. **非引用类型** — 检查路径最后一段标识符是否在 `{"Python", "PyRef", "PyRefMut"}` 中。
2. **引用类型（`&T`）** — 检查内部类型是否为 `Python`、`PyModule` 或 `Bound<'_, PyModule>`/`Borrowed<'_, PyModule>`。

生成器（`src/generator.rs`）会无条件为所有 `MethodKind::Instance` 方法前置 `self`，因此泄漏的 receiver 参数会在 stub 中变成双重 `self`。

## Goals / Non-Goals

**目标：**
- 正确排除 `Py<Self>`、`Bound<'_, Self>` 和 `&Bound<'_, Self>` receiver，使其不出现在 stub 参数中。
- 仅当泛型参数为 `Self` 时排除——保留 `Py<T>` 和 `Bound<'_, T>`（其中 `T != Self`）作为合法参数。
- 保持与所有现有正确 stub 的向后兼容性。

**非目标：**
- 处理类型别名（如 `type Handle = Py<Self>`）。
- 处理 `Py<ConcreteClassName>`（显式写出类名而非使用 `Self` 关键字）。
- collector/parser 层之外的修改。

## Decisions

### 决策 1：检查泛型参数中的 `Self`，而非按类型名全量匹配

**方案：** 检查 `Py<T>` 和 `Bound<'_, T>` 的泛型参数，验证 `T == Self` 后才排除。

**考虑过的替代方案：**
- _直接将 `"Py"` 和 `"Bound"` 加入排除列表_：会错误过滤掉 `Py<SomeOtherClass>` 和 `Bound<'_, SomeOtherClass>`，这些都是合法的 Python 级参数（如传递类实例作为参数）。已否决。
- _按参数名模式匹配（如 `slf`、`slf_handle`）_：脆弱——用户可以选择任何绑定名。已否决。

**理由：** 在 `#[pymethods]` impl 块中，`Self` 始终指代当前实现的类。检查它既精确又足够。

### 决策 2：提取 `has_generic_arg_named` 通用辅助函数

**方案：** 创建通用辅助函数 `has_generic_arg_named(args: &syn::PathArguments, name: &str) -> bool`，检查是否有泛型类型参数的最后一段路径标识符匹配 `name`。在此基础上提供语义化的 `generic_arg_is_self(args) → has_generic_arg_named(args, "Self")` 便捷封装。

**理由：** 非引用分支和引用分支都需要检查泛型参数——非引用分支检查 `Self`，引用分支同时检查 `Self` 和 `PyModule`。将底层逻辑抽象为 `has_generic_arg_named` 避免了两处重复编写 `matches!` 宏，同时保持 `generic_arg_is_self` 作为高可读性的语义入口。

### 决策 3：修改 `&Bound`/`&Borrowed` 检查以同时接受 `Self`

**方案：** 在引用分支中，将现有 `Bound`/`Borrowed` 检查从 `generic_arg == PyModule` 扩展为 `generic_arg == PyModule || generic_arg == Self`。

**理由：** 现有代码已经检查了 `Bound`/`Borrowed` 的泛型参数。添加 `Self` 是最小的改动增量。`Borrowed` 类型也一并包含，虽然很少见。

## Risks / Trade-offs

- **[`#[pymethods]` 外部的 `Py<Self>` 误判]** → `is_pyo3_injected_param` 仅从 `parse_params` 调用，而 `parse_params` 仅在 `#[pymethods]` 和 `#[pyfunction]` 上调用。`#[pyfunction]` 没有 `Self` 上下文，因此 `Py<Self>` 不会出现在那里。低风险。
- **[完全限定路径]** → `pyo3::Py<Self>` 和 `pyo3::Bound<'_, Self>`——检查使用 `tp.path.segments.last()`，无论模块路径前缀如何都能正确匹配最后一段。无风险。
- **[最小范围]** → 变更仅涉及 `is_pyo3_injected_param` 并添加测试。对 generator、config、CLI 或 output layout 无影响。
