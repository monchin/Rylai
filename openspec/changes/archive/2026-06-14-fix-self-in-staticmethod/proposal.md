## Why

rylai 在 `python_version ≥ 3.11`(`native_self` 渲染策略)时,会把 `#[staticmethod]` 中的 `Self`——无论返回类型还是参数类型,无论裸 `Self` 还是 `Py<Self>` / `PyResult<Self>` / `Vec<Self>` 等嵌套——渲染成 `t.Self`。但 [PEP 673](https://peps.python.org/pep-0673/#valid-locations-for-self) 明确**拒绝** staticmethod 中的 `Self`(没有 `self`/`cls` 可绑定),类型检查器会报错(实测 `ty`:`Self cannot be used in a static method`)。已用最小 PyO3 crate 复现并经 `ty --python-version 3.11` 验证(4 个 staticmethod 全报错);py < 3.11 用类名反而正确——这是启用 native Self 后引入的回归。

## What Changes

- generator 层引入 staticmethod 上下文标志:`#[staticmethod]` 中的 `Self` 始终解析为 Python 类名,绕过 `native_self`(对齐 py<3.11 行为)。
- instance method / classmethod / `__new__` 的 `Self` 渲染不变(PEP 673 接受这些位置)。
- 修正现有 generator 测试 `render_policy_py312_emits_native_self_and_no_future_annotations`(它用 staticmethod 断言 `t.Self`,固化了 bug)。

## Capabilities

### New Capabilities
- `self-type-rendering`:规格化 Rust `Self` → Python 的渲染策略——instance/classmethod/`__new__` 方法在 py≥3.11 渲染为 `t.Self`、py<3.11 渲染为类名;`#[staticmethod]` 无论版本均渲染为类名(PEP 673)。

### Modified Capabilities
- `constructor-emission`:其"`__new__` 返回类型遵循版本感知的 `Self` 渲染" requirement 中"该渲染 MUST 与返回 `Self` 的 staticmethod 行为完全一致"不再成立(`__new__` 用 `t.Self`、staticmethod 用类名),需修正为指向 `self-type-rendering`。
- `self-receiver-detection`:其"依赖 Self 的 receiver 类型仅在 receiver 位置被排除" requirement 的补充说明里"py ≥ 3.11 映射为 `t.Self`"对 staticmethod 不准,需限定为非 staticmethod 的普通参数。

## Impact

- 仅 **generator 层**(`src/generator.rs`):`GenCtx` 加 `in_static_method` 标志 + 扩展现有 RAII guard;`resolve_type` 在 staticmethod 上下文用临时 policy(`native_self=false`)调 `map_type`。不改 `map_type` 签名。
- 不影响 CLI / parser / config / output layout。
- 向后兼容:staticmethod 的 stub 从(违反 PEP 673 的)`t.Self` 变为类名;py<3.11 无变化。
- PyO3 兼容:不改变解析假设,仅改渲染。

## Non-goals

- metaclass 中的 `Self`(PEP 673 同样拒绝,但 rylai 不建模 Python metaclass)。
- 修改 `map_type` 签名或其递归分支(generator 层透传临时 policy 即可让 `Py<Self>`、`PyResult<Self>`、`Vec<Self>` 等嵌套形态自动正确)。
- 修改 collector/parser 层(收集层原样保留 `Self`,渲染决策在 generator)。
