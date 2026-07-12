# Async Function Emission

## Purpose

规格化 pyo3 `async fn` 在生成的 Python stub（`.pyi`）中的渲染策略：检测被 `#[pyfunction]` / `#[pymethods]` 标注、且 Rust 签名含 `async` 关键字的函数 / 方法，将其渲染为 `async def` 而非 `def`；保持内层返回类型（不包裹 `Coroutine[...]`）；并排除 pyo3 运行时注入的 `#[pyo3(cancel_handle)]` 参数。对齐 [pyo3 async-await 指南](https://pyo3.rs/main/async-await)。

## Requirements

### Requirement: `async fn` 必须渲染为 `async def`

被 `#[pyfunction]` 或 `#[pymethods]` 标注、且 Rust 签名含 `async` 关键字(`sig.asyncness.is_some()`)的函数 / 方法,在生成的 Python stub 中 MUST 渲染为 `async def`,而非 `def`。该判定 MUST 基于 `syn::Signature::asyncness`,MUST NOT 依赖返回类型嗅探或任何 `rylai.toml` 配置。

#### Scenario: `#[pyfunction]` async 函数

- **WHEN** 源码为 `#[pyfunction] async fn fetch() -> PyResult<String>`
- **THEN** 生成的 stub 必须为 `async def fetch() -> str:`,而非 `def fetch() -> str:`

#### Scenario: 同步函数不受影响

- **WHEN** 源码为 `#[pyfunction] fn fetch() -> PyResult<String>`(无 `async`)
- **THEN** 生成的 stub 仍为 `def fetch() -> str:`(无 `async`)

#### Scenario: `#[pymethods]` async 实例方法

- **WHEN** 某类 `#[pymethods]` impl 中声明 `async fn compute(&self, n: usize) -> PyResult<f64>`
- **THEN** 生成的 stub 必须为 `async def compute(self, n: int) -> float:`

#### Scenario: `#[staticmethod]` async 方法

- **WHEN** `#[pymethods]` 中 `#[staticmethod] async fn build() -> PyResult<Point>`
- **THEN** 生成的 stub 必须为 `@staticmethod` 装饰下的 `async def build() -> Point:`

#### Scenario: `#[classmethod]` async 方法

- **WHEN** `#[pymethods]` 中 `#[classmethod] async fn create(cls: &Bound<'_, PyType>) -> PyResult<Self>`
- **THEN** 生成的 stub 必须为 `@classmethod` 装饰下的 `async def create(cls) -> ...`,首参仍为注入的 bare `cls`(无类型注解),`async` 关键字 MUST 出现在 `def` 前,MUST NOT 因 async 分支改变 `cls` 注入

#### Scenario: `#[pyo3(signature)]` 覆盖不抑制 async 渲染

- **WHEN** `#[pyfunction]` 同时带 `async` 与 `#[pyo3(signature = (...))]`,如 `#[pyo3(signature = (a, b=1))] async fn f(a: usize, b: usize) -> PyResult<String>`
- **THEN** 生成的 stub 形状按 signature 覆盖,但仍 MUST 渲染为 `async def f(a, b=1) -> str:`;asyncness 与 signature 覆盖 MUST 正交,signature 覆盖 MUST NOT 抑制 `async def`

### Requirement: async 函数的返回类型为内层类型

`async fn` 的 stub 返回类型 MUST 为 PyResult / Result 解包后的内层类型(如 `String` → `str`),MUST NOT 额外包裹 `Coroutine[...]` / `Awaitable[...]` —— `async def` 在 Python typing 语义下已隐含"调用返回协程、await 后得到该类型"。

#### Scenario: 返回 `PyResult<String>`

- **WHEN** `async fn fetch() -> PyResult<String>`
- **THEN** stub 返回类型为 `str`,即 `async def fetch() -> str:`(无 `Coroutine` 包裹)

#### Scenario: 返回 `Option<i64>`

- **WHEN** `async fn maybe() -> Option<i64>`
- **THEN** stub 为 `async def maybe() -> int | None:`(沿用既有 Option 规则,无额外协程包裹)

### Requirement: `#[new]`、getter、setter 不得渲染为 `async def`

`__new__` / `__init__`(由 `#[new]` 映射)、`@property`(getter)、setter MUST NOT 渲染为 `async def`,即使对应 Rust 项声明为 `async`(这些在 Python 中非法,pyo3 亦不支持)。此时该等成员 MUST 按同步规则渲染普通 `def` / `property`。

#### Scenario: async `#[new]`(理论边界)

- **WHEN** `#[pymethods]` 中 `#[new] async fn new(...) -> PyResult<Self>`(pyo3 实际不支持)
- **THEN** 生成的 `__init__` 仍为普通 `def __init__(...) -> None:`,不含 `async`

### Requirement: `#[pyo3(cancel_handle)]` 参数必须从 stub 中排除

pyo3 在 `async fn` 签名中以 `#[pyo3(cancel_handle)]` 标注的参数(如 `#[pyo3(cancel_handle)] mut cancel: CancelHandle`)是 pyo3 运行时注入的、Python 侧不可见的参数。此类参数 MUST 从生成的 Python stub 参数列表中排除,判定 MUST 基于该属性(meta list `pyo3` 内含裸 ident `cancel_handle`),MUST NOT 依赖参数类型名(如 `CancelHandle`,其属于 pyo3 `experimental-async` 外部 feature)。该排除 MUST 与既有位置无关的 pyo3 注入检查(`Python<'_>` / `&Bound<'_, PyModule>` 等)并列生效,且 MUST NOT 抑制 `async def` 渲染。

#### Scenario: `cancellable` 案例完整签名

- **WHEN** `#[pyfunction] async fn cancellable(#[pyo3(cancel_handle)] mut cancel: CancelHandle)`(返回类型缺省,→ `None`)
- **THEN** 生成的 stub 必须为 `async def cancellable() -> None:`,不含 `cancel` 参数

#### Scenario: cancel_handle 与正常参数并存

- **WHEN** `#[pyfunction] async fn f(#[pyo3(cancel_handle)] mut handle: CancelHandle, x: usize) -> PyResult<String>`
- **THEN** 生成的 stub 必须为 `async def f(x: int) -> str:`,仅排除带属性的 `handle`,`x` 保留

#### Scenario: 类型名不参与排除判定

- **WHEN** 某 async fn 的参数被 `#[pyo3(cancel_handle)]` 标注,但其类型名**不是** `CancelHandle`(如用户自定义包装或重命名类型)
- **THEN** 该参数仍 MUST 被排除(判定基于属性而非类型名)

#### Scenario: cancel_handle 排除不抑制 async def

- **WHEN** `#[pyfunction] async fn cancellable(#[pyo3(cancel_handle)] mut cancel: CancelHandle)`
- **THEN** stub 渲染为 `async def`(asyncness 与参数排除正交),即 `async def cancellable() -> None:`
