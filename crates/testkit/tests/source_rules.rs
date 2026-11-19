//! Mechanical source rules, checked over the whole workspace.
//!
//! - Every public declaration has documentation. Sources are parsed with
//!   `syn`, so qualifiers (`unsafe`, `extern "C"`, `const`, `async`),
//!   declarations split over several lines and attributes in any layout are
//!   seen as the compiler sees them. Checked: public functions, structs,
//!   enums, unions, traits and their items, type aliases, constants, statics,
//!   inline and file modules, exported macros, and public methods, constants
//!   and types of inherent `impl` blocks; and the root of every crate.
//!   Not required: struct fields, enum variants and trait `impl` items, which
//!   carry documentation only where it says more than their name and type.
//! - Comments are self-contained: they do not point to documents, design
//!   records, milestones, tickets or task identifiers.
//! - The license file is present.
//!
//! What these rules cannot see: items generated inside `macro_rules!`
//! bodies, and whether any comment is accurate, clear or worth keeping. The
//! first is reviewed by hand; the second is always a review question.

use std::{
    fs,
    path::{Path, PathBuf},
};

use syn::{Attribute, ImplItem, Item, TraitItem, Visibility};

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Every `.rs`, `.toml` and `.sh` file and the Makefile, outside build output,
/// prose documentation and the test projects in `fixtures`.
fn files(directory: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(directory).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if path.is_dir() {
            if !matches!(name.as_str(), "target" | ".git" | "docs" | "fixtures") {
                files(&path, out);
            }
        } else if name.ends_with(".rs")
            || name.ends_with(".toml")
            || name.ends_with(".sh")
            || name == "Makefile"
        {
            out.push(path);
        }
    }
}

/// `(path relative to the workspace, contents)` of every checked file.
fn sources() -> Vec<(String, String)> {
    let root = workspace();
    let mut paths = Vec::new();
    files(&root, &mut paths);
    paths.sort();
    paths
        .into_iter()
        .map(|path| {
            let relative = path
                .strip_prefix(&root)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            (relative, fs::read_to_string(&path).unwrap())
        })
        .collect()
}

fn has_docs(attributes: &[Attribute]) -> bool {
    attributes
        .iter()
        .any(|attribute| attribute.path().is_ident("doc"))
}

fn is_test_only(attributes: &[Attribute]) -> bool {
    attributes.iter().any(|attribute| {
        attribute.path().is_ident("cfg")
            && attribute.meta.require_list().is_ok_and(|list| {
                list.tokens
                    .to_string()
                    .split_whitespace()
                    .any(|token| token == "test")
            })
    })
}

fn is_public(visibility: &Visibility) -> bool {
    matches!(visibility, Visibility::Public(_))
}

/// A public declaration without documentation.
#[derive(Debug, PartialEq)]
struct Missing {
    /// Path of the item inside its file, e.g. `Shell::status`.
    name: String,
    /// What it is, e.g. `fn` or `struct`.
    kind: &'static str,
}

/// What [`undocumented`] found in one file.
#[derive(Debug, Default)]
struct FileFindings {
    missing: Vec<Missing>,
    /// Modules declared with `pub mod name;`, whose docs live in their own file.
    external_modules: Vec<String>,
}

fn record(findings: &mut FileFindings, scope: &str, name: String, kind: &'static str) {
    let name = if scope.is_empty() {
        name
    } else {
        format!("{scope}::{name}")
    };
    findings.missing.push(Missing { name, kind });
}

