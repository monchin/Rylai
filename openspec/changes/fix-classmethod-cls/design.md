## Context

rylai 的 `src/collector/parse.rs` 将 PyO3 方法的参数分三类处理:

1. `FnArg::Receiver`(`&self` / `&mut self` / `self`)——直接标记 receiver 已消费。
2. `is_self_receiver_type`(`Py<Self>` / `Bound<'_, Self>` / `&Bound<'_, Self>` / `PyRef<'_, Self>` 等)——**位置敏感**:仅 instance-style 方法(`Instance` / `Getter` / `Setter`)的首参排除,其余位置保留为普通参数。由 `has_instance_receiver(kind)` 把 `Static` / `Class` / `New` 排除。
3. `is_pure_injected_type`(`Python<'_>` / `&PyModule` 等)——**位置无关**,任何位置无条件排除。

`#[classmethod]` 的 cls 参数(`&Bound<'_, PyType>`)不属上述任何一类:它不是 `Self` 泛型(不匹配 `is_self_receiver_type`),也不是框架句柄(不匹配 `is_pure_injected_type`),且 `has_instance_receiver(Class) == false`,因此被当作普通参数保留。

generator 侧(`src/generator.rs:795` `MethodKind::Class` 分支)调用 `method_params(..., with_self=true, ...)`,而 `gen_params`(`:1026`)在 `with_self=true` 时无条件 `push "self"`——于是 classmethod 既前置了错误的 `self`,又(当 Rust 带显式 cls 参数时)泄漏了 cls 参数。对比 `MethodKind::New` 的 initializer 模式(`:773-788`)已正确实现 `with_self=false` + 手动注入 `cls`。

## Goals / Non-Goals

**Goals:**
- classmethod 生成的 stub 以 bare `cls` 为首参(而非 `self`)。
- classmethod 的 cls receiver(`&Bound<'_, PyType>`)从参数列表中排除,不泄漏。
- 不回归 instance / static / `__new__` 现有行为。

**Non-Goals:**
- classmethod 的 `Self` 返回类型渲染(由 `self-type-rendering` 覆盖)。
- 支持非 pyo3 惯例的 cls 拼写(自定义参数名)。
- metaclass / 其他 Python 概念建模。

## Decisions

### 决策 1:新增 `is_cls_receiver_type`,与 `is_self_receiver_type` 对称(位置敏感)

collector 新增谓词识别 `&Bound<'_, PyType>`(PyO3 classmethod 唯一的 cls 形式),仅在 **classmethod 首参**排除。

**为何不复用 `is_self_receiver_type`**:它检测 `Self` 泛型参数(`generic_arg_is_self`),而 cls 是 `PyType`,谓词语义不同。`Bound<'_, PyType>` 与 `Bound<'_, Self>` 的区分恰恰在于泛型实参,两者不会冲突。

**为何不当作 `is_pure_injected_type`(位置无关排除)**:`Python<'_>` / `&PyModule` 是框架句柄,任何位置都不可见;但 cls 的载体类型(`&Bound<'_, PyType>`)在理论上可作为普通参数出现(向方法传 `type` 对象)。保守起见采用位置敏感(仅 classmethod 首参),与 `is_self_receiver_type` 的设计哲学一致,避免误删合法参数。

**为何不扩展 `has_instance_receiver` 把 `Class` 纳入**:instance 的 receiver 排除谓词(`is_self_receiver_type`)与 classmethod 的(`is_cls_receiver_type`)不同,合并会让 `parse_params` 的排除分支语义混淆。改为向 `parse_params` 传入 classmethod 上下文,在独立分支处理 cls。

**为何用 attr 检测 classmethod 而非依赖 kind(方案 B)**:`is_class_method` 标志由 `parse_pymethod` 用 `has_attr(attrs, "classmethod")` **直接检测 attr**,MUST NOT 从 `MethodKind` 派生。因为 `detect_method_kind`(`src/collector/parse.rs:1262`)让 `#[new]` 优先于 `#[classmethod]` 匹配,`#[new]` + `#[classmethod]` 组合的 kind 是 `New` 而非 `Class`——若从 kind 派生(方案 A),该组合的 cls 不会被排除,且需重构 `MethodKind`/`detect_method_kind`。直接检测 attr 让 cls 排除的真理("有 `#[classmethod]` ⟹ 排除 cls")不被 detect 优先级影响,`#[new]` + `#[classmethod]` 自动受益(cls 排除);其 generator 侧的 `__new__(cls)` 修复属独立 change(见 "Out of Scope"),不被本 change 阻塞或耦合。

