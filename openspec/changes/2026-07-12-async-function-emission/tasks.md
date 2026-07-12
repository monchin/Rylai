## 1. 数据模型

- [x] 1.1 在 `src/collector/model.rs` 的 `PyFunction` 结构体新增 `pub is_async: bool` 字段。
- [x] 1.2 在 `src/collector/model.rs` 的 `PyMethod` 结构体新增 `pub is_async: bool` 字段。`MethodKind` 枚举不变(边界由生成器按 kind 判定)。
- [x] 1.3 排查并修复所有既有 `PyFunction` / `PyMethod` 构造点,补 `is_async: false`:既包括 `#[test]` 中手工构造,也包括生产代码 `parse_struct_fields_as_methods`(`src/collector/parse.rs`)为字段 getter/setter 合成的 `PyMethod`(不经 `parse_pymethod`,不会自动回填)。验收:`cargo build` 通过。

## 2. Parser

- [x] 2.1 在 `src/collector/parse.rs` 的 `parse_pyfunction`(约 901 行)取 `let is_async = f.sig.asyncness.is_some();`,回填进返回的 `PyFunction`。
- [x] 2.2 在 `parse_pymethod`(约 1228 行)对 `ImplItem::Fn` 分支取 `m.sig.asyncness.is_some()` 回填 `PyMethod`。
- [x] 2.3 确认 `#[pyo3(signature)]` override 路径不受影响(asyncness 与 signature 正交,只需在签名读取处一并取值)。验收:有 / 无 signature 的 async 函数都输出 `async def`。
- [x] 2.4 在 `parse_params`(`src/collector/parse.rs`)中,对 `FnArg::Typed` 分支,在现有 `is_pure_injected_type` 检查之后新增:检查该参数的 `pat.attrs`(或 `pt.attrs`)是否含 `#[pyo3(cancel_handle)]`(meta list `pyo3` 内含裸 ident `cancel_handle`);若命中则 `continue` 排除该参数。判定基于属性,不基于类型名 `CancelHandle`。新增辅助函数 `has_cancel_handle_attr(attrs: &[Attribute]) -> bool`。验收:`CancelHandle` 参数被排除,正常参数保留。
- [x] 2.5 确认 `cancel_handle` 排除与 `is_async` 正交:即便宿主函数非 async(理论),带该属性的参数仍被排除;且排除不抑制 `async def` 渲染。

## 3. Generator

- [x] 3.1 `src/generator.rs` 函数渲染(约 601 行):当 `f.is_async` 为真,前缀改为 `async def`,否则保持 `def`。
- [x] 3.2 `emit_method_signature_lines`(约 753 行起)对 Instance(810 行)/ Static(788 行)/ Class(795 行)三类:当 `is_async` 为真输出 `async def`;`__new__`(779 行)/ `__init__`(782 行)/ getter(799 行)/ setter **忽略** async 标志,保持 `def`。
- [x] 3.3 验收:同步函数 / 方法输出与现状逐字节一致(回归测试通过)。

## 4. 测试

- [x] 4.1 在 `src/collector/parse.rs` 测试模块新增 `pyfunction_asyncness_is_captured`:输入 `#[pyfunction] async fn fetch() -> PyResult<String>`,断言 `PyFunction.is_async == true`。
- [x] 4.2 新增 `pyfunction_sync_is_not_async`:同步版本断言 `false`。
- [x] 4.3 新增 `pymethod_asyncness_is_captured`:Instance / Static / Class 各一例。
- [x] 4.4 在 `src/generator.rs` 测试模块新增 `async_pyfunction_emits_async_def`:断言输出含 `async def fetch() -> str:`。
- [x] 4.5 新增 `async_pymethod_emits_async_def`:Instance async 方法输出 `async def`。
- [x] 4.6 新增 `async_new_still_emits_sync_def`(若可构造):async `#[new]` 输出仍是 `def __init__`(防回归)。
- [x] 4.7 内联集成测试(parse → generate):输入 `#[pyfunction] async fn fetch() -> PyResult<String>`,经 `parse_pyfunction` 构造 `PyFunction` 后交生成器产出 stub,断言输出含 `async def fetch() -> str:`。覆盖 asyncness 穿越 parse/generate 边界的回归(§5 示例项目为可选时的最低集成保障)。
- [x] 4.8 新增 `cancel_handle_param_is_excluded`:输入 `#[pyfunction] async fn cancellable(#[pyo3(cancel_handle)] mut cancel: CancelHandle)`,断言生成的 stub 为 `async def cancellable() -> None:`(无 `cancel` 参数)。
- [x] 4.9 新增 `cancel_handle_keeps_sibling_params`:输入 `async fn f(#[pyo3(cancel_handle)] mut h: CancelHandle, x: usize) -> PyResult<String>`,断言 stub 为 `async def f(x: int) -> str:`(仅排除 `h`)。
- [x] 4.10 新增 `cancel_handle_excluded_by_attr_not_type_name`:构造一个参数带 `#[pyo3(cancel_handle)]` 但类型名非 `CancelHandle`(如 `MyHandle`)的用例,断言仍被排除(判定基于属性)。
- [x] 4.11 新增 `cancel_handle_excluded_under_signature_override`:输入 `#[pyo3(signature = (x))] #[pyfunction] async fn f(x: usize, #[pyo3(cancel_handle)] mut h: CancelHandle) -> PyResult<String>`(signature 不列 `h`),断言生成的 stub 为 `async def f(x: int) -> str:`。覆盖 parse 阶段排除(cancel 不进 `f.params`)与 generate 阶段 signature 合并(`merge_sig_with_types` 基于已排除的 `f.params`)的跨阶段交互,确保两阶段对 cancel 参数的剔除不冲突。

## 5. 示例项目

- [x] 5.1 新建 `examples/async_await_sample/`(参照 `basic_function_sample` 结构):含 `src/lib.rs`(一个 `#[pyfunction] async fn`(如 `sleep`)+ 一个含 async 方法的 `#[pyclass]` + 一个 `#[pyo3(cancel_handle)]` 案例(如 `cancellable`))、`Cargo.toml`、`pyproject.toml`、`rylai.toml`,生成输出路径 `python/async_await_sample/async_await_sample.pyi`(与 `justfile` 的 `gen-pyi-examples` 约定一致)。
- [x] 5.2 生成并在仓库中提交 stub 至 `examples/async_await_sample/python/async_await_sample/async_await_sample.pyi`,内容含 `async def`(无 `expected/` 目录,遵循 `basic_function_sample` 的 `python/...` 布局)。
- [x] 5.3 若 `justfile` 的 `gen-pyi-examples` 需列入新示例,同步更新。

## 6. 文档

- [x] 6.1 在根 `README.md` 的 Features 小节补一行 "Async functions (`async fn` → `async def`)"。
- [x] 6.2 在 `AGENTS.md` 的 Type Mapping 表后补一条 async 说明(可选)。

## 7. 验证

- [x] 7.1 运行 `cargo test`,全绿。(359 passed;0 failed)
- [x] 7.2 运行 `cargo clippy -- -D warnings`,无告警。
- [x] 7.3 手跑 `cargo run -- <tmp async fn 源码> -o <tmp>`,人工核对 `async def` 与返回类型正确。(临时源码手动核对:`async def fetch(url: str) -> str:`、`async def cancellable() -> None:`(cancel 参数被排除)、async Instance/Static 方法、`#[new]`/getter 保持 `def` —— 全部符合预期。另由 §5 示例 `async_await_sample` 的 `just gen-pyi async_await_sample` 复核。)
