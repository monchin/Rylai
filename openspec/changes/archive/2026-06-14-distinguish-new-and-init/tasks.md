# Tasks

## 1. 检测信号：字面量 `__init__` 存在性

- [x] 1.1 在 generator 的类方法生成入口（`gen_method`，与 `current_self_type` 同作用域），经 `class_has_explicit_init(class)` 计算 `has_explicit_init = class.methods.iter().any(|m| m.rust_ident == "__init__" && m.kind != MethodKind::New)`（排除 `#[new]` 本身，依据 design 决策 2），不修改 model 层
- [x] 1.2 验证 `rust_ident` 字段对字面量 `fn __init__` 取值为 `"__init__"`（`rename_all` 不影响 `rust_ident`）—— 端到端 example 生成的 `__new__`/`__init__` 证实
- [x] 1.3 验证 `#[new] fn __init__`（构造器方法名恰好为 `__init__`）被 `kind != MethodKind::New` 正确排除，不触发 Initializer 模式 —— 集成测试 `new_named_init_not_initializer_mode`
- [x] 1.4 检测覆盖 `multiple-pymethods`：`class_has_explicit_init` 遍历 collector 聚合后的 `class.methods`，跨块聚合由 collector 既有逻辑保证

## 2. Generator 分叉：`MethodKind::New` 的 Initializer 分支

- [x] 2.1 `emit_method_signature_lines` 的 `MethodKind::New` 分支经 `MethodSignatureEmitOpts.has_explicit_init` 接收上下文并按其值分叉
- [x] 2.2 `has_explicit_init == false`：保持现状 —— `def __init__(self, ...) -> None`，现有 306 测试全绿（行为不变）
- [x] 2.3 `has_explicit_init == true`：生成为 `def __new__(cls, ...) -> <Self 渲染>`
- [x] 2.4 为 `__new__` 分支实现首参注入裸 `cls`（**无类型注解**，遵循 PEP 673/typeshed，与 `self` 注入对称）：用 `method_params(..., false, ...)` 取参数后手动前缀 `cls`，不改动 `method_params`/`gen_params` 签名（隔离影响）
- [x] 2.5 为 `__new__` 分支接返回类型到现有 `Self` 渲染：新增 `resolve_self_type` 构造 `Self` PyType 经 `resolve_type`（依赖 `current_self_type` + `RenderPolicy`）产出 `t.Self`（py ≥ 3.11）或类名（py < 3.11）；`method_stub_return_type` 新增 `is_initializer` 参数使 `__new__` 走 Self 而非空串

## 3. Override 系统适配

- [x] 3.1 调整 `method_override_entry_matches` 的 `#[new]` override 别名匹配：保留 rust_ident 直匹配作兜底；dunder 别名按生成模式动态选择（非 Initializer → `__init__`，Initializer → `__new__`，单别名），使别名与实际生成的 stub 名一致
- [x] 3.2 验证现有 `#[new]` override 测试（`m::Counter::__init__`、`m::TfSettings::py_new` 等）在非 Initializer 模式下行为不变 —— 314 测试全绿
- [x] 3.3 验证 Initializer 模式下单个 override 项只命中一个方法 —— 集成测试 `initializer_mode_override_init_does_not_hit_new`

## 4. 显式 `__init__` 生成

- [x] 4.1 显式 `fn __init__`（`MethodKind::Instance`）走现有实例方法路径生成为 `def __init__(self, ...) -> None`，且因 `#[new]` 在 Initializer 模式改生成 `__new__`，不再重复 —— 端到端 example 证实
- [x] 4.2 显式 `__init__` 返回类型由现有映射覆盖：`()` / `PyResult<()>` 解包到 `()` 映射为 `None`，走 `MethodKind::Instance` 路径自动得到 `-> None`，无需额外强制

## 5. 测试

- [x] 5.1 新增 `examples/initializer_mode_sample`：声明 `#[new]` 与字面量 `fn __init__` 的类（省略 `extends = PyDict` 以保持编译稳定，注释说明真实 Initializer 场景），作为 Cargo workspace 成员；rylai 端到端生成 `__new__(cls, ...) -> WithInit` + `__init__(self, ...) -> None` 验证 collector 链路
- [x] 5.2 集成测试 `initializer_mode_emits_new_and_init_separately`：`__new__`（首参 `cls`、返回类名）+ `__init__`（返回 None）同时存在、无重复定义；`initializer_mode_py312_returns_t_self` / `initializer_mode_py39_returns_class_name` 锁定版本感知返回类型
- [x] 5.3 集成测试 `new_named_new_underscore_without_init_still_emits_init`：无显式 `__init__`、`#[new]` 方法名为 `__new__` 仍输出 `__init__`（关键不变量）
- [x] 5.4 集成测试 `pyo3_name_init_does_not_trigger_initializer`：`#[pyo3(name = "__init__")]` 不触发 Initializer 模式
- [x] 5.5 `class_has_explicit_init` 遍历聚合后的 `class.methods`，multiple-pymethods 跨块聚合由 collector 既有逻辑保证；`initializer_mode_emits_new_and_init_separately`（构造聚合后的 methods vec）已验证检测正确
- [x] 5.6 集成测试 `new_named_init_not_initializer_mode`：`#[new] fn __init__` 不触发 Initializer 模式、仍输出 `__init__`（检测公式排除 `#[new]` 本身的防回归）

## 6. 文档与收尾

- [x] 6.1 README `## Features` 增补 PyO3 构造器语义条目 + `[[override]]` 段更新 `#[new]` 别名说明，均引用 [PyO3 Initializer 文档](https://pyo3.rs/main/class.html#initializer)
- [x] 6.2 CHANGELOG `[Unreleased]` 增 `### Added` 条目，注明遵循 PyO3 Initializer 文档、`#[new]` 别名对齐
- [x] 6.3 `cargo test`（314 passed）与 `cargo clippy --all-targets`（无 warning/error）全绿
- [x] 6.4 `openspec validate distinguish-new-and-init --strict` 通过
