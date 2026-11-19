//! Build-tool registrations and their project commands.

use super::*;

/// Service: providers registered by build-tool plugins.
pub struct ProjectRegistry {
    providers: retrowork_platform::Registry<Arc<dyn ProjectProvider>>,
}
impl Default for ProjectRegistry {
    fn default() -> Self {
        Self {
            providers: retrowork_platform::Registry::new(ModuleId::literal(MODULE_ID)),
        }
    }
}

impl ProjectRegistry {
    /// Stages a provider for a dependent module. Rejected activations leave
    /// the registry unchanged; duplicate provider ids reject the contributor.
    pub fn register(
        self: &Rc<Self>,
        cx: &mut ActivationContext<'_>,
        provider: Arc<dyn ProjectProvider>,
    ) -> Result<(), ActivationError> {
        if provider.id() == "projects.files" {
            return Err(ActivationError::new(
                "projects.files is reserved for projects without a build tool",
            ));
        }
        CommandId::new(provider.id()).map_err(|error| ActivationError::new(error.to_string()))?;
        let mut ids = BTreeSet::new();
        for definition in provider.actions() {
            let action = ActionId::new(definition.id)
                .map_err(|error| ActivationError::new(error.to_string()))?;
            if !ids.insert(action.clone()) || definition.title.is_empty() {
                return Err(ActivationError::new(
                    "duplicate action or empty action title",
                ));
            }
            if !STANDARD_ACTIONS
                .iter()
                .any(|standard| standard.id == definition.id)
            {
                if !definition.id.starts_with(&format!("{}.", provider.id())) {
                    return Err(ActivationError::new(
                        "custom action ID must belong to its provider",
                    ));
                }
                let workspace = cx.require_service::<ProjectWorkspace>()?;
                let available = action.clone();
                let command = CommandId::new(&format!("projects.action.{}", definition.id))
                    .map_err(|error| ActivationError::new(error.to_string()))?;
                cx.register_command(
                    CommandDefinition::new(command.clone(), definition.title, move |cx| {
                        workspace.start(&action, cx)
                    })
                    .available_when(move |_, services, _| {
                        services
                            .get::<ProjectWorkspace>()
                            .ok_or("project service unavailable")?
                            .action_available(&available)
                    }),
                )?;
                cx.menu_item(
                    MenuId::literal(MENU_RUN),
                    definition.id,
                    definition.title,
                    command,
                    None,
                    100,
                )?;
            }
        }
        self.providers.register(cx, provider.id(), provider)
    }

    pub(super) fn providers(&self) -> Vec<Arc<dyn ProjectProvider>> {
        self.providers.values()
    }
}
