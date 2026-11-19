//! The core module: menus, the Output view and the commands every
//! application built on the platform has.

use std::rc::Rc;

use semver::Version;

use crate::{
    activation::ActivationContext,
    command::CommandError,
    contribution::ViewLocation,
    id::{CommandId, MenuId, ViewId},
    model::{TextModel, Tone, ViewContent, ViewProvider},
    module::{ActivationError, Module, ModuleDirectory, ModuleManifest, ModuleState},
    shell::{OutputLog, Shell},
};

/// Id of the core module. Its commands may be referenced by any module
/// without declaring a dependency.
pub const CORE_MODULE_ID: &str = "retrowork.core";

/// Id of the File menu.
pub const MENU_FILE: &str = "file";
/// Id of the View menu.
pub const MENU_VIEW: &str = "view";
/// Id of the Tools menu.
pub const MENU_TOOLS: &str = "tools";
/// Id of the Help menu.
pub const MENU_HELP: &str = "help";

/// Id of the Output view in the bottom dock.
pub const VIEW_OUTPUT: &str = "output";

/// Asks the front end to quit (File > Exit, Ctrl+Q).
pub const COMMAND_EXIT: &str = "workbench.exit";
/// Closes the active document (File > Close, Ctrl+W).
pub const COMMAND_CLOSE_DOCUMENT: &str = "workbench.close-document";
/// Brings the Output view forward (View > Output, Ctrl+4).
pub const COMMAND_SHOW_OUTPUT: &str = "output.show";
/// Empties the Output view (View > Clear Output, Ctrl+L).
pub const COMMAND_CLEAR_OUTPUT: &str = "output.clear";
/// Writes every module and its state to the Output (Help > List Modules).
pub const COMMAND_LIST_MODULES: &str = "workbench.list-modules";

/// The module every platform starts with. It shares the shell's
/// [`OutputLog`] so that its Output view shows what handlers append.
pub(crate) struct CoreModule {
    pub(crate) output: Rc<OutputLog>,
}

struct OutputView(Rc<OutputLog>);

impl ViewProvider for OutputView {
    fn content(&self, _shell: &Shell) -> ViewContent {
        ViewContent::Text(TextModel {
            lines: self.0.lines(),
            empty_hint: "No output yet.".to_owned(),
            ..TextModel::default()
        })
    }
}

impl Module for CoreModule {
    fn manifest(&self) -> ModuleManifest {
        ModuleManifest::bundled(
            CORE_MODULE_ID,
            "RetroWork Core",
            Version::parse(env!("CARGO_PKG_VERSION")).expect("crate version is semver"),
        )
    }

    fn activate(&self, cx: &mut ActivationContext<'_>) -> Result<(), ActivationError> {
        cx.menu(MenuId::literal(MENU_FILE), "File", 0)?;
        cx.menu(MenuId::literal(MENU_VIEW), "View", 100)?;
        cx.menu(MenuId::literal(MENU_TOOLS), "Tools", 200)?;
        cx.menu(MenuId::literal(MENU_HELP), "Help", 900)?;

        cx.view(
            ViewId::literal(VIEW_OUTPUT),
            "Output",
            ViewLocation::Bottom,
            0,
            Rc::new(OutputView(Rc::clone(&self.output))),
        )?;

        cx.command(CommandId::literal(COMMAND_EXIT), "Exit", |cx| {
            cx.shell.request_quit();
            Ok(())
        })?;
        cx.menu_item(
            MenuId::literal(MENU_FILE),
            "exit",
            "Exit",
            CommandId::literal(COMMAND_EXIT),
            None,
            1000,
        )?;
        cx.key_binding("ctrl-q", CommandId::literal(COMMAND_EXIT), None)?;

        cx.command(
            CommandId::literal(COMMAND_CLOSE_DOCUMENT),
            "Close Document",
            |cx| {
                match cx.shell.active_document_index() {
                    Some(index) => {
                        let title = cx.shell.documents()[index].title.clone();
                        if cx.shell.close_document(index) {
                            cx.shell.set_status(format!("Closed {title}"));
                        }
                    }
                    None => cx.shell.set_status("No document to close"),
                }
                Ok(())
            },
        )?;
        cx.menu_item(
            MenuId::literal(MENU_FILE),
            "close-document",
            "Close",
            CommandId::literal(COMMAND_CLOSE_DOCUMENT),
            None,
            100,
        )?;
        cx.key_binding("ctrl-w", CommandId::literal(COMMAND_CLOSE_DOCUMENT), None)?;

        cx.command(CommandId::literal(COMMAND_SHOW_OUTPUT), "Output", |cx| {
            cx.shell.show_view(ViewId::literal(VIEW_OUTPUT));
            Ok(())
        })?;
        cx.menu_item(
            MenuId::literal(MENU_VIEW),
            "output",
            "Output",
            CommandId::literal(COMMAND_SHOW_OUTPUT),
            None,
            900,
        )?;
        cx.key_binding("ctrl-4", CommandId::literal(COMMAND_SHOW_OUTPUT), None)?;

        cx.command(
            CommandId::literal(COMMAND_CLEAR_OUTPUT),
            "Clear Output",
            |cx| {
                cx.shell.output().clear();
                cx.shell.set_status("Output cleared");
                Ok(())
            },
        )?;
        cx.menu_item(
            MenuId::literal(MENU_VIEW),
            "clear-output",
            "Clear Output",
            CommandId::literal(COMMAND_CLEAR_OUTPUT),
            None,
            910,
        )?;
        cx.key_binding("ctrl-l", CommandId::literal(COMMAND_CLEAR_OUTPUT), None)?;

        cx.command(
            CommandId::literal(COMMAND_LIST_MODULES),
            "List Modules",
            |cx| {
                let directory = cx
                    .services
                    .get::<ModuleDirectory>()
                    .ok_or_else(|| CommandError::failed("module directory is not available"))?;
                let output = cx.shell.output();
                let reports = directory.reports();
                output.append(format!("Modules ({}):", reports.len()), Tone::Info);
                for report in &reports {
                    let manifest = &report.manifest;
                    let (state, tone) = match &report.state {
                        ModuleState::Active => ("active".to_owned(), Tone::Normal),
                        ModuleState::Rejected(reason) => {
                            (format!("REJECTED: {reason}"), Tone::Warning)
                        }
                    };
                    output.append(
                        format!(
                            "  {} {} [{}] {state}",
                            manifest.id, manifest.version, manifest.kind
                        ),
                        tone,
                    );
                }
                cx.shell.show_view(ViewId::literal(VIEW_OUTPUT));
                cx.shell
                    .set_status(format!("Listed {} modules", reports.len()));
                Ok(())
            },
        )?;
        cx.menu_item(
            MenuId::literal(MENU_HELP),
            "list-modules",
            "List Modules",
            CommandId::literal(COMMAND_LIST_MODULES),
            None,
            0,
        )?;
        Ok(())
    }
}
