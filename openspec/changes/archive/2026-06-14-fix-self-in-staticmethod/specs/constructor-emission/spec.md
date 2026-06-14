## MODIFIED Requirements

### Requirement: `__new__` 返回类型遵循版本感知的 `Self` 渲染

Initializer 模式下 `#[new]` 生成的 `__new__` 返回类型 MUST 经由 `Self` 渲染机制产出(规则见 `self-type-rendering` capability):当 `python_version` ≥ 3.11(PEP 673 `native_self`)时 MUST 为 `t.Self`;当 `python_version` < 3.11 时 MUST 为当前类的类名。

`__new__` 属 PEP 673 接受 `Self` 的位置(类构造器,带 `cls`),故其渲染与 instance method 一致;而 `#[staticmethod]` 因 PEP 673 例外改渲染类名(见 `self-type-rendering`)——**二者不再等价**,旧表述"MUST 与返回 `Self` 的 staticmethod 行为完全一致"据此作废。

#### Scenario: `python_version` 3.12 下 `__new__` 返回 `t.Self`
- **WHEN** Initializer 模式下 `python_version` 配置为 `3.12`
- **THEN** 生成的 `__new__` 签名 MUST 形如 `def __new__(cls, ...) -> t.Self:`,且 stub MUST 包含 `import typing as t`、MUST NOT 包含 `from __future__ import annotations`

#### Scenario: `python_version` 3.9 下 `__new__` 返回类名
- **WHEN** Initializer 模式下 `python_version` 配置为 `3.9`,类名为 `MyDict`
- **THEN** 生成的 `__new__` 签名 MUST 形如 `def __new__(cls, ...) -> MyDict:`,且 stub MUST 包含 `from __future__ import annotations`、MUST NOT 出现 `t.Self`
