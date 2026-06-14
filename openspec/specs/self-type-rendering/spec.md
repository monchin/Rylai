# Self Type Rendering

## Purpose

规格化 Rust `Self` 在生成的 Python stub（`.pyi`）中的渲染策略：根据方法种类（instance / classmethod / `__new__` / `#[staticmethod]`）与 `python_version`，决定 `Self` 渲染为 `t.Self` 关键字还是当前类的 Python 类名，以符合 [PEP 673](https://peps.python.org/pep-0673/) 对 `Self` 合法位置的规定。

## Requirements

### Requirement: 非 staticmethod 方法的 Self 渲染遵循版本感知策略

当 `#[pymethods]` 方法**不是** `#[staticmethod]`（即 instance method / classmethod / `#[new]`），且其返回类型或参数类型涉及 Rust `Self`（含裸 `Self` 及 `Py<Self>` / `PyRef<'_, Self>` / `Bound<'_, Self>` / `PyResult<Self>` / `Vec<Self>` / `Option<Self>` 等嵌套）时，该 `Self` MUST 按 `python_version` 渲染：`python_version` ≥ 3.11（PEP 673 `nativeSelf`）MUST 渲染为 `t.Self`；`python_version` < 3.11 MUST 渲染为当前类的 Python 类名（作为 forward reference，此时 stub MUST 包含 `from __future__ import annotations`）。

#### Scenario: instance method 返回 Self 在 py3.11 渲染为 t.Self
- **WHEN** `#[pymethods]` instance method 声明为 `fn echo(&self) -> Self`，`python_version = 3.11`
- **THEN** 生成的 stub MUST 为 `def echo(self) -> t.Self:`，且 stub MUST 包含 `import typing as t`

#### Scenario: instance method 返回 Self 在 py3.9 渲染为类名
- **WHEN** 同上方法，`python_version = 3.9`，类名为 `Widget`
- **THEN** 生成的 stub MUST 为 `def echo(self) -> Widget:`，且 stub MUST 包含 `from __future__ import annotations`、MUST NOT 出现 `t.Self`

#### Scenario: classmethod 返回 Self 在 py3.11 渲染为 t.Self
- **WHEN** `#[pymethods]` classmethod 声明为 `#[classmethod] fn factory(cls) -> Self`，`python_version = 3.11`
- **THEN** 该 classmethod 的 `Self` 返回类型 MUST 渲染为 `t.Self:`（PEP 673 接受 classmethod 中的 `Self`）

#### Scenario: Initializer 模式下 `__new__` 返回 Self 在 py3.11 渲染为 t.Self
- **WHEN** Initializer 模式下 `#[new]` 方法返回 `Self`（或 `PyResult<Self>`），`python_version = 3.11`
- **THEN** 生成的 `__new__` 返回类型 MUST 为 `t.Self`

### Requirement: staticmethod 中的 Self 必须始终渲染为类名

当 `#[staticmethod]` 方法的返回类型或参数类型涉及 Rust `Self`（含裸 `Self` 及 `Py<Self>` / `PyResult<Self>` / `Vec<Self>` / `Option<Self>` 等嵌套）时，该 `Self` MUST **始终**渲染为当前类的 Python 类名，**无论 `python_version`**。因为 [PEP 673](https://peps.python.org/pep-0673/#valid-locations-for-self) 拒绝 staticmethod 中的 `Self`（没有 `self`/`cls` 可绑定），`t.Self` 会被类型检查器拒绝（实测 `ty` 报 `Self cannot be used in a static method`）。

py ≥ 3.11 时，类体内 forward reference（类名）在 `.pyi` 中合法，rylai MUST NOT 仅为 staticmethod 而额外引入 `from __future__ import annotations`。

#### Scenario: staticmethod 裸 Self 返回在 py3.11 渲染为类名
- **WHEN** `#[staticmethod] fn make_direct() -> Self`，`python_version = 3.11`，类名 `Widget`
- **THEN** 生成的 stub MUST 为 `def make_direct() -> Widget:`，且该方法 MUST NOT 出现 `t.Self`

#### Scenario: staticmethod `PyResult<Self>` 返回在 py3.11 渲染为类名
- **WHEN** `#[staticmethod] fn from_int(x: i64) -> PyResult<Self>`，`python_version = 3.11`，类名 `Widget`
- **THEN** 生成的 stub MUST 为 `def from_int(x: int) -> Widget:`

#### Scenario: staticmethod 裸 Self 参数在 py3.11 渲染为类名
- **WHEN** `#[staticmethod] fn take_direct(other: Self) -> i64`，`python_version = 3.11`，类名 `Widget`
- **THEN** 生成的 stub MUST 为 `def take_direct(other: Widget) -> int:`

#### Scenario: staticmethod `Py<Self>` 参数在 py3.11 渲染为类名（嵌套递归）
- **WHEN** `#[staticmethod] fn take_py(other: Py<Self>) -> i64`，`python_version = 3.11`，类名 `Widget`
- **THEN** 生成的 stub MUST 为 `def take_py(other: Widget) -> int:`

#### Scenario: staticmethod 的 Self 在 py3.9 行为不变
- **WHEN** `#[staticmethod] fn make_direct() -> Self`，`python_version = 3.9`，类名 `Widget`
- **THEN** 生成的 stub MUST 为 `def make_direct() -> Widget:`（与 py3.11 一致，本就用类名）

#### Scenario: 同类内 staticmethod 与其他方法共存时渲染互不干扰
- **WHEN** 同一 `#[pyclass]` 内含 instance method `fn a(&self) -> Self`、classmethod `#[classmethod] fn b(cls) -> Self`、staticmethod `#[staticmethod] fn c() -> Self`，`python_version = 3.11`，类名 `Widget`
- **THEN** instance method `a` 与 classmethod `b` 的 `Self` 返回类型 MUST 渲染为 `t.Self:`，staticmethod `c` 的 MUST 渲染为 `Widget:`（staticmethod 的类名渲染 MUST NOT 泄漏到相邻的 instance/classmethod）
