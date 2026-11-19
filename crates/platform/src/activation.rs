//! The API a module contributes through.
//!
//! Registrations are staged and committed only when the module's `activate`
//! returns `Ok` and the staged set is consistent, so a failing module leaves
//! no partial contributions behind.

use std::{
    any::{Any, TypeId},
    collections::BTreeSet,
    path::Path,
    rc::Rc,
};

use serde_json::Value;

use crate::{
    command::{CommandContext, CommandError, CommandSpec},
    contribution::{
        Contributions, DocumentFactory, KeyBinding, KeyChord, MenuItem, MenuSpec, ToolbarItem,
        ViewLocation, ViewSpec,
    },
    core::CORE_MODULE_ID,
    id::{CommandId, DocumentKind, MenuId, ModuleId, ViewId},
    model::ViewProvider,
    module::{ActivationError, ModuleManifest},
    services::{Inbox, ModuleResource, Services},
};

/// An extension contributed to a point, with its contributor.
pub struct ContributedExtension<'a> {
    pub owner: &'a ModuleId,
    pub base_dir: Option<&'a Path>,
    pub data: &'a Value,
}

/// Applies an accepted extension. Runs only if the contributing module's
/// whole activation succeeds.
pub type ExtensionCommit = Box<dyn FnOnce()>;

/// Validates one extension and returns the action that applies it.
///
/// Validation and application are separate so that a package is loaded
/// completely or not at all: the platform first asks every point to accept
/// every extension of a module, and commits only when all were accepted.
pub type ExtensionPointHandler =
    Rc<dyn Fn(&ContributedExtension<'_>) -> Result<ExtensionCommit, ActivationError>>;

/// What a module registers its contributions through while it activates.
///
/// Everything registered is staged. It becomes visible only if the module's
/// `activate` succeeds and every reference it made can be resolved; a
/// failing module leaves nothing behind.
pub struct ActivationContext<'a> {
    manifest: &'a ModuleManifest,
    existing: &'a Contributions,
    services: &'a Services,
    pub(crate) staged: Contributions,
    pub(crate) staged_services: Vec<(TypeId, Rc<dyn Any>)>,
    pub(crate) staged_registrations: Vec<Box<dyn FnOnce() -> Rc<dyn ModuleResource>>>,
    pub(crate) staged_resources: Vec<Rc<dyn ModuleResource>>,
    registration_keys: BTreeSet<(ModuleId, String)>,
}

impl<'a> ActivationContext<'a> {
    pub(crate) fn new(
        manifest: &'a ModuleManifest,
        existing: &'a Contributions,
        services: &'a Services,
    ) -> Self {
        Self {
            manifest,
            existing,
            services,
            staged: Contributions::default(),
            staged_services: Vec::new(),
            staged_registrations: Vec::new(),
            staged_resources: Vec::new(),
            registration_keys: BTreeSet::new(),
        }
    }

    /// Id of the module being activated.
    pub fn module(&self) -> &ModuleId {
        &self.manifest.id
    }

