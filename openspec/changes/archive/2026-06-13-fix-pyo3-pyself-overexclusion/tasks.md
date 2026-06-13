## 1. 谓词重构（拆分 `is_pyo3_injected_param`）

- [x] 1.1 在 `src/collector/parse.rs` 新增 `is_self_receiver_type(ty: &Type) -> bool`，识别依赖 `Self` 的 receiver 类型：`Py<Self>`、`Bound<'_, Self>`、`&Bound<'_, Self>`、`&Borrowed<'_, Self>`（复用现有 `has_generic_arg_named(args, "Self")` / `generic_arg_is_self`），以及从旧逻辑迁移的 `PyRef<'_, Self>`、`PyRefMut<'_, Self>`（沿用最后一段路径名匹配）。
- [x] 1.2 新增 `is_pure_injected_type(ty: &Type) -> bool`，识别位置无关的注入类型：`Python<'_>`、`&PyModule`、`&Bound<'_, PyModule>`、`&Borrowed<'_, PyModule>`（从旧 `is_pyo3_injected_param` 的对应分支迁移）。
- [x] 1.3 移除旧的 `is_pyo3_injected_param`（逻辑已被 1.1/1.2 取代），或仅在过渡期保留为 `is_pure_injected_type(ty) || is_self_receiver_type(ty)` 的内部封装。

## 2. `parse_params` 状态机 + method kind 传入

- [x] 2.1 给 `parse_params` 增加 `is_instance_method: bool` 参数（见 design 决策 2）。
- [x] 2.2 将 `parse_params` 改为状态机：维护 `receiver_taken: bool`，遍历 `sig.inputs`。`FnArg::Receiver` → 置 `receiver_taken = true` 并跳过；`FnArg::Typed(pt)` → 若 `!receiver_taken && is_instance_method && is_self_receiver_type(&pt.ty)` 则置 `receiver_taken = true` 并跳过，否则若 `is_pure_injected_type(&pt.ty)` 则跳过，否则收集为普通参数。
- [x] 2.3 更新 `parse_params` 的 pyfunction 调用点：传入 `is_instance_method = false`。
- [x] 2.4 更新 `parse_params` 的 pymethods 调用点：依据 `detect_method_kind(attrs, name)`，`MethodKind::Instance` / `Getter` / `Setter` 传 `true`（三者均带实例 receiver），`Static` / `Class` / `New` 传 `false`。**注意调用顺序**：当前 `parse_pymethod` 中 `parse_params`（`parse.rs:1215`）在 `detect_method_kind`（`parse.rs:1222`）之前调用，需先把 `detect_method_kind` 的结果算出，再传给 `parse_params`。

## 3. 测试

- [x] 3.1 新增测试：`fn by_normal(&self, other: Py<Self>)` 断言 `other` 保留在参数中（对应 spec scenario「普通参数位置」）。
- [x] 3.2 新增测试：`fn m(slf: Py<Self>, other: Py<Self>)` 断言仅 `slf` 被排除、`other` 保留（scenario「receiver 之后再次出现」）。
- [x] 3.3 新增测试：`#[staticmethod] fn make(other: Py<Self>)` 断言 `other` 保留且 stub 无 `self`（scenario「staticmethod」）。
- [x] 3.4 确认上一 change 的 `py_self_param_is_excluded`、`bound_self_param_is_excluded` 等测试仍通过（scenario「receiver 位置不回归」）。
- [x] 3.5 新增测试：`fn m(&self, other: Bound<'_, Self>)` / `&Bound<'_, Self>` / `&Borrowed<'_, Self>` 在非 receiver 位置保留。
- [x] 3.6 新增测试：`fn m(&self, other: PyRef<'_, Self>)` / `PyRefMut<'_, Self>` 在非 receiver 位置保留（覆盖决策 3 的行为变化）。
- [x] 3.7 确认测试：`Python<'_>`、`&Bound<'_, PyModule>` 在任意位置仍被排除（纯注入位置无关，不回归）。
- [x] 3.8 确认测试：`Py<OtherClass>` / `Bound<'_, OtherClass>` 仍作为普通参数保留（上一 change 已有，不回归）。
- [x] 3.9 新增测试：`#[setter] fn set_x(slf: PyRefMut<'_, Self>, v: i32)` 与 `#[getter] fn get_x(slf: PyRef<'_, Self>) -> i32`，断言 typed receiver 被排除、setter 的 `value` 仅剩 `v`（覆盖决策 2 的 Getter/Setter 分类，避免 `setter_value_param_type` 取错 value 的静默回归）。

## 4. 验证

- [x] 4.1 运行 `cargo test` 与 `cargo clippy`，确认全部通过、无回归。
- [x] 4.2 端到端验证（已完成）：用临时最小 PyO3 crate（maturin 1.13 + CPython 3.9）编译并 `inspect.signature`，确认运行时签名为 `by_normal (self, /, other)`、`two_self (self, /, other)`、`static_make (other)`，与 rylai 生成的 stub（三个 `other: Foo` 均保留）一致——修复生效，无回归。

## 5. 重构（method-kind 判定收敛 / 谓词简化）

- [x] 5.1 将 `is_self_receiver_type` 的三个重复分支（`Py`/`Bound`、`PyRef`/`PyRefMut`、`Borrowed`）合并为单个 `matches!(ident.as_str(), "Py" | "Bound" | "PyRef" | "PyRefMut" | "Borrowed") && generic_arg_is_self(...)`，`generic_arg_is_self` 调用从 3 次降至 1 次。纯重构，行为不变。
- [x] 5.2 新增 `has_instance_receiver(kind: &MethodKind) -> bool`（`Instance | Getter | Setter` → `true`，`Static | Class | New` → `false`），把"哪些 kind 带实例 receiver"的判定从 `parse_pymethod` 调用点的内联 `matches!` 收敛进 parser 内；`parse_pymethod` 改为 `parse_params(.., has_instance_receiver(&kind))`。`parse_params` 签名保持 `is_instance_method: bool` 不变（见 design 决策 4）。
- [x] 5.3 新增 `has_instance_receiver_classifies_method_kinds` 单测；`cargo test`（110 passed）与 `cargo clippy --all-targets` 全绿。