fn check_items(items: &[Item], scope: &str, findings: &mut FileFindings) {
    for item in items {
        let (visibility, attributes, name, kind): (
            Option<&Visibility>,
            &[Attribute],
            String,
            &str,
        ) = match item {
            Item::Fn(item) => (
                Some(&item.vis),
                &item.attrs,
                item.sig.ident.to_string(),
                "fn",
            ),
            Item::Struct(item) => (
                Some(&item.vis),
                &item.attrs,
                item.ident.to_string(),
                "struct",
            ),
            Item::Enum(item) => (Some(&item.vis), &item.attrs, item.ident.to_string(), "enum"),
            Item::Union(item) => (
                Some(&item.vis),
                &item.attrs,
                item.ident.to_string(),
                "union",
            ),
            Item::Type(item) => (Some(&item.vis), &item.attrs, item.ident.to_string(), "type"),
            Item::Const(item) => (
                Some(&item.vis),
                &item.attrs,
                item.ident.to_string(),
                "const",
            ),
            Item::Static(item) => (
                Some(&item.vis),
                &item.attrs,
                item.ident.to_string(),
                "static",
            ),
            Item::Trait(item) => (
                Some(&item.vis),
                &item.attrs,
                item.ident.to_string(),
                "trait",
            ),
            Item::Mod(item) => (Some(&item.vis), &item.attrs, item.ident.to_string(), "mod"),
            Item::Macro(item) => {
                let exported = item
                    .attrs
                    .iter()
                    .any(|attribute| attribute.path().is_ident("macro_export"));
                match (&item.ident, exported) {
                    (Some(ident), true) => (None, &item.attrs, ident.to_string(), "macro"),
                    _ => continue,
                }
            }
            Item::Impl(item) => {
                if item.trait_.is_none() && !is_test_only(&item.attrs) {
                    check_inherent_impl(item, scope, findings);
                }
                continue;
            }
            _ => continue,
        };
        if is_test_only(attributes) {
            continue;
        }
        let public = visibility.is_none_or(is_public);
        if public && !has_docs(attributes) {
            // A file module carries its documentation as `//!` in its file.
            let external = matches!(item, Item::Mod(module) if module.content.is_none());
            if !external {
                record(findings, scope, name.clone(), kind);
            }
        }
        match item {
            // Private modules are entered too: their public items can be
            // re-exported with `pub use`, which is not resolved here.
            Item::Mod(module) => match &module.content {
                Some((_, inner)) => check_items(inner, &qualified(scope, &name), findings),
                None if public => findings.external_modules.push(name),
                None => {}
            },
            Item::Trait(item) if public => check_trait(item, &qualified(scope, &name), findings),
            _ => {}
        }
    }
}

fn qualified(scope: &str, name: &str) -> String {
    if scope.is_empty() {
        name.to_owned()
    } else {
        format!("{scope}::{name}")
    }
}

fn check_inherent_impl(item: &syn::ItemImpl, scope: &str, findings: &mut FileFindings) {
    let owner = match item.self_ty.as_ref() {
        syn::Type::Path(path) => path
            .path
            .segments
            .last()
            .map_or_else(|| "impl".to_owned(), |segment| segment.ident.to_string()),
        _ => "impl".to_owned(),
    };
    let owner = qualified(scope, &owner);
    for member in &item.items {
        let (visibility, attributes, name, kind) = match member {
            ImplItem::Fn(member) => (
                &member.vis,
                &member.attrs,
                member.sig.ident.to_string(),
                "fn",
            ),
            ImplItem::Const(member) => (
                &member.vis,
                &member.attrs,
                member.ident.to_string(),
                "const",
            ),
            ImplItem::Type(member) => {
                (&member.vis, &member.attrs, member.ident.to_string(), "type")
            }
            _ => continue,
        };
        if is_public(visibility) && !is_test_only(attributes) && !has_docs(attributes) {
            record(findings, &owner, name, kind);
        }
    }
}

fn check_trait(item: &syn::ItemTrait, scope: &str, findings: &mut FileFindings) {
    for member in &item.items {
        let (attributes, name, kind) = match member {
            TraitItem::Fn(member) => (&member.attrs, member.sig.ident.to_string(), "fn"),
            TraitItem::Const(member) => (&member.attrs, member.ident.to_string(), "const"),
            TraitItem::Type(member) => (&member.attrs, member.ident.to_string(), "type"),
            _ => continue,
        };
        if !has_docs(attributes) {
            record(findings, scope, name, kind);
        }
    }
}

/// Parses `source` and lists its undocumented public declarations.
fn undocumented(source: &str) -> Result<FileFindings, String> {
    let file = syn::parse_file(source).map_err(|error| error.to_string())?;
    let mut findings = FileFindings::default();
    check_items(&file.items, "", &mut findings);
    Ok(findings)
}

