## Why

Rylai 完全不感知 `async fn`。`src/collector/parse.rs:901` 的 `parse_pyfunction` 与 `src/collector/parse.rs:1228` 的 `parse_pymethod` 均不读取 `sig.asyncness`;`src/generator.rs`(601/779/782/788/795/799/805/810 行)全部硬编码 `def`;`src/type_map.rs` 亦无协程条目(`grep "async def"` 全仓零命中)。结果:`#[pyfunction] async fn fetch() -> PyResult<String>` 被错误地生成为 `def fetch() -> str` —— 既丢了 `async`,返回类型也谎称直接返回 `str` 而非需要 await 的协程,对调用者产生误导。该特性在 pyo3 官方文档 https://pyo3.rs/main/async-await 中是一等支持。

## What Changes

- 在 `PyFunction`、`PyMethod` 上新增 `is_async: bool`,由 `sig.asyncness.is_some()` 推导。
- `parse_pyfunction`、`parse_pymethod`(Instance/Static/Class 分支)读取并回填该标志。
- `generator.rs` 在函数与这三类方法上条件输出 `async def`;`#[new]`→`__init__`、getter、setter 忽略 async 标志(与 Python 语义一致,这些不可能 async)。
- 返回类型保持"内层类型"(PyResult 解包后),**不**包裹 `Coroutine[...]` —— `async def` 本身即声明协程语义。
- `parse_params` 排除带 `#[pyo3(cancel_handle)]` 属性的参数(pyo3 注入的 `CancelHandle`,Python 侧不可见,见文档 `cancellable` 案例)。
- 新增内联单测与示例项目 `async_await_sample`。

## Capabilities

### New Capabilities

- `async-function-emission`:规格化 `async fn` → Python `async def` 的检测与渲染。

### Modified Capabilities

_(无 —— async 属全新能力,不改动现有三份 spec。)_

## Non-goals

- 不支持 `pyo3-asyncio` 外部 crate 的 `Future`/async runtime 返回类型映射(其返回类型需运行时知识,超出纯静态分析范围)。
- 不处理 `#[new]`/`@property`/setter 的 async(无效 Python,pyo3 亦不支持)。
- 不引入新的 `rylai.toml` 配置项或 CLI flag(零配置原则)。
- 不改变 `#[pyo3(signature)]` override 的解析(asyncness 与之正交)。

## Impact

- **Parser**(`src/collector/parse.rs`):读取 `sig.asyncness` 回填标志(向后兼容,默认 `false`);`parse_params` 新增 `#[pyo3(cancel_handle)]` 参数排除,与既有 `is_pure_injected_type` 并列(位置无关的 pyo3 注入检查)。Style B(函数式模块)无需改动 —— `add_function(wrap_pyfunction!(...))` 经 `find_pyfunction_by_name` → `parse_pyfunction` 解析,自动继承 asyncness。
- **Model**(`src/collector/model.rs`):`PyFunction`/`PyMethod` 新增字段,需同步所有既有构造点 —— 既包括 `#[test]` helper,也包括 `parse_struct_fields_as_methods`(`src/collector/parse.rs`)为字段 getter/setter 合成的 `PyMethod`(这些构造点不经 `parse_pymethod`,不会自动填 `is_async`)。
- **Generator**(`src/generator.rs`):8 处 `def` 中,函数 + Instance/Static/Class 共 4 处条件化;`__new__`/`__init__`/getter/setter 共 4 处保持原样。
- **Tests / Examples**:新增内联测试与 `examples/async_await_sample`。
- **PyO3 兼容性**:对齐 pyo3 main 文档的 async-await 行为;不影响既有同步代码输出。无 CLI / config 变更。
