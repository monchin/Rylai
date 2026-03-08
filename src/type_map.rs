use syn::{Type, TypePath};

/// Convert a Rust `syn::Type` to a Python type string.
/// Returns `(python_type, needs_any_import, needs_optional_import)`.
pub fn map_type(ty: &Type, use_union_syntax: bool) -> TypeMapping {
    match ty {
        Type::Path(tp) => map_type_path(tp, use_union_syntax),
        Type::Reference(r) => map_type(&r.elem, use_union_syntax),
        Type::Tuple(t) if t.elems.is_empty() => TypeMapping::known("None"),
        Type::Tuple(t) => {
            let mapped: Vec<TypeMapping> = t.elems.iter().map(|e| map_type(e, use_union_syntax)).collect();
            let py = format!(
                "tuple[{}]",
                mapped.iter().map(|m| m.py_type.as_str()).collect::<Vec<_>>().join(", ")
            );
            TypeMapping {
                py_type: py,
                needs_any: mapped.iter().any(|m| m.needs_any),
                needs_optional: mapped.iter().any(|m| m.needs_optional),
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
    /// True if the type was unresolvable (caller may warn/error/skip based on config)
    pub is_unknown: bool,
}

impl TypeMapping {
    pub fn known(s: &str) -> Self {
        Self {
            py_type: s.to_string(),
            needs_any: false,
            needs_optional: false,
            is_unknown: false,
        }
    }

    pub fn unknown() -> Self {
        Self {
            py_type: "Any".to_string(),
            needs_any: true,
            needs_optional: false,
            is_unknown: true,
        }
    }
}

fn map_type_path(tp: &TypePath, use_union_syntax: bool) -> TypeMapping {
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
        "i8" | "i16" | "i32" | "i64" | "i128" | "isize"
        | "u8" | "u16" | "u32" | "u64" | "u128" | "usize" => return TypeMapping::known("int"),

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
        // PyResult<T> → unwrap T (errors become Python exceptions, not part of the type)
        "PyResult" | "Result" => {
            if let Some(inner) = args.first() {
                return map_type(inner, use_union_syntax);
            }
            return TypeMapping::known("None");
        }

        // Option<T> → T | None  or  Optional[T]
        "Option" => {
            if let Some(inner) = args.first() {
                let inner_mapped = map_type(inner, use_union_syntax);
                let py_type = if use_union_syntax {
                    format!("{} | None", inner_mapped.py_type)
                } else {
                    format!("Optional[{}]", inner_mapped.py_type)
                };
                return TypeMapping {
                    py_type,
                    needs_any: inner_mapped.needs_any,
                    needs_optional: !use_union_syntax,
                    is_unknown: inner_mapped.is_unknown,
                };
            }
            return TypeMapping::unknown();
        }

        // Vec<T> → list[T]
        "Vec" => {
            if let Some(inner) = args.first() {
                let inner_mapped = map_type(inner, use_union_syntax);
                return TypeMapping {
                    py_type: format!("list[{}]", inner_mapped.py_type),
                    ..inner_mapped
                };
            }
            return TypeMapping::known("list");
        }

        // HashMap<K, V> / BTreeMap<K, V> → dict[K, V]
        "HashMap" | "BTreeMap" | "IndexMap" => {
            let k = args.first().map(|t| map_type(t, use_union_syntax));
            let v = args.get(1).map(|t| map_type(t, use_union_syntax));
            match (k, v) {
                (Some(km), Some(vm)) => {
                    return TypeMapping {
                        py_type: format!("dict[{}, {}]", km.py_type, vm.py_type),
                        needs_any: km.needs_any || vm.needs_any,
                        needs_optional: km.needs_optional || vm.needs_optional,
                        is_unknown: km.is_unknown || vm.is_unknown,
                    };
                }
                _ => return TypeMapping::known("dict"),
            }
        }

        // HashSet<T> / BTreeSet<T> → set[T]
        "HashSet" | "BTreeSet" => {
            if let Some(inner) = args.first() {
                let inner_mapped = map_type(inner, use_union_syntax);
                return TypeMapping {
                    py_type: format!("set[{}]", inner_mapped.py_type),
                    ..inner_mapped
                };
            }
            return TypeMapping::known("set");
        }

        // PyAny / Py<T> / Bound<T> / PyObject → Any
        "PyAny" | "PyObject" | "PyDict" | "PyList" | "PyTuple" | "PySet"
        | "PyBytes" | "PyByteArray" | "PyString" => {
            return TypeMapping {
                py_type: "Any".to_string(),
                needs_any: true,
                needs_optional: false,
                is_unknown: false, // Known pyo3 opaque type, not truly unknown
            };
        }
        "Py" | "Bound" | "Borrowed" => {
            // Py<T> / Bound<'_, T> — recurse into T
            // For Bound<'_, T> the lifetime is a GenericArgument::Lifetime, skip it
            let type_args: Vec<&Type> = match &last_seg.arguments {
                syn::PathArguments::AngleBracketed(ab) => ab
                    .args
                    .iter()
                    .filter_map(|a| match a {
                        syn::GenericArgument::Type(t) => Some(t),
                        _ => None,
                    })
                    .collect(),
                _ => vec![],
            };
            if let Some(inner) = type_args.first() {
                return map_type(inner, use_union_syntax);
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
