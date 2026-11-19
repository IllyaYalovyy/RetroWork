//! Commands: named, module-owned actions that menus, toolbars and key
//! bindings refer to by id.

use std::rc::Rc;

use serde_json::Value;

use crate::{
    id::{CommandId, ModuleId},
    services::ServiceAccess,
    shell::Shell,
};

/// A registered command, without its handler.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandSpec {
    pub id: CommandId,
    pub title: String,
    pub owner: ModuleId,
}

/// What a command handler or view event handler can reach.
pub struct CommandContext<'a> {
    pub shell: &'a mut Shell,
    pub services: ServiceAccess<'a>,
    /// Arguments supplied by the menu item, key binding or caller.
    pub args: Option<&'a Value>,
}

impl CommandContext<'_> {
    /// Reads a required string argument.
    pub fn string_arg(&self, name: &str) -> Result<&str, CommandError> {
        self.args
            .and_then(|args| args.get(name))
            .and_then(Value::as_str)
            .ok_or_else(|| CommandError::InvalidArguments(format!("missing string `{name}`")))
    }
}

/// The code that runs when a command is executed.
pub type CommandHandler = Rc<dyn Fn(&mut CommandContext<'_>) -> Result<(), CommandError>>;

/// Why a command, view event or document could not do what was asked.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CommandError {
    #[error("unknown command `{0}`")]
    UnknownCommand(CommandId),
    #[error("invalid arguments: {0}")]
    InvalidArguments(String),
    #[error("{0}")]
    Failed(String),
    #[error("commands kept scheduling further commands; stopped after {0}")]
    RunawayChain(usize),
}

impl CommandError {
    /// A failure described by `message`, shown to the user as is.
    pub fn failed(message: impl Into<String>) -> Self {
        Self::Failed(message.into())
    }
}

/// Checks whether an action is available without changing application state.
/// Return a user-facing reason when it cannot run. Called while rendering controls.
pub type CommandAvailability =
    Rc<dyn Fn(&Shell, &ServiceAccess<'_>, Option<&Value>) -> Result<(), String>>;

/// One command registration, including its execution and availability policy.
pub struct CommandDefinition {
    pub(crate) id: CommandId,
    pub(crate) title: String,
    pub(crate) handler: CommandHandler,
    pub(crate) availability: Option<CommandAvailability>,
}
impl CommandDefinition {
    /// Defines a command that is available unless a predicate is added.
    pub fn new(
        id: CommandId,
        title: impl Into<String>,
        handler: impl Fn(&mut CommandContext<'_>) -> Result<(), CommandError> + 'static,
    ) -> Self {
        Self {
            id,
            title: title.into(),
            handler: Rc::new(handler),
            availability: None,
        }
    }
    /// Adds a cheap, side-effect-free availability check.
    pub fn available_when(
        mut self,
        check: impl Fn(&Shell, &ServiceAccess<'_>, Option<&Value>) -> Result<(), String> + 'static,
    ) -> Self {
        self.availability = Some(Rc::new(check));
        self
    }
}

/// A command and all policies committed with it during activation.
pub(crate) struct RegisteredCommand {
    pub(crate) spec: CommandSpec,
    pub(crate) handler: CommandHandler,
    pub(crate) availability: Option<CommandAvailability>,
}