fn crate_docs(source: &str) -> bool {
    syn::parse_file(source).is_ok_and(|file| has_docs(&file.attrs))
}

fn is_crate_root(path: &str) -> bool {
    path.ends_with("src/lib.rs") || path.ends_with("src/main.rs") || path.contains("/src/bin/")
}

/// The file that holds module `name` declared as `pub mod name;` in `path`.
fn module_file(path: &str, name: &str) -> Option<String> {
    let path = Path::new(path);
    let directory = if is_crate_root(&path.to_string_lossy())
        || path.file_name().is_some_and(|file| file == "mod.rs")
    {
        path.parent()?.to_owned()
    } else {
        path.parent()?.join(path.file_stem()?)
    };
    [
        directory.join(format!("{name}.rs")),
        directory.join(name).join("mod.rs"),
    ]
    .into_iter()
    .map(|candidate| candidate.to_string_lossy().into_owned())
    .find(|candidate| workspace().join(candidate).is_file())
}

#[test]
fn public_items_and_crate_roots_are_documented() {
    let mut problems = Vec::new();
    for (path, text) in sources() {
        if !(path.ends_with(".rs") && path.contains("/src/")) {
            continue;
        }
        if is_crate_root(&path) && !crate_docs(&text) {
            problems.push(format!("{path}: crate has no `//!` documentation"));
        }
        let findings = match undocumented(&text) {
            Ok(findings) => findings,
            Err(error) => {
                problems.push(format!("{path}: does not parse: {error}"));
                continue;
            }
        };
        for missing in findings.missing {
            problems.push(format!("{path}: {} `{}`", missing.kind, missing.name));
        }
        for module in findings.external_modules {
            match module_file(&path, &module) {
                Some(file) => {
                    let text = fs::read_to_string(workspace().join(&file)).unwrap();
                    if !crate_docs(&text) {
                        problems.push(format!("{file}: public module has no `//!` documentation"));
                    }
                }
                None => problems.push(format!(
                    "{path}: cannot find the file of `pub mod {module}`"
                )),
            }
        }
    }
    assert!(
        problems.is_empty(),
        "{} public declarations lack documentation:\n{}",
        problems.len(),
        problems.join("\n")
    );
}

fn names(source: &str) -> Vec<String> {
    undocumented(source)
        .unwrap()
        .missing
        .into_iter()
        .map(|missing| missing.name)
        .collect()
}

#[test]
fn undocumented_public_declarations_are_found_in_any_valid_syntax() {
    let source = r#"
        pub unsafe fn qualified_unsafe() {}
        pub extern "C" fn qualified_extern() {}
        pub const unsafe extern "C" fn all_qualifiers() {}
        pub async fn qualified_async() {}
        pub
        fn
        split_over_lines(
            value: u8,
        ) -> u8 {
            value
        }
        #[cfg_attr(
            feature = "never",
            derive(Debug)
        )]
        #[must_use]
        pub struct BehindMultilineAttributes;
        pub union Union { a: u8 }
        pub static STATIC: u8 = 0;
        pub const CONSTANT: u8 = 0;
        pub type Alias = u8;
        pub enum Enum { Variant }
        pub mod inline {
            pub fn nested() {}
        }
        #[macro_export]
        macro_rules! exported { () => {}; }

        impl BehindMultilineAttributes {
            pub fn method(&self) {}
            pub const ASSOCIATED: u8 = 0;
        }

        /// Documented trait.
        pub trait Trait {
            fn required(&self);
        }
    "#;
    assert_eq!(
        names(source),
        [
            "qualified_unsafe",
            "qualified_extern",
            "all_qualifiers",
            "qualified_async",
            "split_over_lines",
            "BehindMultilineAttributes",
            "Union",
            "STATIC",
            "CONSTANT",
            "Alias",
            "Enum",
            "inline",
            "inline::nested",
            "exported",
            "BehindMultilineAttributes::method",
            "BehindMultilineAttributes::ASSOCIATED",
            "Trait::required",
        ]
    );
}

