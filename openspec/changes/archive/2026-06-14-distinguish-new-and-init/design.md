## Context

Rylai 把 PyO3 `#[new]` 一律映射为 Python `__init__`（见 [generator.rs:705-707](../../../src/generator.rs#L705)）。`detect_method_kind` 只看 `#[new]` 属性，不看 Rust 方法名，因此 `#[new] fn new()`、`#[new] fn py_new()`、`#[new] fn __new__()` 都生成 `def __init__`。

PyO3 另有 [Initializer 模式](https://pyo3.rs/main/class.html#initializer)：当类（通常 `#[pyclass(extends = PyDict)]` 继承原生类型）需要控制初始化流程时，会**同时**定义 `#[new]`（对应 Python `__new__`）与字面量名为 `__init__` 的方法（对应 Python `__init__`）。当前 Rylai 把两者都生成成 `def __init__`，既偏离 PyO3 语义，又产生重复定义。

现有基础设施可复用：`resolve_type` → `type_map::map_type(rust_type, policy, current_self_type, known_classes)` 已对任何返回 `Self` 的方法做版本感知渲染（py ≥ 3.11 → `t.Self`，py < 3.11 → 类名），由 `GenCtx::current_self_type`（在 `gen_method` 前设为类名）与 `RenderPolicy`（携带 `python_version`）共同决定。staticmethod 返回 `PyResult<Self>` 已走这条路。

## Goals / Non-Goals

**Goals:**
- 让 Rylai 的构造器输出与 PyO3 官方 Initializer 语义一致。
- 复用现有 `Self` 渲染机制，零新增版本逻辑。
- 修复 `#[new]` + 显式 `__init__` 的重复 `def __init__` bug。

**Non-Goals:**
- 不支持 `#[pyo3(name = "__init__")]`（PyO3 不按它识别魔法方法）。
- 不做语义校验 / 报警（无 `#[new]` 但有 `__init__` 等组合不报）。
- 不改变"无显式 `__init__` 时 `#[new]` → `__init__`"的既有行为。

## Decisions

### 决策 1：分叉依据是"是否存在显式 `__init__`"，而非 `#[new]` 的方法名

`#[new]` 在 Initializer 模式下生成 `__new__`，**仅当**类中存在字面量名为 `__init__` 的方法。即便 `#[new]` 的 Rust 名就叫 `__new__`，只要没有显式 `__init__`，仍输出 `__init__`。

**为什么不是"按 `#[new]` 方法名分叉"**：PyO3 文档明确"the Rust method name is not important here"——`#[new]` 在运行时永远注册为 Python `__new__`，与 Rust 名无关。若按 Rust 名分叉（如 `fn __new__` → 输出 `__new__`），则 `#[new] fn new()` 该输出什么会产生混乱，且偏离 PyO3 语义。Rylai 当前把 `#[new]` → `__init__` 本就是一个为贴合 Python 习惯的**语义转换**；分叉的唯一正当触发条件是"`__init__` 这个槽位已被显式占用"。

```
判定：has_explicit_init = class.methods.any(|m| m.rust_ident == "__init__" && m.kind != MethodKind::New)

┌──────────────┬──────────────────┬──────────────────────────┐
│ __init__存在 │ #[new] 生成       │ 显式 __init__ 生成        │
├──────────────┼──────────────────┼──────────────────────────┤
│ 否 (99%)     │ def __init__      │ —                         │
│              │  -> None, self    │                           │
├──────────────┼──────────────────┼──────────────────────────┤
│ 是 (Init模式)│ def __new__       │ def __init__              │
│              │  -> Self, cls     │  -> None, self            │
└──────────────┴──────────────────┴──────────────────────────┘
```

### 决策 2：检测用字面量 `rust_ident == "__init__"` 且排除 `#[new]` 本身，跨全部 `#[pymethods]` 块

PyO3 按字面量 Rust 函数名识别 `__init__`，不识别 `#[pyo3(name = "__init__")]`。Rylai 与之对齐：只匹配 `rust_ident == "__init__"`。

**必须排除 `#[new]` 本身**：`#[new] fn __init__()` 是合法的——PyO3 把它当构造器（运行时注册为 Python `__new__`），其 `kind == MethodKind::New`、`rust_ident == "__init__"`。若不排除，它会被误判成"显式 `__init__`"，使一个普通 constructor 错误进入 Initializer 模式（生成 `__new__` 而非 `__init__`）。语义原则：**一个方法不能同时是 constructor（`#[new]`）和 initializer（显式 `__init__`）**——它们是同一类的两个不同槽位。故检测条件 MUST 为 `rust_ident == "__init__" && kind != MethodKind::New`。等价的强命题：只要类内名为 `__init__` 的方法**有且只有一个**，无论它带不带 `#[new]`，都不可能是 Initializer 模式（Initializer 要求 `#[new]` 与 `__init__` 是两个不同方法）。

**范围**：扫描 `PyClass.methods` 的全部方法（collector 已跨所有 `#[pymethods]` 块聚合，含 `multiple-pymethods` feature）。无需遍历原始 AST。

### 决策 3：`__new__` 返回类型复用现有 `Self` 渲染，首参 `cls`

Initializer 模式下 `#[new]` 的返回类型标记为 `Self`，丢给现有 `resolve_type`，自动产出 `t.Self`（py ≥ 3.11）或类名（py < 3.11）——与 staticmethod 返回 `Self` 的现有行为完全一致。

**为什么不是固定类名**：会失去继承场景准确性，且与现有"返回 Self 的方法"行为不一致，引入两套渲染路径。

`__new__` 首参按 Python 约定用 `cls`（非 `self`），且**不注解类型**（裸 `cls`）。这遵循 [PEP 673](https://peps.python.org/pep-0673/) / typeshed 惯例——`__new__(cls, ...) -> Self` 正是 PEP 673 的标准示例写法，`cls`（与 `self` 一样）由类型检查器自动推断，显式 `cls: type[Self]` 冗余且偏离惯例（旧 TypeVar 写法 `cls: type[_S] -> _S` 已被 Ruff PYI019 标记为应替换为 `Self`）。当前 `method_params(m, true, ...)` 的 `with_self=true` 注入裸 `self`，`__new__` 分支同理注入裸 `cls`——同一机制换名，无需构造类型注解。

### 决策 4：数据流——`has_explicit_init` 在 generator 层即时计算，不污染 model

在 `gen_class`（类生成入口）开始时，扫描 `class.methods` 计算 `has_explicit_init: bool`，作为局部上下文传入每个 `gen_method`。遇到 `MethodKind::New` 时据此分叉。

**为什么不在 model 上加字段**：这是"生成期策略信号"，与收集期确定的 `MethodKind` 性质不同；model 只需如实记录方法，分叉策略属于生成逻辑。即时计算也避免了 collector↔generator 间额外的耦合。

**Alternative（已否决）**：在 `PyClass` 上加 `has_explicit_init` 字段——可行但把策略信息固化进数据模型，且 collector 当前无需感知此区分。

### 决策 5：override 别名单一、恒等于生成名（硬约束）

现 `#[new]` 的 override 匹配额外允许 `__init__` 别名（[generator.rs:389-396](../../../src/generator.rs#L389)）。这个别名不是凭空起的——它精确等于 `#[new]` 当前生成的 stub 名（`__init__`）。这已是当前代码隐式遵循的不变量：**override 的 dunder 别名恒等于该方法实际生成的 stub 名**。

Initializer 模式下 `#[new]` 的生成名变为 `__new__`，故其 override 别名 MUST 同步变为 `__new__`。这不是"可微调的偏好"，而是**硬约束**：若同时保留 `__init__` 和 `__new__` 两个别名指向 `#[new]`，则一个 override 项 `m::Class::__init__` 会同时命中 `#[new]`（双别名之一）与显式 `__init__`（rust_ident/name 直匹配），导致**单个 override 项重复替换两个方法的 stub**——这是 bug。

**结论**：
- override 匹配始终支持 rust_ident 直匹配（兜底）；
- `#[new]` 的 dunder 别名按生成模式动态取**唯一**值——非 Initializer 模式为 `__init__`，Initializer 模式为 `__new__`，MUST NOT 同时持有两个；
- 显式 `__init__` 仅靠 rust_ident/name 直匹配命中，无需额外别名；
- 由于 `__new__` ≠ `__init__`（生成名互异），Initializer 模式下 override 天然精确命中、无重复。

## Risks / Trade-offs

- **[Initializer 模式罕见 → 缺测试覆盖]** → 新增 `examples/initializer_mode_sample`（继承 `PyDict`，同时定义 `#[new]` + `__init__`），并在 `tests/` 加对应集成测试，锁定 `__new__`/`__init__` 分叉与首参/返回类型。
- **[`__new__` 首参 `cls` 与现有 `with_self` 注入逻辑冲突]** → 在 `MethodKind::New` 的 Initializer 分支单独处理首参注入（`cls`），不改动其他方法路径，隔离影响。
- **[override 行为变化影响现有配置]** → 现状无人在 Rylai 内使用 Initializer 模式（否则早就触发重复 `__init__` bug），实际影响面近零；仍保留 rust_ident 直匹配作兜底。
- **[trade-off：仅识别字面量 `__init__`]** → 与 PyO3 完全一致，但若未来 PyO3 改变魔法方法识别方式需同步跟进；可接受。

## Migration Plan

- stub 生成器输出型变更，对无 Initializer 模式的现有用户零影响（输出不变）。
- 对 Initializer 模式用户：从"错误（重复 `__init__`）"变为"正确（`__new__` + `__init__`）"。
- 无需特殊迁移；按 conventional commits 发一个 minor bump，CHANGELOG 注明遵循 PyO3 Initializer 文档。
- README 增补一段"Rylai 遵循 PyO3 构造器语义"的说明，引用 [PyO3 Initializer 文档](https://pyo3.rs/main/class.html#initializer)。
