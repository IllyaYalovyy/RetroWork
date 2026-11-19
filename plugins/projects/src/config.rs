//! Portable project metadata and non-overwriting import publication.

use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{Read, Write},
    path::{Component, Path, PathBuf},
};

/// Project metadata relative to the imported directory.
pub const CONFIG_PATH: &str = ".retrowork/project.toml";
const MAX_BYTES: u64 = 64 * 1024;

/// A logical category shared by project providers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Group {
    Sources,
    Tests,
    Resources,
    Libraries,
    Documentation,
    ProjectFiles,
    Other,
}
impl Group {
    /// The category's name in the project tree.
    pub fn title(self) -> &'static str {
        match self {
            Self::Sources => "Sources",
            Self::Tests => "Tests",
            Self::Resources => "Resources",
            Self::Libraries => "Libraries",
            Self::Documentation => "Documentation",
            Self::ProjectFiles => "Project Files",
            Self::Other => "Other",
        }
    }
}

/// A relative file or directory assigned to a logical category.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogicalRoot {
    pub group: Group,
    pub path: PathBuf,
}

/// Shared metadata; build manifests remain authoritative for build behavior.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectConfig {
    pub version: u32,
    pub name: String,
    pub provider: String,
    #[serde(default)]
    pub roots: Vec<LogicalRoot>,
}
impl ProjectConfig {
    /// Constructs metadata for a newly discovered project.
    pub fn new(name: String, provider: String, roots: Vec<LogicalRoot>) -> Self {
        Self {
            version: 1,
            name,
            provider,
            roots,
        }
    }

    /// Rejects unsupported versions, invalid names and escaping or duplicate paths.
    pub fn validate(&self) -> Result<(), String> {
        if self.version != 1 {
            return Err(format!(
                "unsupported project configuration version {}",
                self.version
            ));
        }
        if self.name.trim().is_empty()
            || self.name.len() > 256
            || self.name.chars().any(char::is_control)
        {
            return Err("project name must contain 1–256 printable characters".into());
        }
        retrowork_platform::ModuleId::new(&self.provider).map_err(|e| e.to_string())?;
        if self.roots.len() > 128 {
            return Err("too many logical roots".into());
        }
        let mut paths = std::collections::BTreeSet::new();
        for root in &self.roots {
            if root.path.as_os_str().is_empty()
                || root
                    .path
                    .components()
                    .any(|part| !matches!(part, Component::Normal(_)))
            {
                return Err("logical roots must be relative paths without parent traversal".into());
            }
            if !paths.insert(&root.path) {
                return Err("duplicate logical root path".into());
            }
        }
        Ok(())
    }

    /// Reads bounded metadata. Missing metadata directs the user to Import Project.
    pub fn load(directory: &Path) -> Result<Self, String> {
        let path = directory.join(CONFIG_PATH);
        let file = fs::File::open(&path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                "this folder has no RetroWork configuration; use Import Project".to_owned()
            } else {
                format!("cannot read {}: {error}", path.display())
            }
        })?;
        let mut text = String::new();
        file.take(MAX_BYTES + 1)
            .read_to_string(&mut text)
            .map_err(|e| e.to_string())?;
        if text.len() as u64 > MAX_BYTES {
            return Err("project configuration is too large".into());
        }
        let config: Self =
            toml::from_str(&text).map_err(|e| format!("invalid project configuration: {e}"))?;
        config.validate()?;
        Ok(config)
    }

    /// Atomically publishes an import without replacing existing configuration.
    pub fn create(&self, directory: &Path) -> Result<(), String> {
        self.validate()?;
        let parent = directory.join(".retrowork");
        let created = match fs::create_dir(&parent) {
            Ok(()) => true,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => false,
            Err(error) => return Err(error.to_string()),
        };
        let result = (|| {
            if fs::symlink_metadata(&parent)
                .map_err(|e| e.to_string())?
                .file_type()
                .is_symlink()
            {
                return Err("project metadata directory must not be a symbolic link".into());
            }
            let mut file = tempfile::NamedTempFile::new_in(&parent).map_err(|e| e.to_string())?;
            file.write_all(
                toml::to_string_pretty(self)
                    .map_err(|e| e.to_string())?
                    .as_bytes(),
            )
            .map_err(|e| e.to_string())?;
            file.as_file().sync_all().map_err(|e| e.to_string())?;
            file.persist_noclobber(directory.join(CONFIG_PATH))
                .map_err(|e| format!("cannot create project configuration: {e}"))?;
            Ok(())
        })();
        if result.is_err() && created {
            let _ = fs::remove_dir(parent);
        }
        result
    }
}

