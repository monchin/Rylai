//! Macro expansion pre-pass: expand user-defined `macro_rules!` macros in Rust source text before
//! `syn` parsing, so that `add_class` / `add_function` calls wrapped in custom macros are visible
//! to the collector.
//!
//! This module uses [`macro_rules_rt`] to perform text-level substitution: each macro invocation
//! site in the source string is replaced by the macro body with metavariables substituted.  The
//! result is fed to `syn::parse_file` instead of the original source.

use crate::config::MacroExpandEntry;
use anyhow::{Context, Result, bail};
use macro_rules_rt::Rule;
use proc_macro2::TokenTree;
use std::cell::RefCell;
use std::path::PathBuf;
use syn::Item;

// ── Public API ────────────────────────────────────────────────────────────────

/// Build a list of [`Rule`]s from `[[macro_expand]]` config entries.
///
/// `sources` is the list of `(path, source_text)` pairs for all `.rs` files in the crate; it is
/// needed when a config entry has no `from`/`to` and the macro definition must be discovered
/// automatically.
///
/// When `warnings` is set, discovery may record messages (skipped unparsable files, duplicate
/// `macro_rules!` definitions).
///
/// Returns one `Rule` per arm per config entry (multi-arm macros produce multiple rules).
pub fn build_macro_rules(
    entries: &[MacroExpandEntry],
    sources: &[(PathBuf, String)],
    warnings: Option<&RefCell<Vec<String>>>,
) -> Result<Vec<Rule>> {
    let mut rules = Vec::new();
    for entry in entries {
        match (&entry.from, &entry.to) {
            (Some(from), Some(to)) => {
                // Explicit from/to: wrap with the macro name and `!(...)` delimiter.
                let rules_for_entry = build_rules_for_explicit(&entry.name, from, to)
                    .with_context(|| {
                        format!(
                            "[[macro_expand]] entry {:?}: failed to build rule from explicit from/to",
                            entry.name
                        )
                    })?;
                rules.extend(rules_for_entry);
            }
            (None, None) => {
                // Auto-discover: search all source files for `macro_rules! <name> { ... }`.
                let arms = discover_macro_arms(&entry.name, sources, warnings).with_context(|| {
                    format!(
                        "[[macro_expand]] entry {:?}: failed to discover macro definition in sources",
                        entry.name
                    )
                })?;
                for (pattern, body) in &arms {
                    let rules_for_arm = build_rules_for_explicit(&entry.name, pattern, body)
                        .with_context(|| {
                            format!(
                                "[[macro_expand]] entry {:?}: failed to build rule for auto-discovered arm",
                                entry.name
                            )
                        })?;
                    rules.extend(rules_for_arm);
                }
            }
            _ => {
                bail!(
                    "[[macro_expand]] entry {:?}: `from` and `to` must both be present or both absent",
                    entry.name
                );
            }
        }
    }
    Ok(rules)
}