**实现**:`parse_params` 签名由 `is_instance_method: bool` 扩展为额外携带 `is_class_method: bool`(由 `parse_pymethod` 用 `has_attr(attrs, "classmethod")` 计算,而非从 kind 派生),在 classmethod 首参位置调用 `is_cls_receiver_type` 排除。

### 决策 2:generator `MethodKind::Class` 仿 `__new__` initializer 模式注入 bare `cls`

改为 `with_self=false` 调 `method_params`,再手动前置 `cls`(逻辑与 `:779-783` 的 `__new__` initializer 分支一致):
```
let rest = method_params(m, location, false, ...)?;
let params = if rest.is_empty() { "cls" } else { format!("cls, {rest}") };
```

**为何不改 `gen_params` 支持 `cls`**:`gen_params` 的 `with_self` 只懂 `self`;cls 仅 classmethod 需要,复用已验证的 `__new__` 注入模式比扩展 `gen_params` 更一致、改动面更小。

**与 signature_override 的兼容**:`method_params`(`src/generator.rs:917`)在带 `#[pyo3(signature = (...))]` 时走 merge 分支,`with_self = false` 返回 merged(不含 self/cls),手动前置 `cls` 后结果与无 signature 路径一致;与 `__new__` initializer 同路径,无需额外处理。

### 决策 3:cls 渲染为 bare(无 `: type` 注解)

与 `__new__` 的 cls、typeshed 及 PEP 673 classmethod 示例一致,注入裸 `cls`。

**为何不给 `cls: type`**:Python 社区惯例与 typeshed 均用 bare `cls`;加注解是偏离惯例且无类型收益(cls 语义上是被调用的类/子类,`type` 无法精确表达)。

### 决策 4:cls 类型范围——仅 `&Bound<'_, PyType>`,聚焦现代 Bound API

`is_cls_receiver_type` 只识别 `&Bound<'_, PyType>` 一种类型。依据:

- PyO3 官方文档 [Class methods](https://pyo3.rs/main/class) 明文规定 classmethod 首参"implicitly has type `&Bound<'_, PyType>`",所有示例清一色此形式。
- 旧 GIL Refs API(`&PyType`)在 PyO3 0.23 已完全移除([migration guide](https://pyo3.rs/main/migration));rylai 目标版本 0.27 下不可编译,合法输入中不会出现。
- `Py<PyType>`、by-value `Bound<'_, PyType>` 文档均不认,pyo3 不接受作 cls。

**为何不兼容 `&PyType`**:rylai 自身依赖 pyo3 0.27,定位现代 Bound API;GIL Refs 已移除近两年,存量代码大多已迁移;支持它会增加一个 match 分支与误匹配风险(`&PyType` 在 0.23+ 已非合法 cls 类型,理论上仍可能作真实参数出现,虽位置敏感排除能规避,收益仍不足以抵消复杂度)。

## Risks / Trade-offs

- **[向后兼容——参数名 self→cls]** 下游 diff 变化。**Mitigation**:这是对齐 Python 惯例的修正;classmethod 用 `self` 本就是 bug,不视为破坏性变更。
- **[过度排除风险]** 若 classmethod 首参碰巧是 `&Bound<'_, PyType>` 但语义上想暴露给 Python。**Mitigation**:pyo3 `#[classmethod]` 的 cls 必为首参且由框架注入,用户不会在此位置声明可见参数;位置敏感排除契合 pyo3 语义。
- **[不支持极旧 PyO3(<0.23)的 GIL Refs]** 用 `&PyType` 写 cls 的存量代码(0.23 前)不被识别。**Mitigation**:rylai 定位现代 Bound API;此类代码在 0.27 下本就无法编译,不影响可用性。

## Migration Plan

- 无数据/配置迁移;改动纯生成层(collector + generator)。
- 回滚:revert `is_cls_receiver_type` + `parse_params` 上下文 + `MethodKind::Class` 注入三处即可恢复旧行为。

## Open Questions

(无——cls 类型范围已闭合,依据见"决策 4"。)

## Out of Scope (Found During Explore)

- **`#[new]` + `#[classmethod]` 组合**("constructor accepting a class argument",见 [PyO3 Class methods](https://pyo3.rs/main/class) "Constructors which accept a class argument" 一节):rylai 中 `detect_method_kind`(`src/collector/parse.rs:1262`)让 `#[new]` 优先于 `#[classmethod]` 匹配,该组合被归为 `MethodKind::New`,cls 参数不会被排除、且 New 分支在无 explicit `__init__` 时会生成 `__init__(self, cls: type, ...)` 而非 `__new__(cls, ...)`。这是 constructor-emission capability 的独立问题,建议另开 change 处理,不纳入本 change。
