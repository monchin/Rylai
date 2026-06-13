## Why

上一个 change（`fix-pyo3-self-receiver-leak`）让 `is_pyo3_injected_param` 识别 `Py<Self>`、`Bound<'_, Self>` 等 receiver 类型并从生成的 stub 中排除。但该函数在 `parse_params` 中对**每一个** `FnArg::Typed` 参数无差别调用，不看位置。后果：当 `Py<Self>` 出现在**非 receiver 位置**时——例如 `fn by_normal(&self, other: Py<Self>)`——`other` 会被错误删除。已用编译产物验证：PyO3 运行时的真实签名是 `(self, /, other)`，rylai 却生成 `by_normal(self)`，在正确调用点引发 mypy/pyright 误报（"参数过多"）。

## What Changes

- 让 receiver 排除逻辑**位置敏感**：对依赖 `Self` 的 receiver 类型（`Py<Self>`、`Bound<'_, Self>`、`&Bound<'_, Self>`、`&Borrowed<'_, Self>`），仅当目标参数处于 **receiver 位置**（方法尚无 `FnArg::Receiver`，且其为首个 typed 参数）时才排除。
- 方法一旦确认了 receiver（`&self` / `&mut self` / `self`，或首个 typed receiver），后续出现的 `Py<Self>` 等一律作为**普通参数保留**。
- 与位置无关的纯注入类型（`Python<'_>`、`&Bound<'_, PyModule>` 等）继续**无条件排除**，行为不变——它们在任何位置都是 PyO3 注入的，与本次问题无关。

## Capabilities

### New Capabilities
_(无)_

### Modified Capabilities
- `self-receiver-detection`：补充"位置敏感性"要求。现有 spec 已覆盖**类型维度**（`T != Self` 时保留），却遗漏了**位置维度**（`T == Self` 但不在 receiver 位置）。新增要求：receiver 排除必须只在 receiver 位置生效，非 receiver 位置的 `Py<Self>` / `Bound<'_, Self>` 等必须作为普通参数保留。

## Impact

- **Parser**（`src/collector/parse.rs`）：`parse_params` 遍历 `sig.inputs` 时跟踪 `receiver_taken`，据此区分"receiver 位置的 Self 类型"（排除）与"普通位置的 Self 类型"（保留）；原 `is_pyo3_injected_param` 拆为 `is_self_receiver_type`（位置敏感，仅 receiver 位置排除）与 `is_pure_injected_type`（位置无关，无条件排除），"哪些 method kind 带实例 receiver" 由 parser 内 `has_instance_receiver(&MethodKind)` 集中判定后以 `bool` 传入。
- 仅 collector/parser 层。**不影响** CLI、generator、config、output layout。
- 向后兼容：此前被误吞的参数现在会正确出现——stub 更贴近运行时签名，无破坏性变更。
- PyO3 兼容：不改变对 PyO3 的解析假设，只收窄排除范围。

## Non-goals

- 类型别名（如 `type Handle = Py<Self>`）解析为 receiver 的识别。
- 返回值 `-> Py<Self>` 的 Python 类型映射（走 `parse_return_type`，不经排除逻辑）。
- 修改 generator 的 `self` 前置逻辑。
