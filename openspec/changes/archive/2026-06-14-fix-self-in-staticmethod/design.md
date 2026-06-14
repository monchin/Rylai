## Context

当前 generator 渲染 Rust `Self` 的链路横跨 `src/generator.rs` 与 `src/type_map.rs`:

- `gen_method`(`src/generator.rs:819`)进入任一方法时设置 `current_self_type = Some(class.name)`,**不区分方法种类**;用 RAII guard `RestoreCurrentSelfTypeGuard`(`src/generator.rs:164`)在返回(含 `?` early-return)时恢复为 `None`。
- `resolve_type`(`src/generator.rs:236`)是 generator 调用 `map_type` 的唯一入口,传入 `&self.policy` + 类名上下文。
- `map_type` 的 `"Self"` 分支(`src/type_map.rs:330`)只看 `policy.native_self`:`true` → `t.Self`,`false` → 类名。
- `native_self` 由 `python_version >= 3.11` 决定(`src/config.rs:387`)。

由于 `map_type` 不感知方法种类,`#[staticmethod]` 的 `Self` 在 py≥3.11 也被渲染成 `t.Self`。但 [PEP 673](https://peps.python.org/pep-0673/#valid-locations-for-self) 拒绝 staticmethod 中的 `Self`(没有 `self`/`cls` 可绑定)。

已验证:用最小 PyO3 crate(含 `#[staticmethod]` 的 4 种 `Self` 形态——裸返回、`PyResult<Self>` 返回、裸参数、`Py<Self>` 参数——加一个 instance method `-> Self` 对照)跑 rylai,py3.11 stub 的 4 个 staticmethod 全部错误 emit `t.Self`,`ty --python-version 3.11` 报 4 个 `Self cannot be used in a static method`;py3.9 用类名正确。手写"修复后期望输出"(staticmethod 用类名、instance 用 `t.Self`)经 `ty --python-version 3.11` 全 pass——证明 py3.11 下类体内 forward reference(类名)在 `.pyi` 合法,无需额外动 `from __future__ import annotations` header。

## Goals / Non-Goals

**Goals:**
- `#[staticmethod]` 中的 `Self`(返回 / 参数,裸及 `Py<Self>`、`PyResult<Self>`、`Vec<Self>`、`Option<Self>` 等嵌套)始终渲染为 Python 类名,符合 PEP 673。
- instance method / classmethod / `__new__` 的 `Self` 渲染**不变**(PEP 673 接受这些位置)。
- 不改 `map_type` 签名。
- 不回归 py<3.11 行为(本就用类名)。

**Non-Goals:**
- metaclass 中的 `Self`(PEP 673 同样拒绝,但 rylai 不建模 Python metaclass)。
- 修改 collector/parser(收集层原样保留 raw `Self`,渲染决策在 generator)。
- 修改 `map_type` 递归分支。

## Decisions

### 决策 1：generator 层引入 `in_static_method` 上下文 + 临时 policy

**方案：** `GenCtx` 加 `in_static_method: bool`;`gen_method` 对 `MethodKind::Static` 置 `true`(用 RAII guard 恢复);`resolve_type` 在该上下文且 `native_self` 时,构造临时 policy(`native_self=false`)调 `map_type`。

**理由：** `map_type` 递归处理 `Py<Self>` / `PyResult<Self>` / `Vec<Self>` / `Option<Self>` 等嵌套。generator 层透传 `native_self=false` 后,所有嵌套 `Self` 自动走类名路径,无需逐个改 `map_type` 递归分支。`map_type` 签名不变,其大量单元测试(type_map.rs)不受影响。

**考虑过的替代方案：** _给 `map_type` 加 `in_static_method` 参数透传所有递归点_——改动面大(递归调用点多 + 测试多),且把"方法上下文"这个 generator 层关注点下沉到类型映射层,职责错位。已否决。

### 决策 2：扩展现有 RAII guard 同时管理两个字段

**方案：** 现有 `RestoreCurrentSelfTypeGuard`(`src/generator.rs:164`)管理 `current_self_type`;扩展它同时管理 `in_static_method`(持有两个 raw 指针,`Drop` 时恢复两者)。重命名为 `RestoreMethodContextGuard`。

**理由：** `current_self_type` 与 `in_static_method` 生命周期完全一致(都在 `gen_method` 设置、`gen_method` 返回恢复),合并到一个 guard 比两个独立 guard 更内聚、`unsafe` 块更少(1 vs 2)。

**考虑过的替代方案：** _新增独立 `RestoreInStaticMethodGuard`_——多一个 `unsafe` 块,且两个字段语义同属"方法上下文",拆开无收益。已否决。

### 决策 3：`resolve_type` 用临时 policy,不修改 `self.policy`

**方案：** `resolve_type` 内 `let effective_policy = if self.in_static_method && self.policy.native_self { clone + native_self=false } else { self.policy.clone() }`,传 `&effective_policy` 给 `map_type`。不修改 `self.policy` 本身。

**理由：** `self.policy` 是 `GenCtx` 全局字段,代表整个文件的版本策略(还驱动 header 的 `future_annotations`);原地修改需额外 guard 且影响面大。临时 policy 每次 resolve clone 一个 4-bool `RenderPolicy`,开销可忽略,作用域局限于单次 `map_type` 调用,无副作用。

## Risks / Trade-offs

- **[guard 泄漏]** `in_static_method` 必须在 `gen_method` 返回(含 early-return)恢复 `false`。RAII guard 保证;新增"同类内 instance/classmethod/static 共存"测试覆盖防回归。
- **[`needs_self_import` dead state]** staticmethod 不再 emit `t.Self`,但 `needs_self_import` 是 dead state(`import typing as t` 无条件 emit,`src/generator.rs:80-82`),无副作用。
- **[现有测试]** `render_policy_py312_emits_native_self_and_no_future_annotations`(`src/generator.rs:1933`)用 staticmethod 断言 `t.Self`,修复后失败,需同步改为类名(本质上把 bug 的回归测试就地转为正确行为的回归测试)。
- **[spec 一致性]** `constructor-emission`、`self-receiver-detection` 现有 spec 中与本次修复冲突的表述,由本 change 的 spec delta 同步修正。

## Migration Plan

纯 generator 层,无配置/数据迁移。回滚 = revert 该 commit。生成的 staticmethod stub 从(违反 PEP 673 的)`t.Self` 变为类名,用户侧无需动作;py<3.11 无变化。
