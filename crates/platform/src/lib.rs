//! RetroWork platform kernel.
//!
//! This crate has no UI toolkit dependency. It defines:
//!
//! - **Identity**: validated ids for modules, commands, views, menus and
//!   document kinds ([`id`]).
//! - **Modules**: manifests, dependency validation and transactional
//!   activation ([`module`], [`resolve`], [`activation`]).
//! - **Contributions**: commands, menus, toolbar items, key bindings, docked
//!   views, document factories and extension points ([`contribution`]).
//! - **View models**: tree, table, text and property content with user events
//!   ([`model`]).
//! - **Shell state**: open documents, active views, status, output, event
//!   trace ([`shell`]).
//! - **Persistence**: crash-safe state storage ([`state`]).
//! - **Packages**: declarative and process packages loaded from disk
//!   ([`package`], [`external`]).
//!
//! # Extension points for module authors
//!
//! | To add | Use |
//! |---|---|
//! | An action | [`ActivationContext::command`] |
//! | A menu, menu item, toolbar button, shortcut | [`ActivationContext::menu`], [`ActivationContext::menu_item`], [`ActivationContext::toolbar_item`], [`ActivationContext::key_binding`] |
//! | A docked panel | [`ActivationContext::view`] with a [`ViewProvider`] |
//! | A document type for the centre area | [`ActivationContext::document_factory`] |
//! | An API for other modules | [`ActivationContext::provide`] / [`ActivationContext::require_service`] |
//! | A data hook other modules and packages can fill | [`ActivationContext::extension_point`] |
//! | Background work | [`Inbox::post_blocking`] from the worker thread |
//!
//! A front end drives the platform through [`Platform::execute`],
//! [`Platform::view_event`] and [`Platform::pump`], and draws what
//! [`Platform::contributions`] and [`Platform::shell`] describe.

pub mod activation;
pub mod command;
pub mod contribution;
pub mod core;
pub mod document_file;
pub mod document_session;
pub mod external;
pub mod id;
pub mod model;
pub mod module;
mod native;
pub mod package;
mod persistence;
pub use native::{NativeModel, NativeProvider, NativeValue};
mod platform;
mod registry;
pub mod resolve;
pub use registry::Registry;
pub mod services;
pub mod shell;
pub mod state;
pub mod task;

pub use activation::{ActivationContext, ContributedExtension, ExtensionCommit};
pub use command::{CommandContext, CommandDefinition, CommandError, CommandSpec};
pub use contribution::{
    Contributions, DocumentFactory, KeyBinding, KeyChord, MenuItem, MenuSpec, OpenedDocument,
    ToolbarItem, ViewLocation, ViewSpec,
};
pub use id::{CommandId, DocumentKind, IdError, MenuId, ModuleId, ViewId};
pub use model::{
    CloseError, FilterModel, InputModel, PropertyModel, SourceModel, TableColumn, TableModel,
    TableRow, TextLine, TextModel, Tone, TreeModel, TreeNode, ViewContent, ViewEvent, ViewProvider,
};
pub use module::{
    ActivationError, Dependency, Extension, Module, ModuleDirectory, ModuleKind, ModuleManifest,
    ModuleReport, ModuleState, PLATFORM_API_VERSION, RejectReason,
};
pub use package::{Package, PackageError, load_packages};
pub use platform::{Platform, PlatformBuilder, ViewTarget};
pub use services::{
    Completion, INBOX_CAPACITY, Inbox, ModuleResource, PostError, RejectedCompletion,
    ServiceAccess, Services,
};
pub use shell::{
    Document, DocumentId, FocusTarget, FolderRequest, OutputLog, PathRequest, ResourceChange,
    SavePathRequest, Shell, Trace, TraceEvent,
};
pub use state::{LoadOutcome, PersistedState, StateStore};

pub use persistence::Persistence;
