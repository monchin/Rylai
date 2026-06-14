## 1. Collector: cls receiver 识别与排除

- [ ] 1.1 在 `src/collector/parse.rs` 新增 `is_cls_receiver_type(&Type) -> bool`,识别 classmethod 唯一的 cls receiver 类型 `&Bound<'_, PyType>`(去一层 ref 后 seg=`Bound` 且泛型实参为 `PyType`;此写法顺带覆盖 by-value `Bound<'_, PyType>`,因 pyo3 不接受该形式作 cls,无副作用)。复用现有 `has_generic_arg_named` 辅助;与 `is_self_receiver_type`(检测 `Self` 泛型)通过泛型实参区分,不冲突。cls 类型范围已闭合(见 design.md 决策 4),无需支持 PyO3 0.23 已移除的旧 GIL Refs `&PyType`。
- [ ] 1.2 扩展 `parse_params` 签名,新增 `is_class_method: bool` 参数(由 `parse_pymethod` 用 `has_attr(attrs, "classmethod")` 直接检测,**不**从 `MethodKind` 派生——方案 B,见 design 决策 1),并在 classmethod 首参位置调用 `is_cls_receiver_type` 排除(设置 `receiver_taken = true`)。更新 `parse_pymethod`(`src/collector/parse.rs:1237`)的调用点传参。
- [ ] 1.3 扩展 collector 单测(`src/collector/parse.rs` 测试区):classmethod `cls: &Bound<'_, PyType>` 首参被排除且不进 params;classmethod 带额外参数 `fn create(cls: &Bound<'_, PyType>, x: i32)` → params 仅含 `x`;instance 方法的 `&self` / `Py<Self>` 排除行为不回归。

## 2. Generator: classmethod 注入 cls

- [ ] 2.1 修改 `src/generator.rs:795` `MethodKind::Class` 分支:改为 `with_self=false` 调 `method_params`,再手动前置 bare `cls`(逻辑仿 `:779-783` `__new__` initializer 分支:`if rest.is_empty() { "cls" } else { format!("cls, {rest}") }`)。
- [ ] 2.2 新增 generator 单测:无参 classmethod → `def create(cls)`;带参 classmethod → `def create(cls, x: int)`;`@classmethod` decorator 仍正确。

## 3. 测试修正与端到端验证

- [ ] 3.1 修正固化 bug 的测试 `classmethod_generates_classmethod_decorator`(`src/generator.rs:3312`):断言从 `def create(self)` 改为 `def create(cls)`。
- [ ] 3.2 新增端到端测试(parse→generate 全链路):带 cls 参数的 classmethod `fn create(cls: &Bound<'_, PyType>, x: i32)` 生成 `def create(cls, x: int)`;回归覆盖 instance `def f(self, ...)` 与 static `def g(x)` 不受影响。
- [ ] 3.3 (可选)用最小 pyo3 crate 验证:定义 `#[classmethod] fn create(cls: &Bound<'_, PyType>)` 类方法,运行 rylai 生成 stub,确认输出为 `def create(cls) -> ...`,cls 参数不泄漏。

## 4. 质量门禁

- [ ] 4.1 `cargo test` 全绿(含新增与修正的测试)。
- [ ] 4.2 `cargo clippy` 无新警告。
