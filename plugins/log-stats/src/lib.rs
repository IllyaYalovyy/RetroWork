//! Log Statistics: a bundled module built only on the public APIs of the
//! platform and of the Log Explorer module.
//!
//! It contributes the **Statistics** view (bottom dock) and the
//! **Summarize Levels** command (Tools menu, toolbar, `Ctrl+Shift+M`), which
//! writes a summary of the records currently shown in the active log to the
//! Output view.

use std::{collections::BTreeMap, rc::Rc};

use retrowork_logs::{FilteredRecords, LogWorkspace, record::Level};
use retrowork_platform::{
    ActivationContext, ActivationError, CommandError, CommandId, Dependency, MenuId, Module,
    ModuleManifest, PropertyModel, Shell, Tone, ViewContent, ViewId, ViewLocation, ViewProvider,
    core::{MENU_TOOLS, VIEW_OUTPUT},
};
use semver::Version;

/// Module id of Log Statistics, which depends on the Log Explorer.
pub const MODULE_ID: &str = "retrowork.log-stats";
/// Id of the Statistics view in the bottom dock.
pub const VIEW_STATISTICS: &str = "logstats.statistics";
/// Writes a level and component summary of the shown records to the Output.
pub const COMMAND_SUMMARIZE: &str = "logstats.summarize";

/// Counts over a set of records.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Summary {
    pub shown: usize,
    pub total: usize,
    /// Count per level, most severe first, levels with no records omitted.
    pub levels: Vec<(Level, usize)>,
    /// Up to three busiest components, by count then name.
    pub top_components: Vec<(String, usize)>,
}

impl Summary {
    /// Counts the records that pass the log's filter.
    pub fn of(records: &FilteredRecords) -> Self {
        let mut levels: BTreeMap<Level, usize> = BTreeMap::new();
        let mut components: BTreeMap<&str, usize> = BTreeMap::new();
        for record in records.iter() {
            *levels.entry(record.level).or_default() += 1;
            if !record.component.is_empty() {
                *components.entry(&record.component).or_default() += 1;
            }
        }
        let mut top_components: Vec<(String, usize)> = components
            .into_iter()
            .map(|(name, count)| (name.to_owned(), count))
            .collect();
        top_components.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        top_components.truncate(3);
        Self {
            shown: records.len(),
            total: records.total(),
            levels: Level::ALL
                .into_iter()
                .filter_map(|level| levels.get(&level).map(|&count| (level, count)))
                .collect(),
            top_components,
        }
    }

    /// One line such as `ERROR 3, WARN 2, INFO 7`.
    pub fn levels_line(&self) -> String {
        if self.levels.is_empty() {
            return "no records".to_owned();
        }
        self.levels
            .iter()
            .map(|(level, count)| format!("{level} {count}"))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

struct StatisticsView(Rc<LogWorkspace>);

impl ViewProvider for StatisticsView {
    fn content(&self, shell: &Shell) -> ViewContent {
        let rows = self
            .0
            .active_document(shell)
            .map(|document| {
                let summary = Summary::of(&document.filtered());
                let mut rows = vec![
                    ("Log".to_owned(), document.source().title.clone()),
                    (
                        "Shown".to_owned(),
                        format!("{} of {}", summary.shown, summary.total),
                    ),
                ];
                rows.extend(
                    summary
                        .levels
                        .iter()
                        .map(|(level, count)| (level.to_string(), count.to_string())),
                );
                rows.extend(summary.top_components.iter().enumerate().map(
                    |(rank, (name, count))| {
                        (format!("Top {}", rank + 1), format!("{name} ({count})"))
                    },
                ));
                rows
            })
            .unwrap_or_default();
        ViewContent::Properties(PropertyModel {
            rows,
            empty_hint: "Open a log to see its statistics.".to_owned(),
        })
    }
}

/// Registers the Statistics view and the Summarize Levels command. Rejected
/// when the Log Explorer is not present.
#[derive(Default)]
pub struct LogStatsModule;

impl Module for LogStatsModule {
    fn manifest(&self) -> ModuleManifest {
        ModuleManifest::bundled(
            MODULE_ID,
            "Log Statistics",
            Version::parse(env!("CARGO_PKG_VERSION")).expect("crate version is semver"),
        )
        .depends_on(Dependency::literal(retrowork_logs::MODULE_ID, "^0.1"))
    }

    fn activate(&self, cx: &mut ActivationContext<'_>) -> Result<(), ActivationError> {
        let workspace = cx.require_service::<LogWorkspace>()?;

        cx.view(
            ViewId::literal(VIEW_STATISTICS),
            "Statistics",
            ViewLocation::Bottom,
            20,
            Rc::new(StatisticsView(Rc::clone(&workspace))),
        )?;

        cx.command(
            CommandId::literal(COMMAND_SUMMARIZE),
            "Summarize Levels",
            move |cx| {
                let document = workspace.active_document(cx.shell).ok_or_else(|| {
                    CommandError::failed("no log is open; select one in Sources first")
                })?;
                let summary = Summary::of(&document.filtered());
                let title = &document.source().title;
                let output = cx.shell.output();
                output.append(
                    format!(
                        "Summary of {title}: {} of {} records shown",
                        summary.shown, summary.total
                    ),
                    Tone::Info,
                );
                output.append(format!("  Levels: {}", summary.levels_line()), Tone::Normal);
                for (name, count) in &summary.top_components {
                    output.append(format!("  {name}: {count}"), Tone::Normal);
                }
                cx.shell.show_view(ViewId::literal(VIEW_OUTPUT));
                cx.shell
                    .set_status(format!("Summarized {} records of {title}", summary.shown));
                Ok(())
            },
        )?;
        cx.menu_item(
            MenuId::literal(MENU_TOOLS),
            "summarize-levels",
            "Summarize Levels",
            CommandId::literal(COMMAND_SUMMARIZE),
            None,
            10,
        )?;
        cx.toolbar_item(
            "logstats.summarize",
            "Summarize",
            CommandId::literal(COMMAND_SUMMARIZE),
            None,
            30,
        )?;
        cx.key_binding("ctrl-shift-m", CommandId::literal(COMMAND_SUMMARIZE), None)?;
        Ok(())
    }
}
