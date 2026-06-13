## Context

`parse_params`（`src/collector/parse.rs`）遍历 `sig.inputs` 收集 Python 参数：遇到 `FnArg::Receiver`（`&self` 等）直接跳过；对每个 `FnArg::Typed` 调用 `is_pyo3_injected_param(&pt.ty)` 决定是否跳过，否则收集为普通参数。

`is_pyo3_injected_param` 只看类型、不看位置。上一 change 让它识别 `Py<Self>`、`Bound<'_, Self>`、`&Bound<'_, Self>`、`&Borrowed<'_, Self>` 并返回 true。由于对所有 typed 参数无差别调用，这些类型即使出现在**非 receiver 位置**也会被排除——已用编译产物验证 `fn by_normal(&self, other: Py<Self>)` 的运行时签名是 `(self, /, other)`，而 rylai 丢失了 `other`。

根因：缺少**位置**与**方法 kind**两个维度。PyO3 中，依赖 `Self` 的 receiver 类型只有在方法签名的**首个参数**且方法为实例方法时才是 receiver；其余位置是表示同类实例的普通参数。而 `Python<'_>`、`&Bound<'_, PyModule>` 是位置无关的注入，任何位置都该排除。

## Goals / Non-Goals

**Goals:**
- 让依赖 `Self` 的 receiver 类型只在 receiver 位置被排除。
- receiver 被确认后，后续同类参数作为普通参数保留并映射到正确的 Python 类型。
- `staticmethod` / `classmethod` 中的 `Py<Self>` 作为普通参数保留。
- 不回归上一 change 已修复的 receiver 位置排除。
- 保持 `Python<'_>` / `&Bound<'_, PyModule>` 等纯注入类型的无条件排除。

**Non-Goals:**
- 类型别名（如 `type Handle = Py<Self>`）解析为 receiver 的识别。
- 返回值 `-> Py<Self>` 的 Python 类型映射（走 `parse_return_type`，不经排除逻辑）。
- 修改 generator 的 `self` 前置逻辑或 `detect_method_kind` 的归类规则。

## Decisions

### 决策 1：将 `is_pyo3_injected_param` 重构为两分类谓词 + `parse_params` 状态机

**方案：** 把 `is_pyo3_injected_param` 拆成两个职责正交的谓词：

- `is_self_receiver_type(ty) -> bool`：依赖 `Self` 的 receiver 类型——`Py<Self>`、`Bound<'_, Self>`、`&Bound<'_, Self>`、`&Borrowed<'_, Self>`，以及现有归在此类的 `PyRef<'_, Self>` / `PyRefMut<'_, Self>`。
- `is_pure_injected_type(ty) -> bool`：位置无关的注入——`Python<'_>`、`&PyModule`、`&Bound<'_, PyModule>`、`&Borrowed<'_, PyModule>`。

`parse_params` 改为状态机，维护 `receiver_taken: bool`。遍历 `sig.inputs`：

- `FnArg::Receiver` → 置 `receiver_taken = true`，跳过。
- `FnArg::Typed(pt)`：
  - 若 `!receiver_taken && is_instance_method && is_self_receiver_type(&pt.ty)` → 置 `receiver_taken = true`，跳过（它是 receiver）。
  - 否则若 `is_pure_injected_type(&pt.ty)` → 跳过（位置无关注入）。
  - 否则 → 收集为普通参数。

**考虑过的替代方案：**
- _给 `is_pyo3_injected_param(ty, is_receiver_position)` 增加位置布尔_：把"类型是否注入"与"是否处于 receiver 位置"两个正交关注点耦合进一个函数，调用方需预先算好位置，且无法清晰表达"纯注入无条件"与"Self 类型条件"的区分。已否决。
- _保留单函数、在 `parse_params` 外层加特判_：位置判断散落，难维护。已否决。

**理由：** 两分类直接对应 PyO3 语义（位置无关注入 vs 位置敏感 receiver）；状态机使"receiver 是否已被占用"显式，且天然覆盖"首个 typed receiver 之后再次出现 Self 类型"的边界。

### 决策 2：receiver 位置判定需要传入方法 kind

**方案：** receiver 位置 = 方法**带有实例 receiver**且参数为签名首个参数（其前无 `FnArg::Receiver`）。为此给 `parse_params` 增加 `is_instance_method: bool` 参数，由两个调用方传入：

- pyfunction 解析处：恒为 `false`（pyfunction 无 `Self` 上下文，`Py<Self>` 本就不会合法出现，传 false 保证任何 `Py<Self>` 都当普通参数）。
- pymethods 方法解析处：由 `detect_method_kind(attrs, name)` 判定，`MethodKind::Instance` / `Getter` / `Setter` 传 `true`，`Static` / `Class` / `New` 传 `false`。

**理由：** `staticmethod` / `classmethod` / `new` 没有 receiver，其 `Py<Self>` 必须作为普通参数保留。不传入 kind 会导致静态方法的首个 `Py<Self>` 被误判为 receiver 而排除。

