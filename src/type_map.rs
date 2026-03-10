use crate::config::RenderPolicy;
use syn::{Type, TypePath};

/// Convert a Rust `syn::Type` to a Python type string.
///
/// `policy`: version-specific rendering decisions (union syntax, native Self, …).
/// `self_type`: when resolving a `#[pymethods]` method, pass the Python class name so
/// that Rust `Self` return types are mapped correctly (to `Self` or to the class name
/// depending on `policy.native_self`).
pub fn map_type(ty: &Type, policy: &RenderPolicy, self_type: Option<&str>) -> TypeMapping {
    match ty {
        Type::Path(tp) => map_type_path(tp, policy, self_type),
        Type::Reference(r) => map_type(&r.elem, policy, self_type),
        // &[u8] / [u8] → bytes  (pyo3 accepts Python `bytes` as &[u8])
        Type::Slice(s) => {
            if let Type::Path(tp) = s.elem.as_ref()
                && tp.path.is_ident("u8")
            {
                return TypeMapping::known("bytes");
            }
            TypeMapping::unknown()
        }
        Type::Tuple(t) if t.elems.is_empty() => TypeMapping::known("None"),
        Type::Tuple(t) => {
            let mapped: Vec<TypeMapping> = t
                .elems
                .iter()
                .map(|e| map_type(e, policy, self_type))
                .collect();
            let py = format!(
                "tuple[{}]",
                mapped
                    .iter()
                    .map(|m| m.py_type.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            TypeMapping {
                py_type: py,
                needs_any: mapped.iter().any(|m| m.needs_any),
                needs_optional: mapped.iter().any(|m| m.needs_optional),
                needs_self_import: mapped.iter().any(|m| m.needs_self_import),
                is_unknown: mapped.iter().any(|m| m.is_unknown),
            }
        }
        Type::ImplTrait(_) | Type::TraitObject(_) => TypeMapping::unknown(),
        _ => TypeMapping::unknown(),
    }
}

#[derive(Debug, Clone)]
pub struct TypeMapping {
    pub py_type: String,
    /// Whether the result contains `Any` (needs `from typing import Any`)
    pub needs_any: bool,
    /// Whether the result contains `Optional` (needs `from typing import Optional`)
    pub needs_optional: bool,
    /// Whether the result contains `Self` (needs `from typing import Self`, py ≥ 3.11)
    pub needs_self_import: bool,
    /// True if the type was unresolvable (caller may warn/error/skip based on config)
    pub is_unknown: bool,
}

impl TypeMapping {
    pub fn known(s: &str) -> Self {
        Self {
            py_type: s.to_string(),
            needs_any: false,
            needs_optional: false,
            needs_self_import: false,
            is_unknown: false,
        }
    }

    pub fn self_keyword() -> Self {
        Self {
            py_type: "Self".to_string(),
            needs_any: false,
            needs_optional: false,
            needs_self_import: true,
            is_unknown: false,
        }
    }

    pub fn unknown() -> Self {
        Self {
            py_type: "Any".to_string(),
            needs_any: true,
            needs_optional: false,
            needs_self_import: false,
            is_unknown: true,
        }
    }
}

fn map_type_path(tp: &TypePath, policy: &RenderPolicy, self_type: Option<&str>) -> TypeMapping {
    // Ignore leading `self::` / `crate::` qualifiers, work with the last segment chain
    let full = tp
        .path
        .segments
        .iter()
        .map(|s| s.ident.to_string())
        .collect::<Vec<_>>()
        .join("::");

    // ── Primitive scalars ────────────────────────────────────────────────────
    match full.as_str() {
        "i8" | "i16" | "i32" | "i64" | "i128" | "isize" | "u8" | "u16" | "u32" | "u64" | "u128"
        | "usize" => return TypeMapping::known("int"),

        "f32" | "f64" => return TypeMapping::known("float"),

        "bool" => return TypeMapping::known("bool"),

        "str" | "String" => return TypeMapping::known("str"),

        // Unit / never
        "()" => return TypeMapping::known("None"),

        _ => {}
    }

    // ── Generic wrappers — need to inspect the first type argument ───────────
    let last_seg = tp.path.segments.last().unwrap();
    let last_ident = last_seg.ident.to_string();
    let args = generic_args(last_seg);

    match last_ident.as_str() {
        // Rust `Self` inside a #[pymethods] block
        "Self" => {
            if policy.native_self {
                // py ≥ 3.11 (PEP 673): emit the `Self` keyword directly
                return TypeMapping::self_keyword();
            }
            // py < 3.11: substitute with the Python class name (forward reference
            // is safe because we emit `from __future__ import annotations`)
            if let Some(cls) = self_type {
                return TypeMapping::known(cls);
            }
            return TypeMapping::unknown();
        }

        // PyResult<T> → unwrap T (errors become Python exceptions, not part of the type)
        "PyResult" | "Result" => {
            if let Some(inner) = args.first() {
                return map_type(inner, policy, self_type);
            }
            return TypeMapping::known("None");
        }

        // Option<T> → T | None  or  Optional[T]
        "Option" => {
            if let Some(inner) = args.first() {
                let inner_mapped = map_type(inner, policy, self_type);
                let py_type = if policy.union_optional {
                    format!("{} | None", inner_mapped.py_type)
                } else {
                    format!("Optional[{}]", inner_mapped.py_type)
                };
                return TypeMapping {
                    py_type,
                    needs_any: inner_mapped.needs_any,
                    needs_optional: !policy.union_optional,
                    needs_self_import: inner_mapped.needs_self_import,
                    is_unknown: inner_mapped.is_unknown,
                };
            }
            return TypeMapping::unknown();
        }

        // Vec<T> → list[T], but Vec<u8> → bytes (pyo3 auto-converts to Python bytes)
        "Vec" => {
            if let Some(inner) = args.first() {
                if let Type::Path(inner_tp) = inner
                    && inner_tp.path.is_ident("u8")
                {
                    return TypeMapping::known("bytes");
                }
                let inner_mapped = map_type(inner, policy, self_type);
                return TypeMapping {
                    py_type: format!("list[{}]", inner_mapped.py_type),
                    ..inner_mapped
                };
            }
            return TypeMapping::known("list");
        }

        // HashMap<K, V> / BTreeMap<K, V> → dict[K, V]
        "HashMap" | "BTreeMap" | "IndexMap" => {
            let k = args.first().map(|t| map_type(t, policy, self_type));
            let v = args.get(1).map(|t| map_type(t, policy, self_type));
            match (k, v) {
                (Some(km), Some(vm)) => {
                    return TypeMapping {
                        py_type: format!("dict[{}, {}]", km.py_type, vm.py_type),
                        needs_any: km.needs_any || vm.needs_any,
                        needs_optional: km.needs_optional || vm.needs_optional,
                        needs_self_import: km.needs_self_import || vm.needs_self_import,
                        is_unknown: km.is_unknown || vm.is_unknown,
                    };
                }
                _ => return TypeMapping::known("dict"),
            }
        }

        // HashSet<T> / BTreeSet<T> → set[T]
        "HashSet" | "BTreeSet" => {
            if let Some(inner) = args.first() {
                let inner_mapped = map_type(inner, policy, self_type);
                return TypeMapping {
                    py_type: format!("set[{}]", inner_mapped.py_type),
                    ..inner_mapped
                };
            }
            return TypeMapping::known("set");
        }

        // PyO3 types with direct Python equivalents
        "PyBytes" => return TypeMapping::known("bytes"),
        "PyByteArray" => return TypeMapping::known("bytearray"),
        "PyString" => return TypeMapping::known("str"),
        "PyDict" => return TypeMapping::known("dict"),
        "PyList" => return TypeMapping::known("list"),
        "PyTuple" => return TypeMapping::known("tuple"),
        "PySet" => return TypeMapping::known("set"),

        // PyAny / PyObject — truly opaque, map to Any
        "PyAny" | "PyObject" => {
            return TypeMapping {
                py_type: "Any".to_string(),
                needs_any: true,
                needs_optional: false,
                needs_self_import: false,
                is_unknown: false,
            };
        }
        // PyRef<'_, T> / PyRefMut<'_, T> — in return position, emit T (e.g. Self → class name or Self)
        "PyRef" | "PyRefMut" => {
            let type_args = generic_args(last_seg);
            if let Some(inner) = type_args.first() {
                return map_type(inner, policy, self_type);
            }
            return TypeMapping::unknown();
        }

        "Py" | "Bound" | "Borrowed" => {
            // Py<T> / Bound<'_, T> — recurse into T
            // For Bound<'_, T> the lifetime is a GenericArgument::Lifetime, skip it
            let type_args = generic_args(last_seg);
            if let Some(inner) = type_args.first() {
                return map_type(inner, policy, self_type);
            }
            return TypeMapping::unknown();
        }

        _ => {}
    }

    // ── Unknown — fall through to Any ────────────────────────────────────────
    TypeMapping::unknown()
}

/// Extract angle-bracketed type arguments from a path segment.
fn generic_args(seg: &syn::PathSegment) -> Vec<&Type> {
    match &seg.arguments {
        syn::PathArguments::AngleBracketed(ab) => ab
            .args
            .iter()
            .filter_map(|a| match a {
                syn::GenericArgument::Type(t) => Some(t),
                _ => None,
            })
            .collect(),
        _ => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RenderPolicy;

    fn parse_ty(s: &str) -> Type {
        syn::parse_str(s).expect("invalid type in test")
    }

    /// Returns a policy with the given `union_optional` flag and `native_self: false`.
    /// Use this as the default in tests that do not specifically test `native_self`.
    fn p(union_optional: bool) -> RenderPolicy {
        RenderPolicy {
            union_optional,
            native_self: false,
            future_annotations: true,
        }
    }

    /// Policy that enables the native `Self` keyword (py ≥ 3.11).
    fn p_native_self() -> RenderPolicy {
        RenderPolicy {
            union_optional: true,
            native_self: true,
            future_annotations: false,
        }
    }

    /// PyO3 PyBytes in function signature should map to Python `bytes`.
    #[test]
    fn pybytes_maps_to_bytes() {
        let ty = parse_ty("PyBytes");
        let m = map_type(&ty, &p(false), None);
        assert_eq!(m.py_type, "bytes");
        assert!(!m.needs_any);
    }

    /// PyO3 PyByteArray should map to Python `bytearray`.
    #[test]
    fn pybytearray_maps_to_bytearray() {
        let ty = parse_ty("PyByteArray");
        let m = map_type(&ty, &p(false), None);
        assert_eq!(m.py_type, "bytearray");
    }

    /// PyO3 PyString should map to Python `str`.
    #[test]
    fn pystring_maps_to_str() {
        let ty = parse_ty("PyString");
        let m = map_type(&ty, &p(false), None);
        assert_eq!(m.py_type, "str");
    }

    /// PyDict / PyList / PyTuple / PySet map to dict / list / tuple / set.
    #[test]
    fn py_container_types_map_to_python_builtins() {
        assert_eq!(
            map_type(&parse_ty("PyDict"), &p(false), None).py_type,
            "dict"
        );
        assert_eq!(
            map_type(&parse_ty("PyList"), &p(false), None).py_type,
            "list"
        );
        assert_eq!(
            map_type(&parse_ty("PyTuple"), &p(false), None).py_type,
            "tuple"
        );
        assert_eq!(map_type(&parse_ty("PySet"), &p(false), None).py_type, "set");
    }

    /// `&Bound<'_, PyBytes>` (reference stripped, then Bound unwraps to PyBytes) → bytes.
    #[test]
    fn bound_pybytes_maps_to_bytes() {
        let ty = parse_ty("Bound<'_, PyBytes>");
        let m = map_type(&ty, &p(false), None);
        assert_eq!(m.py_type, "bytes");
        assert!(!m.needs_any);
    }

    /// Reference type is stripped; inner Bound<PyBytes> still maps to bytes.
    #[test]
    fn ref_bound_pybytes_maps_to_bytes() {
        let ty = parse_ty("&Bound<'_, PyBytes>");
        let m = map_type(&ty, &p(false), None);
        assert_eq!(m.py_type, "bytes");
    }

    /// `&[u8]` is pyo3's idiomatic way to accept Python `bytes` without the GIL wrapper.
    #[test]
    fn ref_slice_u8_maps_to_bytes() {
        let ty = parse_ty("&[u8]");
        let m = map_type(&ty, &p(false), None);
        assert_eq!(m.py_type, "bytes");
        assert!(!m.needs_any);
    }

    /// Bare `[u8]` (no reference) also maps to bytes.
    #[test]
    fn slice_u8_maps_to_bytes() {
        let ty = parse_ty("[u8]");
        let m = map_type(&ty, &p(false), None);
        assert_eq!(m.py_type, "bytes");
    }

    /// `Vec<u8>` is auto-converted by pyo3 to Python `bytes` on return.
    #[test]
    fn vec_u8_maps_to_bytes() {
        let ty = parse_ty("Vec<u8>");
        let m = map_type(&ty, &p(false), None);
        assert_eq!(m.py_type, "bytes");
        assert!(!m.needs_any);
    }

    /// `Vec<i32>` must still map to `list[int]`, not affected by the u8 special-case.
    #[test]
    fn vec_i32_maps_to_list_int() {
        let ty = parse_ty("Vec<i32>");
        let m = map_type(&ty, &p(false), None);
        assert_eq!(m.py_type, "list[int]");
    }

    /// `Self` without a class context falls back to `Any` (py < 3.11).
    #[test]
    fn self_without_context_maps_to_any() {
        let ty = parse_ty("Self");
        let m = map_type(&ty, &p(false), None);
        assert_eq!(m.py_type, "Any");
        assert!(m.is_unknown);
    }

    /// `Self` with a class context maps to the Python class name (py < 3.11).
    #[test]
    fn self_with_context_maps_to_class_name() {
        let ty = parse_ty("Self");
        let m = map_type(&ty, &p(false), Some("PdfDocument"));
        assert_eq!(m.py_type, "PdfDocument");
        assert!(!m.needs_any);
        assert!(!m.is_unknown);
        assert!(!m.needs_self_import);
    }

    /// `PyResult<Self>` with class context unwraps to the class name (py < 3.11).
    #[test]
    fn pyresult_self_with_context_maps_to_class_name() {
        let ty = parse_ty("PyResult<Self>");
        let m = map_type(&ty, &p(false), Some("PdfDocument"));
        assert_eq!(m.py_type, "PdfDocument");
        assert!(!m.needs_any);
    }

    /// With `native_self` (py ≥ 3.11), `Self` maps to the `Self` keyword regardless
    /// of whether a class name is provided, and `needs_self_import` is set.
    #[test]
    fn self_native_keyword_emitted_for_py311() {
        let ty = parse_ty("Self");
        let m = map_type(&ty, &p_native_self(), Some("PdfDocument"));
        assert_eq!(m.py_type, "Self");
        assert!(!m.is_unknown);
        assert!(m.needs_self_import, "Self import must be flagged");
    }

    /// `PyResult<Self>` with `native_self` unwraps to the `Self` keyword.
    #[test]
    fn pyresult_self_native_keyword_for_py311() {
        let ty = parse_ty("PyResult<Self>");
        let m = map_type(&ty, &p_native_self(), Some("PdfDocument"));
        assert_eq!(m.py_type, "Self");
        assert!(m.needs_self_import);
        assert!(!m.needs_any);
    }

    /// `PyRef<'_, Self>` (e.g. __enter__ return) with class context maps to the class name.
    #[test]
    fn pyref_self_with_context_maps_to_class_name() {
        let ty = parse_ty("pyo3::PyRef<'_, Self>");
        let m = map_type(&ty, &p(false), Some("PdfDocument"));
        assert_eq!(m.py_type, "PdfDocument");
        assert!(!m.needs_any);
    }

    /// `PyRef<'_, Self>` with `native_self` maps to the `Self` keyword.
    #[test]
    fn pyref_self_native_keyword_for_py311() {
        let ty = parse_ty("pyo3::PyRef<'_, Self>");
        let m = map_type(&ty, &p_native_self(), Some("PdfDocument"));
        assert_eq!(m.py_type, "Self");
        assert!(m.needs_self_import);
    }

    /// `Option<i32>` uses `X | None` syntax when `union_optional` is true.
    #[test]
    fn option_uses_union_syntax_when_enabled() {
        let ty = parse_ty("Option<i32>");
        let m = map_type(&ty, &p(true), None);
        assert_eq!(m.py_type, "int | None");
        assert!(!m.needs_optional);
    }

    /// `Option<i32>` uses `Optional[X]` syntax when `union_optional` is false.
    #[test]
    fn option_uses_optional_syntax_when_disabled() {
        let ty = parse_ty("Option<i32>");
        let m = map_type(&ty, &p(false), None);
        assert_eq!(m.py_type, "Optional[int]");
        assert!(m.needs_optional);
    }
}