#[test]
fn documented_private_and_test_declarations_are_accepted() {
    let source = r#"
        //! Crate docs.

        /// Line docs.
        #[inline]
        pub fn line_docs() {}

        #[doc = "Attribute docs."]
        pub fn attribute_docs() {}

        #[must_use]
        /// Docs after an attribute.
        pub fn docs_after_attribute() -> u8 { 0 }

        pub(crate) fn crate_visible() {}
        pub(super) fn parent_visible() {}
        fn private() {}

        /// Fields and variants need no docs of their own.
        pub struct Fields { pub value: u8 }
        /// Enum.
        pub enum Variants { First, Second(u8) }

        /// Methods of trait impls inherit the trait's docs.
        pub struct Type;
        impl Clone for Type { fn clone(&self) -> Self { Type } }
        impl Type { fn private(&self) {} pub(crate) fn internal(&self) {} }

        /// Docs.
        pub trait Trait {
            /// Docs.
            fn documented(&self);
        }

        macro_rules! local { () => {}; }
        mod private_module { pub fn reachable_only_inside() {} fn hidden() {} }
        pub mod elsewhere;

        #[cfg(test)]
        mod tests {
            pub fn helper() {}
        }
    "#;
    let findings = undocumented(source).unwrap();
    // `pub` inside a private module is still checked, because it may be
    // re-exported; reachability is not decided here.
    assert_eq!(
        findings.missing,
        [Missing {
            name: "private_module::reachable_only_inside".into(),
            kind: "fn",
        }]
    );
    assert_eq!(findings.external_modules, ["elsewhere"]);
    assert!(crate_docs(source));
    assert!(!crate_docs("pub fn f() {}"));
}

#[test]
fn source_that_does_not_parse_is_reported() {
    assert!(undocumented("pub fn broken(").is_err());
}

/// Phrases that make a comment depend on something outside the code.
const EXTERNAL_REFERENCES: &[&str] = &[
    "docs/",
    ".md",
    "adr-",
    "adr/",
    "milestone",
    "design doc",
    "spec section",
    "ticket",
    "jira",
    "todo",
    "fixme",
    "xxx",
];

fn comment_of(line: &str, path: &str) -> Option<String> {
    let trimmed = line.trim_start();
    let marker = if path.ends_with(".rs") { "//" } else { "#" };
    trimmed
        .strip_prefix(marker)
        .filter(|_| !trimmed.starts_with("#!") && !trimmed.starts_with("#["))
        .map(str::to_ascii_lowercase)
}

#[test]
fn comments_do_not_point_outside_the_code() {
    let mut found = Vec::new();
    for (path, text) in sources() {
        if path.ends_with("source_rules.rs") {
            continue;
        }
        for (index, line) in text.lines().enumerate() {
            let Some(comment) = comment_of(line, &path) else {
                continue;
            };
            if let Some(phrase) = EXTERNAL_REFERENCES
                .iter()
                .find(|phrase| comment.contains(*phrase))
            {
                found.push(format!(
                    "{path}:{}: `{phrase}` in {}",
                    index + 1,
                    line.trim()
                ));
            }
        }
    }
    assert!(
        found.is_empty(),
        "comments must be self-contained:\n{}",
        found.join("\n")
    );
}

#[test]
fn comment_lines_are_recognized_per_language() {
    assert_eq!(
        comment_of("    // See docs/x.md", "a.rs").as_deref(),
        Some(" see docs/x.md")
    );
    assert_eq!(comment_of("/// Docs", "a.rs").as_deref(), Some("/ docs"));
    assert_eq!(comment_of("# note", "Makefile").as_deref(), Some(" note"));
    assert_eq!(comment_of("#[derive(Debug)]", "a.rs"), None);
    assert_eq!(comment_of("#!/bin/sh", "a.sh"), None);
    assert_eq!(comment_of("let x = 1; // trailing", "a.rs"), None);
}

#[test]
fn the_apache_license_is_present() {
    let license = fs::read_to_string(workspace().join("LICENSE")).unwrap();
    assert!(license.contains("Apache License"));
    assert!(license.contains("Version 2.0, January 2004"));
    let manifest = fs::read_to_string(workspace().join("Cargo.toml")).unwrap();
    assert!(manifest.contains("license = \"Apache-2.0\""));
}