/// Suggests conventional source, test, resource and documentation roots without filesystem I/O.
pub fn conventional_roots(project: &crate::Project) -> Vec<LogicalRoot> {
    let mut roots = Vec::new();

    if project.units.is_empty() {
        for (group, path) in [
            (Group::Sources, "src"),
            (Group::Tests, "tests"),
            (Group::Resources, "resources"),
            (Group::Libraries, "lib"),
            (Group::Documentation, "docs"),
        ] {
            roots.push(LogicalRoot {
                group,
                path: path.into(),
            });
        }
    }
    for unit in &project.units {
        if let Some(parent) = unit
            .manifest
            .parent()
            .and_then(|path| path.strip_prefix(&project.root).ok())
        {
            for (group, suffix) in [
                (Group::Sources, "src"),
                (Group::Tests, "tests"),
                (Group::Resources, "resources"),
                (Group::Documentation, "docs"),
            ] {
                roots.push(LogicalRoot {
                    group,
                    path: parent.join(suffix),
                });
            }
        }
        if let Ok(path) = unit.manifest.strip_prefix(&project.root) {
            roots.push(LogicalRoot {
                group: Group::ProjectFiles,
                path: path.into(),
            });
        }
    }
    for path in ["README.md", "LICENSE", "doc"] {
        roots.push(LogicalRoot {
            group: Group::Documentation,
            path: path.into(),
        });
    }
    roots.push(LogicalRoot {
        group: Group::ProjectFiles,
        path: CONFIG_PATH.into(),
    });
    roots
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn import_round_trips_and_never_replaces_existing_metadata() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            ProjectConfig::load(dir.path())
                .unwrap_err()
                .contains("Import Project")
        );
        let config = ProjectConfig::new(
            "Example".into(),
            "cargo".into(),
            vec![LogicalRoot {
                group: Group::Sources,
                path: "src".into(),
            }],
        );
        config.create(dir.path()).unwrap();
        assert_eq!(ProjectConfig::load(dir.path()).unwrap(), config);
        assert!(
            ProjectConfig::new("Other".into(), "ninja".into(), vec![])
                .create(dir.path())
                .is_err()
        );
        assert_eq!(ProjectConfig::load(dir.path()).unwrap(), config);
    }
    #[test]
    fn invalid_metadata_is_rejected_without_writing() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = ProjectConfig::new("Example".into(), "cargo".into(), vec![]);
        for path in ["", "/tmp", "../src", "src/../tests"] {
            config.roots = vec![LogicalRoot {
                group: Group::Sources,
                path: path.into(),
            }];
            assert!(config.create(dir.path()).is_err());
        }
        config.roots.clear();
        config.version = 99;
        assert!(config.validate().is_err());
        config.version = 1;
        config.name.clear();
        assert!(config.validate().is_err());
        config.name = "ok".into();
        config.provider = "bad provider".into();
        assert!(config.validate().is_err());
        assert!(!dir.path().join(CONFIG_PATH).exists());
    }
    #[test]
    fn corrupt_oversized_and_unknown_fields_fail_closed() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join(".retrowork")).unwrap();
        for content in [
            "broken".to_owned(),
            "x".repeat(MAX_BYTES as usize + 1),
            "version = 1\nname = 'a'\nprovider = 'cargo'\nunknown = true".into(),
        ] {
            fs::write(dir.path().join(CONFIG_PATH), content).unwrap();
            assert!(ProjectConfig::load(dir.path()).is_err());
        }
    }
}
