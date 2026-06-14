# Constructor Emission

## Purpose

定义 Rylai 生成器如何把 PyO3 构造器（`#[new]` 与字面量 `__init__`）映射为 Python stub 的 `__new__` / `__init__`，遵循 [PyO3 Initializer 文档](https://pyo3.rs/main/class.html#initializer) 语义。

## Requirements

### Requirement: 无显式 `__init__` 时 `#[new]` 生成为 Python `__init__`

当 `#[pyclass]` 的全部 `#[pymethods]` 块中存在带 `#[new]` 属性的方法、但**不存在**字面量名为 `__init__` 的方法时，该 `#[new]` 方法 MUST 生成为 `def __init__(self, ...) -> None`。分叉依据是"是否存在显式 `__init__`"，而非 `#[new]` 的 Rust 方法名——即便 `#[new]` 方法名就叫 `__new__`，只要没有显式 `__init__`，仍 MUST 输出 `__init__`。

#### Scenario: 仅 `#[new]`，方法名 `new`
- **WHEN** `#[pymethods]` 声明 `#[new] fn new(value: i32) -> Self`，类内无字面量名为 `__init__` 的方法
- **THEN** 生成的 stub MUST 为 `def __init__(self, value: int) -> None`

#### Scenario: `#[new]` 方法名为 `__new__` 但无显式 `__init__`
- **WHEN** `#[pymethods]` 声明 `#[new] fn __new__(value: i32) -> Self`，类内无字面量名为 `__init__` 的方法
- **THEN** 生成的 stub MUST 仍为 `def __init__(self, value: int) -> None`，方法名不构成分叉依据

#### Scenario: `#[new]` 方法名为 `py_new`
- **WHEN** `#[pymethods]` 声明 `#[new] fn py_new(value: i32) -> Self`，类内无字面量名为 `__init__` 的方法
- **THEN** 生成的 stub MUST 为 `def __init__(self, value: int) -> None`

### Requirement: 显式 `__init__` 必须独立于 `#[new]`

判定"是否存在显式 `__init__`"时，一个名为 `__init__` 的方法仅当其**不是** `#[new]` 方法（`kind != MethodKind::New`）时才计入。一个方法不能同时充当 constructor（`#[new]`）与 initializer（显式 `__init__`）——它们是同一类的两个不同槽位。因此 `#[new] fn __init__(...)`（构造器的方法名恰好为 `__init__`，PyO3 合法）MUST NOT 被判定为存在显式 `__init__`，MUST 收敛到单构造器行为（生成 `__init__`）。

#### Scenario: `#[new]` 方法名恰好为 `__init__` 不触发 Initializer 模式
- **WHEN** `#[pymethods]` 声明 `#[new] fn __init__(value: i32) -> Self`，类内无其他独立的 `__init__` 方法
- **THEN** 该类 MUST NOT 进入 Initializer 模式，`#[new]` MUST 生成为 `def __init__(self, value: int) -> None`（与单构造器行为一致）

### Requirement: Initializer 模式下 `#[new]` 生成为 Python `__new__`

当 `#[pyclass]` 的全部 `#[pymethods]` 块中**同时**存在带 `#[new]` 属性的方法与字面量名为 `__init__` 的方法（PyO3 [Initializer 模式](https://pyo3.rs/main/class.html#initializer)）时，`#[new]` 方法 MUST 生成为 `def __new__(cls, ...) -> Self`，显式 `__init__` 方法 MUST 生成为 `def __init__(self, ...) -> None`。两者 MUST NOT 生成重复的 `def __init__`。

#### Scenario: 同时定义 `#[new]` 与显式 `__init__`
- **WHEN** `#[pymethods]` 声明 `#[new] fn new(args: ...) -> Self` 与 `fn __init__(&self, args: ...) -> ()`，且类继承原生类型（如 `#[pyclass(extends = PyDict)]`）
- **THEN** 生成的 stub MUST 同时包含 `def __new__(cls, ...) -> ...` 与 `def __init__(self, ...) -> None`，二者 MUST NOT 重复

### Requirement: `__new__` 返回类型遵循版本感知的 `Self` 渲染

Initializer 模式下 `#[new]` 生成的 `__new__` 返回类型 MUST 经由现有 `Self` 渲染机制产出：当 `python_version` ≥ 3.11（PEP 673 `nativeSelf`）时 MUST 为 `t.Self`；当 `python_version` < 3.11 时 MUST 为当前类的类名。该渲染 MUST 与"返回 `Self` 的 staticmethod"行为完全一致。

#### Scenario: `python_version` 3.12 下 `__new__` 返回 `t.Self`
- **WHEN** Initializer 模式下 `python_version` 配置为 `3.12`
- **THEN** 生成的 `__new__` 签名 MUST 形如 `def __new__(cls, ...) -> t.Self:`，且 stub MUST 包含 `import typing as t`、MUST NOT 包含 `from __future__ import annotations`

#### Scenario: `python_version` 3.9 下 `__new__` 返回类名
- **WHEN** Initializer 模式下 `python_version` 配置为 `3.9`，类名为 `MyDict`
- **THEN** 生成的 `__new__` 签名 MUST 形如 `def __new__(cls, ...) -> MyDict:`，且 stub MUST 包含 `from __future__ import annotations`、MUST NOT 出现 `t.Self`

### Requirement: `__new__` 首参为 `cls`

Initializer 模式下生成的 `__new__` 其首参 MUST 为 `cls`（Python 构造器约定），而非 `self`。Rust `#[new]` 方法的 receiver（若以 typed receiver 形式声明）MUST NOT 出现在 stub 参数中，且 MUST NOT 覆盖 `cls` 首参位置。

#### Scenario: `__new__` 首参注入 `cls`
- **WHEN** Initializer 模式下 `#[new]` 方法签名为 `fn new(args: &Bound<'_, PyTuple>) -> PyResult<Self>`
- **THEN** 生成的 `__new__` 首参 MUST 为 `cls`，`args` 之外的注入型参数按既有 receiver/注入规则处理

### Requirement: 仅识别字面量方法名 `__init__`

Initializer 模式的触发检测 MUST 仅匹配 Rust 方法名字面量为 `__init__`（`rust_ident == "__init__"`）。`#[pyo3(name = "__init__")]` 这类 rename MUST NOT 触发 Initializer 模式分叉——与 PyO3 自身按字面量识别魔法方法的行为一致。

#### Scenario: `#[pyo3(name = "__init__")]` 不触发 Initializer 模式
- **WHEN** `#[pymethods]` 声明带 `#[new]` 的方法，且存在 `#[pyo3(name = "__init__")] fn my_init(&self, ...)`（Rust 名非 `__init__`）
- **THEN** 系统 MUST NOT 进入 Initializer 模式，`#[new]` MUST 仍生成为 `def __init__`

### Requirement: Initializer 模式检测跨全部 `#[pymethods]` 块

当启用 `multiple-pymethods` feature 时，字面量 `__init__` 的存在检测 MUST 跨越该类的全部 `#[pymethods]` impl 块。`#[new]` 与显式 `__init__` 出现在不同 impl 块时，分叉判定 MUST 与二者同块时一致。

#### Scenario: `#[new]` 与 `__init__` 分处不同 impl 块
- **WHEN** 同一 `#[pyclass]` 在第一个 `#[pymethods]` 块声明 `#[new] fn new(...)`，在第二个 `#[pymethods]` 块声明 `fn __init__(&self, ...)`
- **THEN** 系统 MUST 进入 Initializer 模式，`#[new]` 生成为 `__new__`，第二个块的 `__init__` 生成为 `__init__`

### Requirement: Initializer 模式下 override 精确命中、无重复

当用户通过 `[[override]]` 覆盖构造器时，override 项 MUST 按目标方法的**生成 stub 名**精确命中。Initializer 模式下 `::Class::__new__`（或 `#[new]` 的 rust_ident）MUST 只命中 `#[new]`，`::Class::__init__` MUST 只命中显式 `__init__`。单个 override 项 MUST NOT 同时替换 `#[new]` 与显式 `__init__` 两个方法的 stub。`#[new]` 的 dunder 别名 MUST 等于其当前生成名（非 Initializer 模式为 `__init__`，Initializer 模式为 `__new__`），MUST NOT 同时持有 `__init__` 与 `__new__` 两个别名。

#### Scenario: Initializer 模式下 override `__new__` 只命中 `#[new]`
- **WHEN** Initializer 模式下 override 项为 `m::MyClass::__new__`
- **THEN** 该 override MUST 只替换 `#[new]` 生成的 `__new__` stub，MUST NOT 影响显式 `__init__` 的 stub

#### Scenario: Initializer 模式下 override `__init__` 只命中显式 `__init__`
- **WHEN** Initializer 模式下 override 项为 `m::MyClass::__init__`
- **THEN** 该 override MUST 只替换显式 `__init__` 生成的 stub，MUST NOT 同时替换 `#[new]` 的 `__new__` stub