    /// A service registered by an already active module.
    pub fn service<T: 'static>(&self) -> Result<Option<Rc<T>>, ActivationError> {
        if let Some(owner) = self.services.owner::<T>() {
            self.check_owner(owner)?;
        }
        Ok(self.services.get::<T>())
    }

    /// A service this module cannot work without.
    pub fn require_service<T: 'static>(&self) -> Result<Rc<T>, ActivationError> {
        self.service::<T>()?.ok_or_else(|| {
            ActivationError::new(format!(
                "required service `{}` is not registered",
                std::any::type_name::<T>()
            ))
        })
    }

    /// Offers `service` to modules activated later.
    pub fn provide<T: 'static>(&mut self, service: Rc<T>) -> Result<(), ActivationError> {
        let type_id = TypeId::of::<T>();
        if self.services.contains(type_id)
            || self
                .staged_services
                .iter()
                .any(|(staged, _)| *staged == type_id)
        {
            return Err(ActivationError::new(format!(
                "service `{}` is already registered",
                std::any::type_name::<T>()
            )));
        }
        self.staged_services.push((type_id, service));
        Ok(())
    }

    /// Owns a resource and stops it if activation fails or the platform shuts down.
    pub fn manage(&mut self, resource: Rc<dyn ModuleResource>) {
        self.staged_resources.push(resource);
    }

    fn check_owner(&self, owner: &ModuleId) -> Result<(), ActivationError> {
        if owner != &self.manifest.id
            && owner.as_str() != CORE_MODULE_ID
            && !self
                .manifest
                .dependencies
                .iter()
                .any(|dependency| &dependency.id == owner)
        {
            return Err(ActivationError::new(format!(
                "service owner `{owner}` is not a declared dependency"
            )));
        }
        Ok(())
    }

    /// Stages a registry mutation and its matching disposal resource.
    pub(crate) fn stage_registration(
        &mut self,
        owner: ModuleId,
        key: &str,
        apply: impl FnOnce() -> Rc<dyn ModuleResource> + 'static,
    ) -> Result<(), ActivationError> {
        self.check_owner(&owner)?;
        if key.is_empty() || !self.registration_keys.insert((owner, key.to_owned())) {
            return Err(ActivationError::new(format!(
                "duplicate or empty registration key `{key}`"
            )));
        }
        self.staged_registrations.push(Box::new(apply));
        Ok(())
    }

    /// The queue background threads post UI-thread work to.
    pub fn inbox(&self) -> Inbox {
        self.services.access(Some(&self.manifest.id)).inbox()
    }

    /// Defines extension point `name`. Modules that declare a dependency on
    /// this one can contribute data to it from their manifest; `handler`
    /// validates each contribution and returns the action that applies it.
    pub fn extension_point(
        &mut self,
        name: &str,
        handler: impl Fn(&ContributedExtension<'_>) -> Result<ExtensionCommit, ActivationError>
        + 'static,
    ) -> Result<(), ActivationError> {
        if self.existing.extension_points.contains_key(name)
            || self.staged.extension_points.contains_key(name)
        {
            return Err(ActivationError::new(format!(
                "extension point `{name}` is already defined"
            )));
        }
        self.staged.extension_points.insert(
            name.to_owned(),
            (self.manifest.id.clone(), Rc::new(handler)),
        );
        Ok(())
    }

    /// Registers an always-available command. Fails if its id is taken.
    pub fn command(
        &mut self,
        id: CommandId,
        title: &str,
        handler: impl Fn(&mut CommandContext<'_>) -> Result<(), CommandError> + 'static,
    ) -> Result<(), ActivationError> {
        self.register_command(crate::command::CommandDefinition::new(id, title, handler))
    }

    /// Stages command metadata, availability and execution as one contribution.
    pub fn register_command(
        &mut self,
        definition: crate::command::CommandDefinition,
    ) -> Result<(), ActivationError> {
        let id = definition.id;
        if let Some(owner) = self
            .existing
            .command(&id)
            .or_else(|| self.staged.command(&id))
            .map(|spec| &spec.owner)
        {
            return Err(ActivationError::new(format!(
                "command `{id}` is already registered by `{owner}`"
            )));
        }
        let spec = CommandSpec {
            id: id.clone(),
            title: definition.title,
            owner: self.manifest.id.clone(),
        };
        self.staged.commands.insert(
            id,
            crate::command::RegisteredCommand {
                spec,
                handler: definition.handler,
                availability: definition.availability,
            },
        );
        Ok(())
    }

    /// Adds a top-level menu. Menus are shown by ascending `order`, then id.
    pub fn menu(&mut self, id: MenuId, title: &str, order: i32) -> Result<(), ActivationError> {
        if self.menu_exists(&id) {
            return Err(ActivationError::new(format!(
                "menu `{id}` is already registered"
            )));
        }
        self.staged.menus.push(MenuSpec {
            id,
            title: title.to_owned(),
            order,
            owner: self.manifest.id.clone(),
        });
        Ok(())
    }

    fn menu_exists(&self, id: &MenuId) -> bool {
        self.existing
            .menus
            .iter()
            .chain(&self.staged.menus)
            .any(|menu| &menu.id == id)
    }

    /// Adds an item that runs `command` with `args` to `menu`.
    ///
    /// `id` must be unique within the menu. The menu and the command may belong
    /// to this module, the core, or a declared dependency; that is checked when
    /// activation finishes.
    pub fn menu_item(
        &mut self,
        menu: MenuId,
        id: &str,
        title: &str,
        command: CommandId,
        args: Option<Value>,
        order: i32,
    ) -> Result<(), ActivationError> {
        validate_item_id(id)?;
        if self
            .existing
            .menu_items
            .iter()
            .chain(&self.staged.menu_items)
            .any(|item| item.menu == menu && item.id == id)
        {
            return Err(ActivationError::new(format!(
                "menu `{menu}` already has an item `{id}`"
            )));
        }
        self.staged.menu_items.push(MenuItem {
            menu,
            id: id.to_owned(),
            title: title.to_owned(),
            command,
            args,
            order,
            owner: self.manifest.id.clone(),
        });
        Ok(())
    }

    /// Adds a toolbar button that runs `command` with `args`. Buttons are shown
    /// by ascending `order`, then id.
    pub fn toolbar_item(
        &mut self,
        id: &str,
        title: &str,
        command: CommandId,
        args: Option<Value>,
        order: i32,
    ) -> Result<(), ActivationError> {
        validate_item_id(id)?;
        if self
            .existing
            .toolbar
            .iter()
            .chain(&self.staged.toolbar)
            .any(|item| item.id == id)
        {
            return Err(ActivationError::new(format!(
                "toolbar already has an item `{id}`"
            )));
        }
        self.staged.toolbar.push(ToolbarItem {
            id: id.to_owned(),
            title: title.to_owned(),
            command,
            args,
            order,
            owner: self.manifest.id.clone(),
        });
        Ok(())
    }

    /// Binds `chord` to `command`. A chord can be bound once; the module that
    /// registers it first keeps it and the later registration fails.
    pub fn key_binding(
        &mut self,
        chord: &str,
        command: CommandId,
        args: Option<Value>,
    ) -> Result<(), ActivationError> {
        let chord = KeyChord::parse(chord)
            .map_err(|error| ActivationError::new(format!("key binding `{chord}`: {error}")))?;
        if let Some(taken) = self
            .existing
            .key_bindings
            .iter()
            .chain(&self.staged.key_bindings)
            .find(|binding| binding.chord == chord)
        {
            return Err(ActivationError::new(format!(
                "key `{chord}` is already bound to `{}` by `{}`",
                taken.command, taken.owner
            )));
        }
        self.staged.key_bindings.push(KeyBinding {
            chord,
            command,
            args,
            owner: self.manifest.id.clone(),
        });
        Ok(())
    }

    /// Docks a view at `location`; `provider` supplies its content and handles
    /// its events. Fails if the id is taken.
    pub fn view(
        &mut self,
        id: ViewId,
        title: &str,
        location: ViewLocation,
        order: i32,
        provider: Rc<dyn ViewProvider>,
    ) -> Result<(), ActivationError> {
        if self.existing.view(&id).is_some() || self.staged.view(&id).is_some() {
            return Err(ActivationError::new(format!(
                "view `{id}` is already registered"
            )));
        }
        self.staged.views.push((
            ViewSpec {
                id,
                title: title.to_owned(),
                location,
                order,
                owner: self.manifest.id.clone(),
            },
            provider,
        ));
        Ok(())
    }

    /// Makes this module the one that opens documents of `kind`. Fails if
    /// another module already does.
    pub fn document_factory(
        &mut self,
        kind: DocumentKind,
        factory: Rc<dyn DocumentFactory>,
    ) -> Result<(), ActivationError> {
        if self.existing.document_factories.contains_key(&kind)
            || self.staged.document_factories.contains_key(&kind)
        {
            return Err(ActivationError::new(format!(
                "document kind `{kind}` is already registered"
            )));
        }
        self.staged
            .document_factories
            .insert(kind, (self.manifest.id.clone(), factory));
        Ok(())
    }

    /// Checks references between staged and existing contributions.
    ///
    /// A menu item, toolbar item or key binding may only refer to a command
    /// owned by this module, by the core, or by a module this one declares a
    /// dependency on. That keeps manifests honest: if the reference works,
    /// removing the other module is guaranteed to reject this one instead of
    /// leaving a dead menu entry.
    ///
    /// Also offers this module's manifest extensions to their extension
    /// points. Returns the actions that apply them; the caller runs those only
    /// when everything was accepted.
    pub(crate) fn validate(&self) -> Result<Vec<ExtensionCommit>, ActivationError> {
        let allowed_owners: BTreeSet<&ModuleId> = self
            .manifest
            .dependencies
            .iter()
            .map(|dependency| &dependency.id)
            .chain([&self.manifest.id])
            .collect();
        let check_command = |what: &str, command: &CommandId| {
            let spec = self
                .staged
                .command(command)
                .or_else(|| self.existing.command(command))
                .ok_or_else(|| {
                    ActivationError::new(format!("{what} refers to unknown command `{command}`"))
                })?;
            if allowed_owners.contains(&spec.owner) || spec.owner.as_str() == CORE_MODULE_ID {
                Ok(())
            } else {
                Err(ActivationError::new(format!(
                    "{what} refers to command `{command}` of `{}`, which is not a declared dependency",
                    spec.owner
                )))
            }
        };
        for item in &self.staged.menu_items {
            if !self.menu_exists(&item.menu) {
                return Err(ActivationError::new(format!(
                    "menu item `{}` refers to unknown menu `{}`",
                    item.id, item.menu
                )));
            }
            check_command(&format!("menu item `{}`", item.id), &item.command)?;
        }
        for item in &self.staged.toolbar {
            check_command(&format!("toolbar item `{}`", item.id), &item.command)?;
        }
        for binding in &self.staged.key_bindings {
            check_command(&format!("key `{}`", binding.chord), &binding.command)?;
        }

        let mut commits = Vec::new();
        for extension in &self.manifest.extensions {
            let point = &extension.point;
            let (owner, handler) = self
                .staged
                .extension_points
                .get(point)
                .or_else(|| self.existing.extension_points.get(point))
                .ok_or_else(|| {
                    ActivationError::new(format!("unknown extension point `{point}`"))
                })?;
            if !allowed_owners.contains(owner) && owner.as_str() != CORE_MODULE_ID {
                return Err(ActivationError::new(format!(
                    "extension point `{point}` belongs to `{owner}`, which is not a declared dependency"
                )));
            }
            let commit = handler(&ContributedExtension {
                owner: &self.manifest.id,
                base_dir: extension.base_dir.as_deref(),
                data: &extension.data,
            })
            .map_err(|error| ActivationError::new(format!("extension to `{point}`: {error}")))?;
            commits.push(commit);
        }
        Ok(commits)
    }
}

/// Item ids become part of automation selectors, so they follow the same
/// character rules as one identifier segment.
fn validate_item_id(id: &str) -> Result<(), ActivationError> {
    let valid = !id.is_empty()
        && id.chars().all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_' || c == '.'
        });
    if valid {
        Ok(())
    } else {
        Err(ActivationError::new(format!(
            "item id `{id}` must be non-empty and use only a-z, 0-9, `-`, `_`, `.`"
        )))
    }
}
