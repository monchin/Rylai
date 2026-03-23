//! PyO3-compatible `rename_all` for struct field names exposed as Python properties.
//!
//! Rule strings match [`PyO3`](https://docs.rs/pyo3/latest/pyo3/attr.pyclass.html) /
//! `RenamingRuleLitStr` in `pyo3-macros-backend`.

use heck::{
    ToKebabCase, ToLowerCamelCase, ToShoutyKebabCase, ToShoutySnakeCase, ToSnakeCase,
    ToUpperCamelCase,
};
use std::cell::RefCell;

/// Whether `rule` is a PyO3 `rename_all` literal Rylai implements.
pub fn is_valid_pyclass_rename_all_rule(rule: &str) -> bool {
    matches!(
        rule,
        "camelCase"
            | "PascalCase"
            | "snake_case"
            | "SCREAMING_SNAKE_CASE"
            | "kebab-case"
            | "SCREAMING-KEBAB-CASE"
            | "lowercase"
            | "UPPERCASE"
    )
}

/// User-facing warning when `rename_all` is not a known rule (single line; emit at most once per class).
pub fn format_invalid_pyclass_rename_all_warning(invalid_rule: &str) -> String {
    format!(
        r#"rylai: invalid #[pyclass(rename_all = "{invalid_rule}")] — expected camelCase, kebab-case, lowercase, PascalCase, SCREAMING-KEBAB-CASE, SCREAMING_SNAKE_CASE, snake_case, or UPPERCASE; using Rust field names unchanged"#
    )
}

/// Apply PyO3 `rename_all` to a Rust field ident. On unknown `rule`, returns `ident` unchanged and
/// optionally records a warning.
pub fn apply_pyclass_rename_all(
    ident: &str,
    rule: &str,
    warnings: Option<&RefCell<Vec<String>>>,
) -> String {
    match rule {
        "camelCase" => ident.to_lower_camel_case(),
        "PascalCase" => ident.to_upper_camel_case(),
        "snake_case" => ident.to_snake_case(),
        "SCREAMING_SNAKE_CASE" => ident.to_shouty_snake_case(),
        "kebab-case" => ident.to_kebab_case(),
        "SCREAMING-KEBAB-CASE" => ident.to_shouty_kebab_case(),
        "lowercase" => flat_lowercase(ident),
        "UPPERCASE" => flat_uppercase(ident),
        other => {
            if let Some(w) = warnings {
                w.borrow_mut()
                    .push(format_invalid_pyclass_rename_all_warning(other));
            }
            ident.to_string()
        }
    }
}

fn flat_lowercase(ident: &str) -> String {
    ident
        .to_snake_case()
        .split('_')
        .flat_map(|part| part.chars().flat_map(char::to_lowercase))
        .collect()
}

fn flat_uppercase(ident: &str) -> String {
    ident
        .to_snake_case()
        .split('_')
        .flat_map(|part| part.chars().flat_map(char::to_uppercase))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn foo_bar_table() {
        assert_eq!(
            apply_pyclass_rename_all("foo_bar", "camelCase", None),
            "fooBar"
        );
        assert_eq!(
            apply_pyclass_rename_all("foo_bar", "PascalCase", None),
            "FooBar"
        );
        assert_eq!(
            apply_pyclass_rename_all("foo_bar", "snake_case", None),
            "foo_bar"
        );
        assert_eq!(
            apply_pyclass_rename_all("foo_bar", "SCREAMING_SNAKE_CASE", None),
            "FOO_BAR"
        );
        assert_eq!(
            apply_pyclass_rename_all("foo_bar", "kebab-case", None),
            "foo-bar"
        );
        assert_eq!(
            apply_pyclass_rename_all("foo_bar", "SCREAMING-KEBAB-CASE", None),
            "FOO-BAR"
        );
        assert_eq!(
            apply_pyclass_rename_all("foo_bar", "lowercase", None),
            "foobar"
        );
        assert_eq!(
            apply_pyclass_rename_all("foo_bar", "UPPERCASE", None),
            "FOOBAR"
        );
    }

    #[test]
    fn valid_rules_match_apply_arms() {
        for rule in [
            "camelCase",
            "PascalCase",
            "snake_case",
            "SCREAMING_SNAKE_CASE",
            "kebab-case",
            "SCREAMING-KEBAB-CASE",
            "lowercase",
            "UPPERCASE",
        ] {
            assert!(is_valid_pyclass_rename_all_rule(rule), "{rule}");
        }
        assert!(!is_valid_pyclass_rename_all_rule("not_a_rule"));
    }

    #[test]
    fn unknown_rule_records_warning_and_returns_ident() {
        let w = RefCell::new(Vec::new());
        let out = apply_pyclass_rename_all("foo_bar", "not_a_rule", Some(&w));
        assert_eq!(out, "foo_bar");
        assert_eq!(w.borrow().len(), 1);
        assert!(w.borrow()[0].contains("not_a_rule"));
    }
}
