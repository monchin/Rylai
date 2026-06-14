## 1. generator:引入 staticmethod 上下文标志

- [x] 1.1 在 `src/generator.rs` 的 `GenCtx` 结构(`current_self_type` 字段后,约 211 行)新增 `in_static_method: bool` 字段,注释说明:PEP 673 拒绝 staticmethod 中的 `Self`,该上下文下 `Self` 解析为类名;在 `generate_with_known_classes` 的 ctx 构造(约 49 行 `current_self_type: None,` 后)初始化 `in_static_method: false,`。
- [x] 1.2 扩展 RAII guard:把 `RestoreCurrentSelfTypeGuard`(约 164-174 行)重命名为 `RestoreMethodContextGuard`,持有 `self_type: *mut Option<String>` 与 `in_static: *mut bool` 两个字段,`Drop` 时 `*self_type = None; *in_static = false;`(更新 SAFETY 注释)。
- [x] 1.3 在 `gen_method`(约 819-821 行)设置 `current_self_type` 后,加 `self.in_static_method = matches!(m.kind, MethodKind::Static);`,并把 guard 构造改为同时传两个字段指针。
- [x] 1.4 在 `resolve_type`(约 242-247 行)调 `map_type` 前,计算 `effective_policy`:`if self.in_static_method && self.policy.native_self` 则 clone 并置 `native_self = false`,否则 clone 原 policy;传 `&effective_policy` 给 `map_type`(注释说明 PEP 673 + 嵌套递归自动走类名)。

## 2. 修正固化 bug 的现有测试

- [x] 2.1 修改 `src/generator.rs:1933` 的 `render_policy_py312_emits_native_self_and_no_future_annotations`:把 `stub.contains("-> t.Self:")` 断言改为 `stub.contains("-> PdfDocument:")`;测试名改为 `render_policy_py312_staticmethod_self_uses_class_name`,注释说明 PEP 673 拒绝 staticmethod 中的 Self。保留 `!stub.contains("from __future__ import annotations")` 与 `stub.contains(TYPING_IMPORT_LINE)` 断言。

## 3. 新增 `self-type-rendering` 的 generator 测试

置于 `src/generator.rs` 现有 RenderPolicy 测试组之后(约 1950 行);复用 helper `make_method` / `make_class_with_methods` / `make_param` / `config_with_python_version` / `stub_for_config`,断言失败消息带 `{stub}`。这些测试走完整 `generate` 管线(stub 文本断言),对应 `specs/self-type-rendering/spec.md` 各 scenario。

- [x] 3.1 `static_method_bare_self_return_py311_uses_class_name`:`#[staticmethod] fn make_direct() -> Self` + py3.11,断言 `def make_direct() -> Widget:` 且不含 `t.Self`。
- [x] 3.2 `static_method_pyresult_self_return_py311_uses_class_name`:`#[staticmethod] fn from_int(x: i64) -> PyResult<Self>` + py3.11,断言 `def from_int(x: int) -> Widget:`。
- [x] 3.3 `static_method_bare_self_param_py311_uses_class_name`:参数 `Self` + py3.11,断言 `def take_direct(other: Widget) -> int:`。
- [x] 3.4 `static_method_py_self_param_py311_uses_class_name`:参数 `Py<Self>` + py3.11,断言 `def take_py(other: Widget) -> int:`(验证 `map_type` 嵌套递归)。
- [x] 3.5 `instance_method_self_return_py311_keeps_t_self`(对照):instance `fn echo(&self) -> Self` + py3.11,断言 `def echo(self) -> t.Self:`。
- [x] 3.6 `classmethod_self_return_py311_keeps_t_self`(对照):classmethod `#[classmethod] fn factory(cls) -> Self` + py3.11,断言 `def factory(cls) -> t.Self:`。
- [x] 3.7 `mixed_methods_in_one_class_py311_static_uses_class_name_others_t_self`(防 guard 泄漏):同类内 instance/classmethod/static 各一,断言三者分别为 `t.Self` / `t.Self` / `Widget`。
- [x] 3.8 `static_method_self_return_py39_unchanged_class_name`(回归):staticmethod `-> Self` + py3.9,断言 `def make_direct() -> Widget:` 且含 `from __future__ import annotations`。

## 4. 验证

- [x] 4.1 `cargo test` 与 `cargo clippy --all-targets` 全绿、无回归。
- [x] 4.2 端到端:在临时目录建最小 PyO3 crate(`#[pymodule] mod` 内一个 `#[pyclass]`,含 `#[staticmethod]` 返回/参数 `Self` 各形态 + 一个 instance method `-> Self` 对照),分别用 `python_version = "3.11"` 与 `"3.9"` 跑 `rylai` 生成 stub;确认 py3.11 stub 中 staticmethod → 类名、instance → `t.Self`,py3.9 全部类名。
- [x] 4.3 类型检查器:对 py3.11 stub 跑 `ty check --python-version 3.11`,确认 `All checks passed!`(对照修复前报 `Self cannot be used in a static method`);py3.9 stub 同样 pass。
