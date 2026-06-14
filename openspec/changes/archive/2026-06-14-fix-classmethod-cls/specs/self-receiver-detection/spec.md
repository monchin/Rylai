## ADDED Requirements

### Requirement: classmethod 的 cls receiver 必须从 stub 参数中排除并注入 cls

当 `#[pymethods]` 中带 `#[classmethod]` 的方法首参类型为 PyO3 注入的 cls receiver(`&Bound<'_, PyType>`)时,该参数 MUST 被视为隐式 receiver 并从生成的 stub 参数列表中排除。生成的 classmethod stub MUST 以 bare `cls`(无类型注解)作为首个参数,MUST NOT 以 `self` 作为首个参数。

#### Scenario: 无参 classmethod 注入 cls

- **WHEN** `#[pymethods]` 中带 `#[classmethod]` 的方法声明为 `fn create() -> Self`
- **THEN** 生成的 stub MUST 为 `def create(cls) -> ...`,以 bare `cls` 为首参,MUST NOT 出现 `self`

#### Scenario: 带 `&Bound<'_, PyType>` cls 参数的 classmethod

- **WHEN** `#[pymethods]` 中带 `#[classmethod]` 的方法声明为 `fn create(cls: &Bound<'_, PyType>, x: i32)`
- **THEN** 生成的 stub MUST 为 `def create(cls, x: int) -> ...`,首参为注入的 bare `cls`,且 Rust 侧的 `cls` 参数 MUST NOT 出现在参数列表中

#### Scenario: instance method 的 self 注入不受影响(回归)

- **WHEN** `#[pymethods]` 普通实例方法声明为 `fn f(&self, x: i32)`
- **THEN** 生成的 stub MUST 仍为 `def f(self, x: int) -> ...`,以 `self` 为首参(classmethod 的 cls 改动 MUST NOT 影响 instance 方法)

#### Scenario: staticmethod 的无 receiver 行为不受影响(回归)

- **WHEN** `#[pymethods]` 中带 `#[staticmethod]` 的方法声明为 `fn g(x: i32)`
- **THEN** 生成的 stub MUST 仍为 `def g(x: int) -> ...`,MUST NOT 注入 `cls` 或 `self`

#### Scenario: classmethod 带 pyo3 signature 覆盖时仍正确注入 cls

- **WHEN** `#[pymethods]` 中带 `#[classmethod]` 与 `#[pyo3(signature = (...))]` 的方法声明为 `fn create(cls: &Bound<'_, PyType>, x: i32)`,且 signature 覆盖不含 cls
- **THEN** 生成的 stub MUST 为 `def create(cls, x: int) -> ...`,首参为注入的 bare `cls`,Rust 侧的 cls 参数 MUST NOT 出现,且 cls MUST NOT 因 signature 覆盖而重复