/// Apply all `rules` to `source` in sequence, returning the fully-expanded text.
///
/// Rules that do not match any site in the source leave the text unchanged.
/// Any rule that fails to parse (invalid input tokens) propagates an error.
pub fn expand_source(source: &str, rules: &[Rule]) -> Result<String> {
    let mut text = source.to_owned();
    for rule in rules {
        text = rule
            .replace_all(&text)
            .context("macro_expand: replace_all failed")?;
    }
    Ok(text)
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Build rules for a single arm given the macro `name`, inner `pattern` (matcher without the
/// macro name / delimiter) and `body` (transcriber).
///
/// We generate three `Rule` variants — one for each macro call delimiter `()`, `[]`, `{}` — so
/// that regardless of the delimiter used at call sites all invocations are expanded.
fn build_rules_for_explicit(name: &str, pattern: &str, body: &str) -> Result<Vec<Rule>> {
    let delimiters = [('(', ')'), ('[', ']'), ('{', '}')];
    let mut rules = Vec::new();
    for (open, close) in delimiters {
        let from_str = format!("{name}!{open}{pattern}{close}");
        let matcher: macro_rules_rt::Matcher = from_str
            .parse()
            .with_context(|| format!("failed to parse matcher: {from_str:?}"))?;
        let transcriber: macro_rules_rt::Transcriber = body
            .parse()
            .with_context(|| format!("failed to parse transcriber: {body:?}"))?;
        let rule = Rule::new(matcher, transcriber)
            .with_context(|| format!("failed to create Rule for macro {name:?}"))?;
        rules.push(rule);
    }
    Ok(rules)
}

fn push_parse_skip_summary(
    warnings: Option<&RefCell<Vec<String>>>,
    name: &str,
    parse_errors: &[(PathBuf, String)],
) {
    if parse_errors.is_empty() {
        return;
    }
    let Some(w) = warnings else {
        return;
    };
    let first = parse_errors
        .first()
        .map(|(p, e)| format!("{}: {}", p.display(), e))
        .unwrap_or_default();
    let extra = if parse_errors.len() > 1 {
        format!(" (and {} more)", parse_errors.len() - 1)
    } else {
        String::new()
    };
    w.borrow_mut().push(format!(
        "macro_expand: syn could not parse {} source file(s) while discovering `macro_rules! {}` (skipped){extra}; first: {}",
        parse_errors.len(),
        name,
        first
    ));
}

/// Search `sources` for `macro_rules! <name> { ... }` and return all its arms as
/// `(pattern_tokens, body_tokens)` string pairs.
///
/// Errors if the macro is not found in any source file with extractable arms.
fn discover_macro_arms(
    name: &str,
    sources: &[(PathBuf, String)],
    warnings: Option<&RefCell<Vec<String>>>,
) -> Result<Vec<(String, String)>> {
    let mut parse_errors: Vec<(PathBuf, String)> = Vec::new();
    let mut definitions: Vec<(PathBuf, Vec<(String, String)>)> = Vec::new();

    for (path, source) in sources {
        let file = match syn::parse_file(source) {
            Ok(f) => f,
            Err(e) => {
                parse_errors.push((path.clone(), e.to_string()));
                continue;
            }
        };
        for item in &file.items {
            if let Item::Macro(im) = item {
                let macro_name = im.mac.path.segments.last().map(|s| s.ident.to_string());
                let defined_name = im.ident.as_ref().map(|id| id.to_string());
                if macro_name.as_deref() == Some("macro_rules")
                    && defined_name.as_deref() == Some(name)
                {
                    let arms = extract_macro_arms(&im.mac.tokens).with_context(|| {
                        format!(
                            "failed to extract arms from macro_rules! {name} in {}",
                            path.display()
                        )
                    })?;
                    if !arms.is_empty() {
                        definitions.push((path.clone(), arms));
                    }
                }
            }
        }
    }

    if definitions.len() > 1
        && let Some(w) = warnings
    {
        let paths: Vec<String> = definitions
            .iter()
            .map(|(p, _)| p.display().to_string())
            .collect();
        w.borrow_mut().push(format!(
            "macro_expand: `macro_rules! {}` has extractable arms in multiple files ({}); using only {}",
            name,
            paths.join(", "),
            definitions[0].0.display()
        ));
    }

    if let Some((_, arms)) = definitions.first() {
        push_parse_skip_summary(warnings, name, &parse_errors);
        return Ok(arms.clone());
    }

    push_parse_skip_summary(warnings, name, &parse_errors);

    let mut msg = format!(
        "[[macro_expand]] name = {name:?}: could not find `macro_rules! {name}` with extractable arms in any source file"
    );
    if !parse_errors.is_empty() {
        msg.push_str(&format!(
            ". {} source file(s) could not be parsed by syn and were skipped (the macro definition may lie in one of them).",
            parse_errors.len()
        ));
    }
    bail!(msg)
}

/// Parse a `macro_rules!` token body and return each arm as `(pattern, body)` strings.
///
/// This is a best-effort walk over the top-level token sequence: each arm is expected as
/// `Group => Group` with a fat arrow. Unusual formatting may fail to extract arms; use explicit
/// `from` / `to` in config when auto-discovery misses your macro.
fn extract_macro_arms(tokens: &proc_macro2::TokenStream) -> Result<Vec<(String, String)>> {
    let tts: Vec<TokenTree> = tokens.clone().into_iter().collect();
    let mut arms = Vec::new();
    let mut i = 0;
    while i < tts.len() {
        // Expect a Group (the pattern group)
        let pattern_group = match &tts[i] {
            TokenTree::Group(g) => g.clone(),
            _ => {
                i += 1;
                continue;
            }
        };
        i += 1;

        // Expect `=` `>`
        if i + 1 >= tts.len() {
            break;
        }
        let is_fat_arrow = matches!(
            (&tts[i], &tts[i + 1]),
            (TokenTree::Punct(p1), TokenTree::Punct(p2))
                if p1.as_char() == '=' && p2.as_char() == '>'
        );
        if !is_fat_arrow {
            // Not a valid arm start — skip and resync
            continue;
        }
        i += 2;

        // Expect the body Group
        if i >= tts.len() {
            break;
        }
        let body_group = match &tts[i] {
            TokenTree::Group(g) => g.clone(),
            _ => {
                i += 1;
                continue;
            }
        };
        i += 1;

        // Optional trailing semicolon
        if i < tts.len()
            && let TokenTree::Punct(p) = &tts[i]
            && p.as_char() == ';'
        {
            i += 1;
        }

        arms.push((
            pattern_group.stream().to_string(),
            body_group.stream().to_string(),
        ));
    }

    Ok(arms)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── build_rules_for_explicit ──────────────────────────────────────────────

    #[test]
    fn explicit_rule_round_trips_simple_macro() {
        let rules = build_rules_for_explicit("my_macro", "$x:expr", "{ let _v = $x; }")
            .expect("build rules");
        // At least one rule must expand a `()` call correctly.
        let expanded = expand_source("my_macro!(42)", &rules).expect("expand");
        assert!(
            expanded.contains("42"),
            "expanded output should contain the argument: {expanded}"
        );
        assert!(
            !expanded.contains("my_macro"),
            "macro call should have been replaced: {expanded}"
        );
    }

    #[test]
    fn explicit_rule_bracket_delimiter() {
        let rules = build_rules_for_explicit("m", "$x:expr", "wrap($x)").expect("build rules");
        let expanded = expand_source("m![hello]", &rules).expect("expand");
        assert!(
            expanded.contains("hello"),
            "bracket call expanded: {expanded}"
        );
    }

    // ── extract_macro_arms ────────────────────────────────────────────────────

    #[test]
    fn extract_arms_from_simple_macro() {
        let src = r#"
macro_rules! add_things {
    ($py:expr, $m:expr, [$($cls:ty),*]) => {
        $($m.add_class::<$cls>()?;)*
    };
    ($py:expr, $m:expr) => {
        let _ = $py;
    };
}
"#;
        let file: syn::File = syn::parse_str(src).expect("parse");
        for item in &file.items {
            if let Item::Macro(im) = item {
                let arms = extract_macro_arms(&im.mac.tokens).expect("extract");
                assert_eq!(arms.len(), 2, "expected 2 arms, got {}", arms.len());
                return;
            }
        }
        panic!("no macro found");
    }

    // ── discover_macro_arms ───────────────────────────────────────────────────

    #[test]
    fn discover_finds_macro_in_sources() {
        let src = r#"
macro_rules! register {
    ($m:expr, [$($cls:ty),*]) => { $($m.add_class::<$cls>()?;)* };
}
"#;
        let sources = vec![(PathBuf::from("lib.rs"), src.to_string())];
        let arms = discover_macro_arms("register", &sources, None).expect("discover");
        assert_eq!(arms.len(), 1);
    }

    #[test]
    fn discover_errors_when_macro_not_found() {
        let sources = vec![(PathBuf::from("lib.rs"), "fn foo() {}".to_string())];
        let err = discover_macro_arms("missing_macro", &sources, None).unwrap_err();
        assert!(
            err.to_string().contains("missing_macro"),
            "error should mention macro name: {err}"
        );
    }

    #[test]
    fn discover_warns_on_unparseable_files() {
        let good = r#"
macro_rules! register {
    ($m:expr) => { $m };
}
"#;
        let sources = vec![
            (
                PathBuf::from("bad.rs"),
                "this is not valid rust <<<".to_string(),
            ),
            (PathBuf::from("lib.rs"), good.to_string()),
        ];
        let w = RefCell::new(Vec::new());
        let arms = discover_macro_arms("register", &sources, Some(&w)).expect("discover");
        assert_eq!(arms.len(), 1);
        assert!(
            !w.borrow().is_empty(),
            "expected warning about skipped file: {:?}",
            w.borrow()
        );
    }

    #[test]
    fn discover_warns_when_duplicate_definitions() {
        let body = r#"
macro_rules! register {
    ($x:expr) => { $x };
}
"#;
        let sources = vec![
            (PathBuf::from("a.rs"), body.to_string()),
            (PathBuf::from("b.rs"), body.to_string()),
        ];
        let w = RefCell::new(Vec::new());
        let _ = discover_macro_arms("register", &sources, Some(&w)).expect("discover");
        assert!(
            w.borrow().iter().any(|s| s.contains("multiple files")),
            "expected duplicate warning, got {:?}",
            w.borrow()
        );
    }

    // ── build_macro_rules + expand_source (integration) ──────────────────────

    #[test]
    fn build_and_expand_explicit_add_pymodule() {
        let entry = MacroExpandEntry {
            name: "add_pymodule".to_string(),
            from: Some(
                r#"$py:expr, $parent:expr, $name:expr, [$($cls:ty),* $(,)?]"#.to_string(),
            ),
            to: Some(
                r#"{ let sub = pyo3::types::PyModule::new($py, $name)?; $(sub.add_class::<$cls>()?;)* $parent.add_submodule(&sub)?; Ok::<_, pyo3::PyErr>(()) }"#
                    .to_string(),
            ),
        };
        let sources: Vec<(PathBuf, String)> = Vec::new();
        let rules = build_macro_rules(&[entry], &sources, None).expect("build rules");
        let input = r#"add_pymodule!(py, m, "services", [PyScheme, PyOtherClass]);"#;
        let output = expand_source(input, &rules).expect("expand");
        assert!(output.contains("add_class"), "missing add_class: {output}");
        assert!(output.contains("PyScheme"), "missing PyScheme: {output}");
        assert!(
            output.contains("PyOtherClass"),
            "missing PyOtherClass: {output}"
        );
        assert!(
            !output.contains("add_pymodule!"),
            "macro call not replaced: {output}"
        );
    }

    #[test]
    fn build_and_expand_auto_discover() {
        // macro-rules-rt requires that every metavariable inside a $(...)* repetition block is
        // itself a repeating variable. Non-repeating variables (like $m in `$($m.add_class...)*)
        // are not supported. This test uses a pattern where only the repeating $cls appears
        // inside the repetition.
        let src = r#"
macro_rules! register_classes {
    ($($cls:ty),* $(,)?) => {
        $(add_class::<$cls>()?;)*
    };
}
fn setup() -> PyResult<()> {
    register_classes!(Foo, Bar);
    Ok(())
}
"#;
        let entry = MacroExpandEntry {
            name: "register_classes".to_string(),
            from: None,
            to: None,
        };
        let sources = vec![(PathBuf::from("lib.rs"), src.to_string())];
        let rules = build_macro_rules(&[entry], &sources, None).expect("build rules");
        let output = expand_source(src, &rules).expect("expand");
        assert!(output.contains("add_class"), "missing add_class: {output}");
        assert!(output.contains("Foo"), "missing Foo: {output}");
    }
}
