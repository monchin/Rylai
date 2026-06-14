## Why

Rylai 当前把所有带 `#[new]` 的 PyO3 方法一律生成为 Python `__init__`。这覆盖了绝大多数场景，但 PyO3 提供了一种 [Initializer 模式](https://pyo3.rs/main/class.html#initializer)：当类（通常继承 `PyDict` 等原生类型）需要控制初始化流程时，会**同时**定义 `#[new]` 与字面量名为 `__init__` 的方法。此时 Rylai 会把两者都生成成 `def __init__`，既不符合 PyO3 语义，也会产生重复定义。Rylai 应遵循 PyO3 官方语义：`#[new]` 对应 Python `__new__`，显式 `__init__` 对应 `__init__`。

## What Changes

- 新增检测：遍历类的**全部** `#[pymethods]` 块（含 `multiple-pymethods`），判断是否存在字面量名为 `__init__` 的方法。
- **无显式 `__init__`**（现状，保持）：`#[new]` 生成为 `def __init__(self, ...) -> None`。即便 `#[new]` 的 Rust 方法名就叫 `__new__`，也仍输出 `__init__`——方法名不是分叉依据（PyO3 明确 Rust 方法名不重要）。
- **有显式 `__init__`**（Initializer 模式，新增）：
  - `#[new]` 生成为 `def __new__(cls, ...) -> Self`（首参 `cls`），返回类型复用现有 `python_version` 驱动的 Self 渲染（py ≥ 3.11 → `t.Self`，py < 3.11 → 类名）。
  - 显式 `__init__` 生成为 `def __init__(self, ...) -> None`。
- 检测条件与 PyO3 一致：只认字面量方法名 `__init__`。
- 不做额外校验或报警，完全遵循 PyO3 文档。
- 顺带修复：`#[new]` + 显式 `__init__` 不再生成重复的 `def __init__`。

## Non-goals

- 不支持 `#[pyo3(name = "__init__")]` 这类 rename——PyO3 自身不按它识别魔法方法，Rylai 也不引入 PyO3 都不承认的行为。
- 不对"只有显式 `__init__` 没有 `#[new]`"等组合做语义校验或报警——stub 生成器不承担 lint 职责。
- 不改变"无显式 `__init__` 时 `#[new]` → `__init__`"这一既有正确行为（这是设计本身，非兼容妥协）。

## Capabilities

### New Capabilities
- `constructor-emission`: PyO3 构造器（`#[new]` 与字面量 `__init__`）到 Python stub 的 `__new__`/`__init__` 映射规则与首参/返回类型契约。

### Modified Capabilities
<!-- 无。现有 self-receiver-detection 处理 receiver 参数排除，与本构造器分叉是不同问题。 -->

## Impact

- **generator（核心）**：`MethodKind::New` 的生成逻辑需根据"是否存在显式 `__init__`"分叉；`__new__` 分支首参改为 `cls`、返回类型走 `Self`。
- **collector**：需收集并向上暴露"类内是否存在字面量 `__init__` 方法"的信号（跨所有 `#[pymethods]` 块）。
- **model**：可能需新增上下文标志（如 `MethodKind::New` 携带 initializer 模式标志），不改变现有枚举对 `#[new]` 的识别。
- **CLI / config**：不受影响，无新配置项。
- **PyO3 兼容性**：行为与 [PyO3 Initializer 文档](https://pyo3.rs/main/class.html#initializer) 一致，无破坏性变更。