**为何 Getter / Setter 也传 `true`：** PyO3 中 getter / setter 本质是带实例 receiver 的方法，receiver 同样可写作 `&self` / `&mut self`，或 typed 形式（`PyRef<'_, Self>` / `PyRefMut<'_, Self>` 等，见 [PyO3 class/protocols](https://pyo3.rs/main/class/protocols) 与 [issue #1206](https://github.com/PyO3/pyo3/issues/1206)）。若对它们传 `false`，typed receiver 不会被排除——对 setter 而言是回归：generator 的 `setter_value_param_type` 取 `m.params.first()`（`src/generator.rs`），会把残留的 receiver 误当作 `value` 参数。getter 不受输出影响（generator 硬编码 `@property def prop(self)`，忽略 `params`），但为分类一致仍归为 `true`。

**关于 classmethod 的 `cls`：** `#[classmethod] fn create(cls: &Bound<'_, PyType>, ...)` 中的 `cls` 既不是 `Self` 也不是 `PyModule`，因此 `is_self_receiver_type` 与 `is_pure_injected_type` 均不命中，会被作为普通参数保留——生成 `@classmethod def create(cls, ...) -> Self`，这正符合 Python 中 classmethod 的语义，无需特殊处理。

### 决策 3：将 PyRef / PyRefMut 从"无条件注入"移入"self-receiver 类型"

**方案：** 现有代码把 `"PyRef" | "PyRefMut"` 当作无条件注入。重构后归入 `is_self_receiver_type`，使其同样位置敏感（仅 receiver 位置排除）。

**理由：** `PyRef<'_, T>` 在非 receiver 位置同样是普通参数。虽实践中 `PyRef` 几乎只在 receiver 位置使用，归类正确性要求一致。回归风险极低，新增测试覆盖。

### 决策 4：method-kind 判定收敛为 `has_instance_receiver` 函数（保持 bool 签名）

**方案：** 保持 `parse_params` 接收 `is_instance_method: bool` 不变；在 parser 内新增 `has_instance_receiver(kind: &MethodKind) -> bool`，集中表达"哪些 kind 带实例 receiver"——`Instance | Getter | Setter` 返回 `true`，`Static | Class | New` 返回 `false`。`parse_pymethod` 改为调用 `has_instance_receiver(&kind)` 取代原先内联的 `matches!`。

**考虑过的替代方案：**
- _让 `parse_params` 直接接收 `MethodKind`、在函数内部判定_（本 change 原始 Open Question 的候选）：否决。`parse_params` 的契约是"解析参数"，只需一个布尔事实（有无实例 receiver），传入完整 kind 会抬高耦合；且 `pyfunction` 无 `MethodKind`，需借用 `MethodKind::Static` 表达"无 receiver"，语义别扭。

**理由：** 同样达成了"判定知识收敛进 parser 内"的目标（调用方不再内联 `matches!`），同时保持 `parse_params` 接口最小、`pyfunction` 调用点自然（传 `false`）。若未来 `parse_params` 需依据更多 kind 信息做判定（不止 receiver 有无），再升级为接收 `MethodKind`。

## Risks / Trade-offs

- **[staticmethod / classmethod / new 边界]** → 若不传 method kind，静态方法的首个 `Py<Self>` 会被误排。决策 2 通过传入 `is_instance_method` 覆盖。
- **[getter / setter 边界]** → getter / setter 同样带实例 receiver（可为 typed 形式如 `PyRefMut<'_, Self>`）。若按"仅 Instance 为 true"分类，setter 的 typed receiver 会残留，导致 `setter_value_param_type` 取错 `value` 类型（静默回归，罕见但无测试覆盖）。决策 2 将 Getter / Setter 一并置为 `true` 规避；tasks 3.9 新增测试覆盖。
- **[`parse_params` 签名变更]** → 增加 `is_instance_method` 参数，影响 2 个调用点（pyfunction / pymethods）。改动局部、有测试覆盖。
- **[PyRef / PyRefMut 行为变化]** → 从无条件排除改为位置敏感。实践中 `PyRef` 仅在 receiver 位置出现，回归风险极低；新增负面测试（非 receiver 位置的 `PyRef`）覆盖。
- **[性能]** → 线性扫描 + 谓词调用，可忽略。

## Migration Plan

纯 parser 层重构，无数据或配置迁移。回滚 = revert 该 commit。生成的 stub 只会变得更贴近运行时签名，用户侧无需任何动作。

## Open Questions

_(无。)_

> 原"是否应改为传入完整 `MethodKind`"之问已由决策 4 以"`has_instance_receiver` 函数 + 保持 `bool` 签名"方案解决：判定知识已收敛进 parser 内（`has_instance_receiver`），而 `parse_params` 接口维持最小（仅依赖 bool）。
