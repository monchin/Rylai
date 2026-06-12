## 1. 辅助函数

- [x] 1.1 在 `src/collector/parse.rs` 中（`is_pyo3_injected_param` 附近）添加通用辅助函数 `has_generic_arg_named(args: &syn::PathArguments, name: &str) -> bool`，检查 `AngleBracketed` 参数中是否有泛型类型参数为 `Type::Path` 且最后一段标识符匹配 `name`。
- [x] 1.2 在 `has_generic_arg_named` 基础上提供语义化封装 `generic_arg_is_self(args) → has_generic_arg_named(args, "Self")`。

## 2. 扩展 is_pyo3_injected_param

- [x] 2.1 在**非引用分支**，现有 `matches!` 检查 `"Python" | "PyRef" | "PyRefMut"` 之后，添加检查：若 `seg.ident == "Py"` 且 `generic_arg_is_self(&seg.arguments)` 返回 true，则返回 `true`。
- [x] 2.2 在**非引用分支**，同样添加检查：若 `seg.ident == "Bound"` 且 `generic_arg_is_self(&seg.arguments)` 返回 true，则返回 `true`。
- [x] 2.3 在**引用分支**，修改 `Bound`/`Borrowed` 检查以同时接受 `Self`：将内联闭包替换为调用 `generic_arg_is_self(&seg.arguments) || has_generic_arg_named(&seg.arguments, "PyModule")`，复用辅助函数避免重复逻辑。

## 3. 测试

- [x] 3.0 抽取测试辅助函数 `class_method_param_names(source, method_name)` 和 `function_param_names(source)`，将 6 个测试共用的 setup 样板（config、impl_map、cx 构建等）集中管理。
- [x] 3.1 添加测试 `py_self_param_is_excluded` — 解析 `fn method(slf_handle: Py<Self>, x: i32)` 的 `#[pymethods]` 方法，断言参数中不含 `slf_handle`。
- [x] 3.2 添加测试 `py_self_magic_method_excluded` — 解析 `fn __iter__(slf: Py<Self>) -> Py<Self>`，断言参数为空。
- [x] 3.3 添加测试 `bound_self_param_is_excluded` — 解析 `fn method(slf: Bound<'_, Self>, x: i32)`，断言参数中不含 `slf`。
- [x] 3.4 添加测试 `ref_bound_self_param_is_excluded` — 解析 `fn method(slf: &Bound<'_, Self>, x: i32)`，断言参数中不含 `slf`。
- [x] 3.5 添加测试 `borrowed_self_param_is_excluded` — 解析 `fn method(slf: &Borrowed<'_, Self>, x: i32)`，断言参数中不含 `slf`。
- [x] 3.6 添加测试 `py_non_self_is_preserved` — 解析 `fn method(obj: Py<OtherClass>)`，断言参数中包含 `obj`（确保不会过度排除）。
- [x] 3.7 添加测试 `bound_non_self_is_preserved` — 解析 `fn process(obj: Bound<'_, OtherClass>)` 的 `#[pyfunction]`，断言参数中包含 `obj`（覆盖非引用分支 `Bound` 的负面情况）。
- [x] 3.8 运行 `cargo test` 和 `cargo clippy` 验证无回归。

## 4. 验证

- [x] 4.1 使用修复后的 rylai 重新运行 `/tmp/rylai-repro/` 最小复现用例，确认之前泄漏的三个方法现在均生成正确的 stub（输出中不含 `slf`/`slf_handle`）。
