# Self Receiver Detection

## Purpose

定义 Rylai 解析器在处理 PyO3 `#[pymethods]` 时如何正确识别和排除隐式 receiver 参数（`self`），确保生成的 Python stub 不会泄漏 Rust 侧的 receiver 参数名。

## Requirements

### Requirement: Py<Self> receiver 必须从 stub 参数中排除
当 `#[pymethods]` 方法的第一个参数类型为 `Py<Self>`（PyO3 的 by-value receiver）时，该参数必须被视为隐式的 `self`，且不得出现在生成的 Python stub 参数列表中。

#### Scenario: Py<Self> 方法带额外参数
- **WHEN** `#[pymethods]` 方法声明为 `fn page(slf_handle: Py<Self>, width: f64, height: f64)`
- **THEN** 生成的 stub 必须为 `def page(self, width: float, height: float) -> ...`，不含 `slf_handle` 参数

#### Scenario: Py<Self> 魔术方法（__iter__）
- **WHEN** `#[pymethods]` 方法声明为 `fn __iter__(slf: Py<Self>) -> Py<Self>`
- **THEN** 生成的 stub 必须为 `def __iter__(self) -> ...`，不含 `slf` 参数

#### Scenario: Py<Self> 为唯一参数
- **WHEN** `#[pymethods]` 方法声明为 `fn consume(slf: Py<Self>)`
- **THEN** 生成的 stub 必须为 `def consume(self) -> None`，不含 `slf` 参数

### Requirement: Bound<'_, Self> receiver 必须从 stub 参数中排除
当 `#[pymethods]` 方法的第一个参数类型为 `Bound<'_, Self>`（PyO3 的 bound receiver by value）时，该参数必须被视为隐式的 `self`，且不得出现在生成的 Python stub 参数列表中。

#### Scenario: Bound<'_, Self> 带额外参数
- **WHEN** `#[pymethods]` 方法声明为 `fn process(slf: Bound<'_, Self>, data: i32)`
- **THEN** 生成的 stub 必须为 `def process(self, data: int) -> ...`，不含 `slf` 参数

### Requirement: &Bound<'_, Self> receiver 必须从 stub 参数中排除
当 `#[pymethods]` 方法的第一个参数类型为 `&Bound<'_, Self>`（PyO3 的 borrowed bound receiver）时，该参数必须被视为隐式的 `self`，且不得出现在生成的 Python stub 参数列表中。

#### Scenario: &Bound<'_, Self> 继承场景
- **WHEN** `#[pymethods]` 方法声明为 `fn set(slf: &Bound<'_, Self>, key: String, value: Bound<'_, PyAny>)`
- **THEN** 生成的 stub 必须为 `def set(self, key: str, value: t.Any) -> None`，不含 `slf` 参数

### Requirement: Py<NonSelf> 和 Bound<'_, NonSelf> 不得被排除
当 `#[pymethods]` 或 `#[pyfunction]` 的参数类型为 `Py<T>` 或 `Bound<'_, T>` 且 `T` 不是 `Self` 时，该参数必须作为普通参数保留在 stub 中。

#### Scenario: Py<OtherClass> 作为普通参数
- **WHEN** `#[pyfunction]` 声明为 `fn process(my_obj: Py<OtherClass>)`
- **THEN** 生成的 stub 必须包含 `my_obj: OtherClass` 参数

### Requirement: 全部六种 PyO3 receiver 模式必须生成正确的 stub
解析器必须正确处理全部六种合法的 PyO3 `#[pymethods]` receiver 类型，在 stub 中生成 `self`（静态方法除外），且不得有任何 receiver 参数名泄漏。

#### Scenario: 完整 receiver 矩阵
- **WHEN** 一个类包含使用 `&self`、`&mut self`、`PyRef<'_, Self>`、`PyRefMut<'_, Self>`、`Py<Self>` 和 `&Bound<'_, Self>` receiver 的方法
- **THEN** 所有生成的 stub 必须精确地以 `self` 作为第一个参数（静态/类方法除外），且无任何 receiver 名参数泄漏
