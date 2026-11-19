//! Module identity, manifests and the activation contract.
//!
//! Three kinds of module share one manifest and one dependency model but have
//! deliberately different trust and capability levels:
//!
//! | Kind | Code | Runs | Distribution today |
//! |---|---|---|---|
//! | [`ModuleKind::Bundled`] | trusted Rust, compiled into the application | in-process | built with the application; third parties need a custom build |
//! | [`ModuleKind::Declarative`] | none, a `package.toml` with data | host interprets it | a directory dropped into a packages folder |
//! | [`ModuleKind::Process`] | any executable | separate OS process, JSON lines over stdio | a directory plus an executable; protocol is experimental |
//!
//! Bundled crates are *not* installable community plugins and this crate does
//! not pretend otherwise.

use std::{cell::RefCell, fmt, path::PathBuf};

use semver::{Version, VersionReq};
use serde::Serialize;
use serde_json::Value;

use crate::{activation::ActivationContext, id::ModuleId};

/// Version of the contract in this crate that packages declare compatibility with.
///
/// Pre-1.0: a minor bump may break packages, so manifests should require `^0.1`.
pub const PLATFORM_API_VERSION: Version = Version::new(0, 1, 0);

/// How a module is delivered and how far it is trusted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ModuleKind {
    /// Trusted Rust code linked into the application.
    Bundled,
    /// A data-only package. Contains no executable code.
    Declarative,
    /// Code in a separate process speaking the plugin protocol.
    Process,
}

impl fmt::Display for ModuleKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ModuleKind::Bundled => "bundled",
            ModuleKind::Declarative => "declarative",
            ModuleKind::Process => "process",
        })
    }
}

/// A module this one needs, and which of its versions are acceptable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dependency {
    pub id: ModuleId,
    pub version: VersionReq,
}

impl Dependency {
    /// A dependency written in source code.
    ///
    /// # Panics
    /// Panics on an invalid id or version requirement literal.
    #[track_caller]
    pub fn literal(id: &'static str, version: &'static str) -> Self {
        Self {
            id: ModuleId::literal(id),
            version: VersionReq::parse(version)
                .unwrap_or_else(|error| panic!("invalid version requirement `{version}`: {error}")),
        }
    }
}

/// Data contributed to an extension point defined by another module.
///
/// Extensions are declared in the manifest, so the host collects them from all
/// accepted modules *before* any module is activated. The module that owns the
/// point reads them during activation and is responsible for validating `data`.
#[derive(Clone, Debug, PartialEq)]
pub struct Extension {
    /// Name of the extension point, e.g. `logs.sources`.
    pub point: String,
    /// Directory relative paths in `data` are resolved against, when the
    /// contributor is a package on disk.
    pub base_dir: Option<PathBuf>,
    pub data: Value,
}

/// Everything the platform needs to know about a module before activating it.
#[derive(Clone, Debug, PartialEq)]
pub struct ModuleManifest {
    pub id: ModuleId,
    pub name: String,
    pub version: Version,
    pub kind: ModuleKind,
    /// Requirement on [`PLATFORM_API_VERSION`].
    pub requires_api: VersionReq,
    pub dependencies: Vec<Dependency>,
    pub extensions: Vec<Extension>,
}

impl ModuleManifest {
    /// Manifest of a bundled module built together with this platform version.
    ///
    /// # Panics
    /// Panics on an invalid id literal.
    #[track_caller]
    pub fn bundled(id: &'static str, name: &str, version: Version) -> Self {
        Self {
            id: ModuleId::literal(id),
            name: name.to_owned(),
            version,
            kind: ModuleKind::Bundled,
            requires_api: VersionReq::parse(&format!("={PLATFORM_API_VERSION}"))
                .expect("exact requirement on a valid version parses"),
            dependencies: Vec::new(),
            extensions: Vec::new(),
        }
    }

    /// Adds `dependency` to the manifest.
    pub fn depends_on(mut self, dependency: Dependency) -> Self {
        self.dependencies.push(dependency);
        self
    }
}

/// A unit of functionality that contributes through [`ActivationContext`].
pub trait Module {
    /// Identity, version, dependencies and extensions. Called once, before
    /// any module is activated, and must not depend on other modules.
    fn manifest(&self) -> ModuleManifest;

    /// Registers this module's contributions.
    ///
    /// Activation is transactional: when this returns an error nothing the
    /// module registered becomes visible, and modules depending on it are
    /// rejected.
    fn activate(&self, cx: &mut ActivationContext<'_>) -> Result<(), ActivationError>;
}

/// A module's own reason for failing to activate.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ActivationError(pub String);

impl ActivationError {
    /// An activation failure described by `message`.
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

/// Why a module is not part of the running application.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RejectReason {
    #[error("another module with id `{0}` was registered first")]
    DuplicateId(ModuleId),
    #[error("requires platform API {required}, this is {actual}")]
    IncompatibleApi {
        required: VersionReq,
        actual: Version,
    },
    #[error("depends on `{0}`, which is not present")]
    MissingDependency(ModuleId),
    #[error("depends on `{id}` {required}, but version {found} is present")]
    DependencyVersionMismatch {
        id: ModuleId,
        required: VersionReq,
        found: Version,
    },
    #[error("depends on `{0}`, which was rejected or failed")]
    DependencyUnavailable(ModuleId),
    #[error("dependency cycle: {}", format_cycle(.0))]
    DependencyCycle(Vec<ModuleId>),
    #[error("activation failed: {0}")]
    ActivationFailed(String),
}

fn format_cycle(cycle: &[ModuleId]) -> String {
    cycle
        .iter()
        .map(ModuleId::as_str)
        .collect::<Vec<_>>()
        .join(" -> ")
}

/// Whether a module is running, and if not, why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModuleState {
    Active,
    Rejected(RejectReason),
}

/// What happened to one registered module. Rejected modules stay listed so the
/// user can see why a feature is missing.
#[derive(Clone, Debug, PartialEq)]
pub struct ModuleReport {
    pub manifest: ModuleManifest,
    pub state: ModuleState,
}

impl ModuleReport {
    /// Whether the module activated successfully.
    pub fn is_active(&self) -> bool {
        self.state == ModuleState::Active
    }
}

/// Service listing every registered module and what became of it.
#[derive(Default)]
pub struct ModuleDirectory {
    reports: RefCell<Vec<ModuleReport>>,
}

impl ModuleDirectory {
    /// All modules in registration order, rejected ones included.
    pub fn reports(&self) -> Vec<ModuleReport> {
        self.reports.borrow().clone()
    }

    pub(crate) fn set(&self, reports: Vec<ModuleReport>) {
        *self.reports.borrow_mut() = reports;
    }
}
