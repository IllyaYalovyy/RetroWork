//! Packages on disk: a directory with a `package.toml`.
//!
//! A *declarative* package contains only data: extensions to points defined
//! by modules it depends on, and menu items, toolbar items and key bindings
//! that invoke those modules' commands. The host interprets it; nothing in it
//! is executed.
//!
//! A *process* package additionally names an executable and the commands it
//! implements. See [`crate::external`] for how it is run.
//!
//! Unknown keys are errors, so a misspelt key is reported instead of ignored.

use std::{
    fs, io,
    path::{Path, PathBuf},
    rc::Rc,
    time::Duration,
};

use semver::{Version, VersionReq};
use serde::Deserialize;
use serde_json::Value;

use crate::{
    activation::ActivationContext,
    external::{self, ProcessHost},
    id::{CommandId, MenuId, ModuleId},
    module::{ActivationError, Dependency, Extension, Module, ModuleKind, ModuleManifest},
};

/// File name of a package manifest.
pub const MANIFEST_FILE: &str = "package.toml";

/// Why a package directory could not be loaded.
#[derive(Debug, thiserror::Error)]
pub enum PackageError {
    #[error("cannot read {path}: {source}")]
    Io { path: PathBuf, source: io::Error },
    #[error("invalid manifest {path}: {message}")]
    Invalid { path: PathBuf, message: String },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct RawManifest {
    id: ModuleId,
    name: String,
    version: Version,
    kind: RawKind,
    requires_api: VersionReq,
    #[serde(default)]
    dependency: Vec<RawDependency>,
    #[serde(default)]
    extension: Vec<RawExtension>,
    #[serde(default)]
    menu_item: Vec<RawMenuItem>,
    #[serde(default)]
    toolbar_item: Vec<RawToolbarItem>,
    #[serde(default)]
    key_binding: Vec<RawKeyBinding>,
    process: Option<RawProcess>,
    #[serde(default)]
    command: Vec<RawCommand>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum RawKind {
    Declarative,
    Process,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDependency {
    id: ModuleId,
    version: VersionReq,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawExtension {
    point: String,
    data: Value,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawMenuItem {
    menu: MenuId,
    id: String,
    title: String,
    command: CommandId,
    args: Option<Value>,
    #[serde(default)]
    order: i32,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawToolbarItem {
    id: String,
    title: String,
    command: CommandId,
    args: Option<Value>,
    #[serde(default)]
    order: i32,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawKeyBinding {
    key: String,
    command: CommandId,
    args: Option<Value>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct RawProcess {
    /// Executable name or path. Looked up as an absolute path, then inside
    /// the package directory, then next to the host executable.
    command: String,
    #[serde(default = "default_timeout_ms")]
    timeout_ms: u64,
}

fn default_timeout_ms() -> u64 {
    3000
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCommand {
    id: CommandId,
    title: String,
}

/// A package loaded from disk, ready to be registered as a module.
pub struct Package {
    manifest: ModuleManifest,
    directory: PathBuf,
    menu_items: Vec<RawMenuItem>,
    toolbar_items: Vec<RawToolbarItem>,
    key_bindings: Vec<RawKeyBinding>,
    process: Option<RawProcess>,
    commands: Vec<RawCommand>,
}

impl Package {
    /// Loads the package in `directory`.
    pub fn load(directory: &Path) -> Result<Self, PackageError> {
        let path = directory.join(MANIFEST_FILE);
        let text = fs::read_to_string(&path).map_err(|source| PackageError::Io {
            path: path.clone(),
            source,
        })?;
        Self::parse(&text, directory).map_err(|message| PackageError::Invalid { path, message })
    }

    fn parse(text: &str, directory: &Path) -> Result<Self, String> {
        let raw: RawManifest = toml::from_str(text).map_err(|error| error.to_string())?;
        let kind = match raw.kind {
            RawKind::Declarative => {
                if raw.process.is_some() || !raw.command.is_empty() {
                    return Err(
                        "a declarative package contains no code: remove `[process]` and `[[command]]`, or set kind = \"process\""
                            .to_owned(),
                    );
                }
                ModuleKind::Declarative
            }
            RawKind::Process => {
                if raw.process.is_none() {
                    return Err("a process package needs a `[process]` table".to_owned());
                }
                if raw.command.is_empty() {
                    return Err("a process package needs at least one `[[command]]`".to_owned());
                }
                ModuleKind::Process
            }
        };
        if raw.name.trim().is_empty() {
            return Err("`name` is empty".to_owned());
        }
        Ok(Self {
            manifest: ModuleManifest {
                id: raw.id,
                name: raw.name,
                version: raw.version,
                kind,
                requires_api: raw.requires_api,
                dependencies: raw
                    .dependency
                    .into_iter()
                    .map(|dependency| Dependency {
                        id: dependency.id,
                        version: dependency.version,
                    })
                    .collect(),
                extensions: raw
                    .extension
                    .into_iter()
                    .map(|extension| Extension {
                        point: extension.point,
                        base_dir: Some(directory.to_owned()),
                        data: extension.data,
                    })
                    .collect(),
            },
            directory: directory.to_owned(),
            menu_items: raw.menu_item,
            toolbar_items: raw.toolbar_item,
            key_bindings: raw.key_binding,
            process: raw.process,
            commands: raw.command,
        })
    }

    /// The id from the manifest, available before the package is registered.
    pub fn id(&self) -> &ModuleId {
        &self.manifest.id
    }
}

impl Module for Package {
    fn manifest(&self) -> ModuleManifest {
        self.manifest.clone()
    }

    fn activate(&self, cx: &mut ActivationContext<'_>) -> Result<(), ActivationError> {
        if let Some(process) = &self.process {
            let program = external::resolve_program(&self.directory, &process.command)
                .map_err(ActivationError::new)?;
            let host = Rc::new(ProcessHost::new(
                self.manifest.id.clone(),
                program,
                Duration::from_millis(process.timeout_ms),
            ));
            for command in &self.commands {
                external::register_command(cx, &host, &command.id, &command.title)?;
            }
        }
        for item in self.menu_items.iter().cloned() {
            cx.menu_item(
                item.menu,
                &item.id,
                &item.title,
                item.command,
                item.args,
                item.order,
            )?;
        }
        for item in self.toolbar_items.iter().cloned() {
            cx.toolbar_item(&item.id, &item.title, item.command, item.args, item.order)?;
        }
        for binding in self.key_bindings.iter().cloned() {
            cx.key_binding(&binding.key, binding.command, binding.args)?;
        }
        Ok(())
    }
}

/// Loads every package directly inside `directory`, in name order.
///
/// A sub-directory without a manifest is skipped. A broken package is
/// reported and does not stop the others from loading. A missing `directory`
/// yields nothing.
pub fn load_packages(directory: &Path) -> (Vec<Package>, Vec<PackageError>) {
    let mut packages = Vec::new();
    let mut errors = Vec::new();
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return (packages, errors),
        Err(source) => {
            errors.push(PackageError::Io {
                path: directory.to_owned(),
                source,
            });
            return (packages, errors);
        }
    };
    let mut directories: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.join(MANIFEST_FILE).is_file())
        .collect();
    directories.sort();
    for directory in directories {
        match Package::load(&directory) {
            Ok(package) => packages.push(package),
            Err(error) => errors.push(error),
        }
    }
    (packages, errors)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DECLARATIVE: &str = r#"
        id = "sample.filters"
        name = "Sample Filters"
        version = "1.2.3"
        kind = "declarative"
        requires-api = "^0.1"

        [[dependency]]
        id = "retrowork.logs"
        version = "^0.1"

        [[extension]]
        point = "logs.sources"
        data = { id = "auth", path = "data/auth.log" }

        [[menu-item]]
        menu = "filters"
        id = "errors"
        title = "Errors Only"
        command = "logs.filter.set"
        args = { query = "level:error" }

        [[key-binding]]
        key = "ctrl-1"
        command = "logs.filter.set"
        args = { query = "level:error" }
    "#;

    fn parse_error(text: &str) -> String {
        match Package::parse(text, Path::new("/pkg")) {
            Ok(_) => panic!("manifest unexpectedly parsed"),
            Err(message) => message,
        }
    }

    #[test]
    fn declarative_manifest_parses_into_a_module_manifest() {
        let package = Package::parse(DECLARATIVE, Path::new("/pkg")).unwrap();
        let manifest = package.manifest();
        assert_eq!(manifest.id.as_str(), "sample.filters");
        assert_eq!(manifest.version, Version::new(1, 2, 3));
        assert_eq!(manifest.kind, ModuleKind::Declarative);
        assert_eq!(manifest.dependencies.len(), 1);
        assert_eq!(
            manifest.extensions,
            [Extension {
                point: "logs.sources".into(),
                base_dir: Some("/pkg".into()),
                data: serde_json::json!({ "id": "auth", "path": "data/auth.log" }),
            }]
        );
        assert_eq!(package.menu_items[0].order, 0);
        assert_eq!(
            package.key_bindings[0].args,
            Some(serde_json::json!({ "query": "level:error" }))
        );
    }

    #[test]
    fn declarative_package_cannot_carry_code() {
        let with_process = format!("{DECLARATIVE}\n[process]\ncommand = \"tool\"\n");
        assert!(parse_error(&with_process).contains("contains no code"));
        let with_command = format!("{DECLARATIVE}\n[[command]]\nid = \"x.y\"\ntitle = \"X\"\n");
        assert!(parse_error(&with_command).contains("contains no code"));
    }

    #[test]
    fn process_package_needs_an_executable_and_commands() {
        let base = r#"
            id = "sample.tool"
            name = "Tool"
            version = "0.1.0"
            kind = "process"
            requires-api = "^0.1"
        "#;
        assert!(parse_error(base).contains("`[process]`"));
        let no_commands = format!("{base}\n[process]\ncommand = \"tool\"\n");
        assert!(parse_error(&no_commands).contains("`[[command]]`"));
        let complete = format!("{no_commands}\n[[command]]\nid = \"tool.run\"\ntitle = \"Run\"\n");
        let package = Package::parse(&complete, Path::new("/pkg")).unwrap();
        assert_eq!(package.manifest().kind, ModuleKind::Process);
        assert_eq!(package.process.as_ref().unwrap().timeout_ms, 3000);
    }

    #[test]
    fn typos_and_invalid_values_are_errors_not_silently_ignored() {
        let typo = DECLARATIVE.replace("requires-api", "requires_api");
        assert!(parse_error(&typo).contains("requires_api"));
        let unknown_key = format!("{DECLARATIVE}\nauthor = \"me\"\n");
        assert!(parse_error(&unknown_key).contains("author"));
        let bad_id = DECLARATIVE.replace("sample.filters", "Sample Filters");
        assert!(parse_error(&bad_id).contains("Sample Filters"));
        let bad_version = DECLARATIVE.replace("1.2.3", "latest");
        assert!(!parse_error(&bad_version).is_empty());
        let bad_kind = DECLARATIVE.replace("\"declarative\"", "\"native\"");
        assert!(parse_error(&bad_kind).contains("native"));
    }

    #[test]
    fn loading_a_directory_reports_broken_packages_and_keeps_good_ones() {
        let root = tempfile::tempdir().unwrap();
        let write = |name: &str, text: &str| {
            let directory = root.path().join(name);
            fs::create_dir(&directory).unwrap();
            fs::write(directory.join(MANIFEST_FILE), text).unwrap();
        };
        write("b-good", DECLARATIVE);
        write("a-broken", "id = ");
        fs::create_dir(root.path().join("c-not-a-package")).unwrap();
        fs::write(root.path().join("stray-file.toml"), DECLARATIVE).unwrap();

        let (packages, errors) = load_packages(root.path());
        assert_eq!(packages.len(), 1);
        assert_eq!(packages[0].id().as_str(), "sample.filters");
        assert_eq!(errors.len(), 1);
        assert!(errors[0].to_string().contains("a-broken"));

        let (packages, errors) = load_packages(&root.path().join("missing"));
        assert!(packages.is_empty() && errors.is_empty());
    }
}
