## ADDED Requirements

### Requirement: 依赖 Self 的 receiver 类型仅在 receiver 位置被排除

当 `#[pymethods]` 方法的参数类型为依赖 `Self` 的 receiver 类型（`Py<Self>`、`Bound<'_, Self>`、`&Bound<'_, Self>`、`&Borrowed<'_, Self>`）时，该参数 MUST 仅在 **receiver 位置**被视为隐式 `self` 并从生成的 stub 中排除。

"receiver 位置"定义为：方法**带有实例 receiver**（`MethodKind::Instance` / `Getter` / `Setter`，即非 `#[staticmethod]` / `#[classmethod]` / `#[new]`），且该参数为方法签名中的首个参数，其前不存在 `&self` / `&mut self` / `self`（即无 `FnArg::Receiver`）。

一旦方法的 receiver 被确认（出现了 `FnArg::Receiver`，或首个 typed 参数已被识别为 receiver），后续所有依赖 `Self` 的 receiver 类型参数**必须作为普通参数保留**在 stub 中，并映射为对应的 Python 类类型。

`Python<'_>`、`&Bound<'_, PyModule>` 等与位置无关的纯注入类型不受此规则约束，继续在任何位置无条件排除。

补充：被保留为普通参数的 `Py<Self>` / `Bound<'_, Self>` 等类型，其 Python 类型取决于渲染策略——py < 3.11 映射为类名（如 `Foo`），py ≥ 3.11（`native_self`）映射为 `t.Self`。下方 scenario 以默认策略（类名）示例，两种映射均满足"作为普通参数保留"的契约。

#### Scenario: Py<Self> 在普通参数位置必须保留
- **WHEN** `#[pymethods]` 方法声明为 `fn by_normal(&self, other: Py<Self>) -> i64`
- **THEN** 生成的 stub 必须为 `def by_normal(self, other: Foo) -> int`，`other` 作为普通参数保留，不得删除

#### Scenario: receiver 位置之后再次出现的 Py<Self> 必须保留
- **WHEN** `#[pymethods]` 方法声明为 `fn m(slf: Py<Self>, other: Py<Self>) -> i64`（首个 `Py<Self>` 作 receiver，第二个作普通参数）
- **THEN** 生成的 stub 必须为 `def m(self, other: Foo) -> int`，仅首个 `slf` 被排除，`other` 作为普通参数保留

#### Scenario: staticmethod 中的 Py<Self> 必须作为普通参数保留
- **WHEN** `#[pymethods]` 中带 `#[staticmethod]` 的方法声明为 `fn make(other: Py<Self>) -> i64`
- **THEN** 生成的 stub 必须为 `def make(other: Foo) -> int`（无 `self`），`other` 作为普通参数保留，因为静态方法没有 receiver

#### Scenario: Py<Self> 在 receiver 位置仍被正确排除（不回归）
- **WHEN** `#[pymethods]` 实例方法声明为 `fn by_receiver(slf: Py<Self>, x: i64) -> i64`
- **THEN** 生成的 stub 必须为 `def by_receiver(self, x: int) -> int`，`slf` 作为 receiver 被排除（与上一 change 行为一致，不产生回归）

#### Scenario: getter / setter 的 typed Self receiver 必须被排除
- **WHEN** `#[pymethods]` 声明 `#[setter] fn set_x(slf: PyRefMut<'_, Self>, v: i32)` 与 `#[getter] fn get_x(slf: PyRef<'_, Self>) -> i32`
- **THEN** getter 生成的 stub 必须为 `@property def x(self) -> int`；setter 必须为 `@x.setter def x(self, value: int) -> None`——即 typed receiver 被排除，setter 的 `value` 取到真正的参数 `v` 而非 receiver
