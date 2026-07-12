## Context

pyo3 自 0.21 起对 `#[pymodule]` / `#[pyfunction]` / `#[pymethods]` 内的 `async fn` 提供一等支持(见 https://pyo3.rs/main/async-await):被 `#[pyfunction]` 标注的 `async fn` 在 Python 侧表现为返回 coroutine 的可 await 可调用对象。

Rylai 当前的静态分析链对此完全无感知:

- `parse_pyfunction`(`src/collector/parse.rs:901`)只读 `f.sig.ident` / `inputs` / `output`,从不读 `f.sig.asyncness`。
- `parse_pymethod`(`src/collector/parse.rs:1228`)同理;`PyMethod`(`src/collector/model.rs:110`)与 `MethodKind`(`model.rs:124`)无 async 概念。
- `PyFunction`(`model.rs:37`)无 `is_async` 字段。
- `generator.rs` 的 8 处函数/方法渲染(601 / 779 / 782 / 788 / 795 / 799 / 805 / 810 行)全部字面量 `"def "`。
- `type_map.rs` 无 `Coroutine` / `Awaitable` / `Future` 条目。

根因:`async` 关键字从未进入数据模型,故渲染与类型映射均无从区分同步/异步。

## Goals / Non-Goals

**Goals:**

- 检测 `#[pyfunction]` / `#[pymethods]` 中的 `async fn`,生成正确的 `async def` stub。
- 返回类型保持为内层类型(Python typing 语义下 `async def f() -> T` 已隐含协程)。
- 零配置;不回归既有同步输出。

**Non-Goals:**

- `pyo3-asyncio` 外部 runtime 的 `Future` 返回类型映射。
- `#[new]` / `@property` / setter 的 async(无效 Python)。
- 新增 `rylai.toml` 项或 CLI flag。
- `CancelHandle` 类型本身到 Python 类型的映射(它永远被排除,不进入 stub)。

## Decisions

### 决策 1:检测信号源

**方案:** 以 `syn::Signature::asyncness`(即源码 `async` 关键字)为唯一信号。`parse_pyfunction` / `parse_pymethod` 在构造模型时取 `sig.asyncness.is_some()` 写入 `is_async: bool`。

**考虑过的替代方案:**

- _按返回类型嗅探_(`Coroutine` / `Future` / `Pin<...>`):已否决。`async fn` 的 Rust 侧返回类型仍是内层 `T`(desugar 进函数体,不在签名 AST),嗅探不可靠;且 pyo3 自身的 `#[pyfunction]` 宏正是 keyed off `async` 关键字。
- _要求 `rylai.toml` 显式标注_:已否决。违反零配置原则,且源码已有明确信号。

**理由:** `sig.asyncness` 是 AST 上权威且唯一的信号,与 pyo3 自身判定一致。

### 决策 2:stub 渲染形式

**方案:** 输出 `async def name(...) -> T`,其中 `T` 为 PyResult 解包后的内层返回类型。**不**包裹 `Coroutine[...]` / `Awaitable[...]`。

**考虑过的替代方案:**

- _`def name(...) -> Coroutine[T, None, None]: ...`_:已否决。`Coroutine` 有 3 个类型参数(send / throw / yield),Rust 侧无从得知;且非惯用法 —— 人手写 stub 都用 `async def`。type checker(mypy / pyright)对 `async def f() -> T` 已正确推导为"调用返回协程、await 后得到 `T`"。
- _两种都生成_:已否决,易混淆且冗余。

**理由:** 与 Python typing 惯用法一致,语义最准确,生成物即人手写模样。

### 决策 3:async 的适用边界

**方案:** `is_async` 作用于 `#[pyfunction]` 与 `#[pymethods]` 的 Instance / Static / Class 方法。对 `#[new]`(→ `__init__`)、getter、setter,**忽略** async 标志,仍输出普通 `def`(可记录一条 warning)。

**考虑过的替代方案:**

- _无差别输出 `async def __init__`_:已否决。`async def __init__` 与 `async @property` 在 Python 中非法,pyo3 也不支持 async `#[new]`。

**理由:** 保证生成物始终是合法 Python;async `#[new__]` / `@property` 本就是不存在的情况。

### 决策 4:排除 `#[pyo3(cancel_handle)]` 参数

**方案:** 在 `parse_params`(`src/collector/parse.rs`)中,凡 `FnArg::Typed` 携带 `#[pyo3(cancel_handle)]` 属性(meta list `pyo3` 内含裸 ident `cancel_handle`)的参数,MUST 从 stub 参数列表排除 —— 与现有 `is_pure_injected_type`(排除 `Python<'_>` / `&Bound<'_, PyModule>` 等)并列为"位置无关的 pyo3 注入检查",在 `is_pure_injected_type` 之后立即判定。该排除 MUST NOT 依赖参数类型名(`CancelHandle` 在 `experimental-async` feature 下定义,且不应把类型名硬编码进排除逻辑)。

**考虑过的替代方案:**

- _按类型名 `CancelHandle` 排除_:已否决。类型名属于外部 feature(`pyo3::experimental::async_await`),硬编码类型名会与"纯静态分析、不依赖外部 crate 类型"的原则冲突;而 `#[pyo3(cancel_handle)]` 属性是 pyo3 权威的、位置无关的注入信号,与既有 `is_pure_injected_type` 同源。
- _仅当函数 `is_async` 时排除_:已否决。属性本身已表达"pyo3 注入"语义,无需叠加 async 前提;且 `cancel_handle` 的检测与 asyncness 正交(虽然实际只出现在 async fn),保持排除逻辑单一职责。
- _归入新的 `pyo3_attr_injected_params` 通用机制_:已否决(过度设计)。当前仅 `cancel_handle` 一个属性,直接在 `parse_params` 加一条属性检查即可;未来若出现更多 `#[pyo3(...)]` 注入属性再抽象。

**理由:** 与既有注入参数排除同源(基于 pyo3 语义属性而非类型名),覆盖文档 `cancellable` 案例的完整签名,且不引入对外部 feature 类型的依赖。

## Risks / Trade-offs

- **[既有 `#[test]` 构造点回归]** → `PyFunction` / `PyMethod` 加字段后,所有现有测试中手工构造这两个结构的位置会编译失败。逐处补 `is_async: false`;以 `cargo test` 守护。
- **[pyo3 版本差异]** → 早期 pyo3 的 async 支持需借助 pyo3-asyncio;本变更只覆盖原生 `async fn`(pyo3 ≥ 0.21)。Non-goals 已声明 pyo3-asyncio 不在范围。
- **[示例项目维护成本]** → 新增 `async_await_sample` 需同步维护 `examples/async_await_sample/python/async_await_sample/async_await_sample.pyi`(与 `basic_function_sample` 一致的 `python/...` 布局)。若觉过重,可降级为仅内联测试。

## Migration Plan

纯新增能力,无数据 / 配置迁移。回滚 = revert 该 commit;既有同步代码输出不变(新字段默认 `false`)。

## Open Questions

_(无。)_
