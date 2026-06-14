## Why

rylai 生成的 `#[classmethod]` 方法 stub 把隐式 receiver 注入成 `self`,但 Python 惯例与 [PEP 673](https://peps.python.org/pep-0673/) 的 classmethod 示例一致——隐式参数应命名为 `cls`。更严重的是,当 Rust 侧 classmethod 带 PyO3 注入的 cls 参数(`&Bound<'_, PyType>`)时,该参数既未被识别为 receiver 排除、又被前置了一个错误的 `self`,生成的 stub 形如 `def create(self, cls: type, x: int)`——`self` 错误且 `cls` 参数泄漏。类型检查器与 IDE 均按 `cls` 约定处理 classmethod,此 bug 使生成的 stub 既不符惯例又可能多出/错置参数。

## What Changes

- **collector/parser 层**:将 `#[classmethod]` 首参的 cls 类型 `&Bound<'_, PyType>` 识别为 PyO3 注入的隐式 cls receiver 并排除(与 instance 的 `Py<Self>` receiver 排除同源,但绑定到 classmethod 位置)。
- **generator 层**:`MethodKind::Class` 分支改为注入 bare `cls`(仿 `__new__` initializer 模式 `with_self=false` + 手动注入),而非注入 `self`。
- 修正固化 bug 的测试 `classmethod_generates_classmethod_decorator`(`src/generator.rs:3312`):断言从 `def create(self)` 改为 `def create(cls)`。
- 新增端到端测试:带 cls 参数的 classmethod 应生成为 `def create(cls, x: int)`(cls 排除 + 正确注入)。

## Capabilities

### New Capabilities

(无)

### Modified Capabilities

- `self-receiver-detection`:扩展 receiver 识别——`#[classmethod]` 首参的 cls 类型 `&Bound<'_, PyType>` 是 PyO3 注入的隐式 receiver,MUST 从 stub 参数排除;classmethod 的隐式参数 MUST 注入为 `cls` 而非 `self`。

## Impact

- **collector/parser 层**(`src/collector/parse.rs`):`parse_params` 与 receiver 检测需感知 classmethod 的 cls receiver(目前 `has_instance_receiver` 排除 `Class`,cls 被当普通参数)。
- **generator 层**(`src/generator.rs:795`):`MethodKind::Class` 分支注入逻辑改为 `cls`。
- 不影响 CLI / config / output layout。
- 向后兼容:classmethod stub 参数名从 `self` 变为 `cls`(对齐 Python 惯例);带 cls 参数的 classmethod 不再泄漏 cls 参数。
- PyO3 兼容:识别 pyo3 classmethod 唯一的 cls receiver 类型 `&Bound<'_, PyType>`(旧 GIL Refs `&PyType` 在 PyO3 0.23 已移除,rylai 0.27 目标版本下不出现,不兼容)。

## Non-goals

- classmethod 的 `Self` 返回类型渲染(`self-type-rendering` 已覆盖,classmethod 在 py≥3.11 正确渲染 `t.Self`)。
- 修改 instance method 的 `self` 注入或 staticmethod 行为(均已正确)。
- 支持非标准的 cls 拼写(自定义参数名)——仅识别 pyo3 惯例的 cls 类型。
- 兼容 PyO3 <0.23 的旧 GIL Refs `&PyType`(已移除,见 design 决策 4)。
- `#[new]` + `#[classmethod]` 组合(constructor accepting a class argument)——独立 bug,见 design "Out of Scope",另开 change。
