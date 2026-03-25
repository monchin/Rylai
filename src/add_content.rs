//! Splice configured snippets into generated `.pyi` text after [`crate::generator`] runs.
//! Banner and `import typing` line must match [`crate::stub_constants`].

use crate::config::{
    AddContentEntry, AddContentLocation, canonical_stub_rel_path, normalize_add_content_file,
};
use crate::stub_constants::{AUTO_GENERATED_BANNER, TYPING_IMPORT_LINE};
use anyhow::{Context, Result, bail};
use std::path::Path;

/// Appends `\n` when `s` is non-empty and does not already end with a newline (`\n` or `\r\n`).
fn ensure_trailing_newline(s: &str) -> String {
    if s.is_empty() {
        return String::new();
    }
    let mut out = s.to_string();
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// Apply all `[[add_content]]` entries that match `rel_path` to `stub`.
pub fn apply_add_content(
    stub: &str,
    rel_path: &Path,
    entries: &[AddContentEntry],
) -> Result<String> {
    let key = canonical_stub_rel_path(rel_path);
    let mut heads: Vec<&str> = Vec::new();
    let mut mids: Vec<&str> = Vec::new();
    let mut tails: Vec<&str> = Vec::new();

    for e in entries {
        let p = normalize_add_content_file(&e.file)
            .with_context(|| format!("add_content.file {:?}", e.file))?;
        if canonical_stub_rel_path(&p) != key {
            continue;
        }
        let c = e.content.as_str();
        if c.is_empty() {
            continue;
        }
        match e.location {
            AddContentLocation::Head => heads.push(c),
            AddContentLocation::AfterImportTyping => mids.push(c),
            AddContentLocation::Tail => tails.push(c),
        }
    }

    if heads.is_empty() && mids.is_empty() && tails.is_empty() {
        return Ok(stub.to_string());
    }

    let mut out = stub.to_string();

    if !heads.is_empty() {
        let block = join_snippets(&heads);
        out = insert_after_auto_generated_banner(&out, &block);
    }

    if !mids.is_empty() {
        let block = join_snippets(&mids);
        out = insert_after_typing_import(&out, &block)?;
    }

    if !tails.is_empty() {
        let block = join_snippets(&tails);
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(&block);
    }

    Ok(out)
}

fn join_snippets(parts: &[&str]) -> String {
    let mut s = String::new();
    for (i, p) in parts.iter().enumerate() {
        if i > 0 && !s.ends_with('\n') && !p.starts_with('\n') {
            s.push('\n');
        }
        s.push_str(p);
    }
    ensure_trailing_newline(&s)
}

/// Insert `head` content after the auto-generated banner, or at the very beginning if the banner is absent.
fn insert_after_auto_generated_banner(stub: &str, block: &str) -> String {
    if block.is_empty() {
        return stub.to_string();
    }
    if stub.starts_with(AUTO_GENERATED_BANNER) {
        let insert_at = AUTO_GENERATED_BANNER.len();
        return format!("{}{}{}", &stub[..insert_at], block, &stub[insert_at..]);
    }
    format!("{block}{stub}")
}

fn insert_after_typing_import(stub: &str, block: &str) -> Result<String> {
    if block.is_empty() {
        return Ok(stub.to_string());
    }

    let mut line_start = 0usize;
    for line in stub.split_inclusive('\n') {
        let trimmed = line.trim_end_matches('\r').trim_end();
        if trimmed == TYPING_IMPORT_LINE {
            let insert_at = line_start + line.len();
            return Ok(format!(
                "{}{}{}",
                &stub[..insert_at],
                block,
                &stub[insert_at..]
            ));
        }
        line_start += line.len();
    }

    bail!(
        "add_content (after-import-typing): stub for this file has no line `{}`",
        TYPING_IMPORT_LINE
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AddContentEntry;
    use crate::stub_constants::{AUTO_GENERATED_BANNER, TYPING_IMPORT_LINE};

    fn entry(file: &str, location: AddContentLocation, content: &str) -> AddContentEntry {
        AddContentEntry {
            file: file.to_string(),
            location,
            content: content.to_string(),
        }
    }

    #[test]
    fn head_inserts_after_auto_generated_banner() {
        let stub = format!(
            "{}from __future__ import annotations\n\n{}\n\nx = 1\n",
            AUTO_GENERATED_BANNER, TYPING_IMPORT_LINE
        );
        let out = apply_add_content(
            stub.as_str(),
            Path::new("m.pyi"),
            &[entry(
                "m.pyi",
                AddContentLocation::Head,
                "# User note: extra typing\n",
            )],
        )
        .unwrap();
        assert!(out.starts_with(AUTO_GENERATED_BANNER));
        let after_banner = &out[AUTO_GENERATED_BANNER.len()..];
        assert!(
            after_banner.starts_with("# User note: extra typing"),
            "got:\n{out}"
        );
        assert!(after_banner.contains("from __future__"));
    }

    #[test]
    fn head_without_banner_inserts_at_file_start() {
        let stub = format!("{}\n\nx = 1\n", TYPING_IMPORT_LINE);
        let out = apply_add_content(
            stub.as_str(),
            Path::new("m.pyi"),
            &[entry("m.pyi", AddContentLocation::Head, "# note\n")],
        )
        .unwrap();
        assert!(out.starts_with("# note\n"));
        assert!(out.contains(TYPING_IMPORT_LINE));
    }

    #[test]
    fn tail_appends_and_adds_trailing_newline_when_omitted() {
        let stub = format!("{}\n\nx = 1", TYPING_IMPORT_LINE);
        let out = apply_add_content(
            stub.as_str(),
            Path::new("m.pyi"),
            &[entry("m.pyi", AddContentLocation::Tail, "y = 2")],
        )
        .unwrap();
        assert!(out.ends_with("y = 2\n"), "got:\n{out}");
    }

    #[test]
    fn after_import_typing_inserts_after_line() {
        let stub = format!(
            "from __future__ import annotations\n\n{}\n\nclass A: ...\n",
            TYPING_IMPORT_LINE
        );
        let out = apply_add_content(
            stub.as_str(),
            Path::new("m.pyi"),
            &[entry(
                "m.pyi",
                AddContentLocation::AfterImportTyping,
                "X: t.TypeAlias = int",
            )],
        )
        .unwrap();
        let needle = format!("{TYPING_IMPORT_LINE}\n");
        let pos = out.find(&needle).unwrap();
        let after = &out[pos..];
        assert!(
            after.starts_with(&format!("{TYPING_IMPORT_LINE}\nX:")),
            "got:\n{out}"
        );
    }

    #[test]
    fn wrong_file_noop() {
        let stub = format!("{}\n", TYPING_IMPORT_LINE);
        let out = apply_add_content(
            stub.as_str(),
            Path::new("a.pyi"),
            &[entry("b.pyi", AddContentLocation::Head, "x")],
        )
        .unwrap();
        assert_eq!(out, stub);
    }

    #[test]
    fn missing_typing_line_errors() {
        let stub = "x = 1\n";
        let err = apply_add_content(
            stub,
            Path::new("m.pyi"),
            &[entry("m.pyi", AddContentLocation::AfterImportTyping, "y")],
        );
        assert!(err.is_err());
    }
}
