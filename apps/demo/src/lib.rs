//! Composition of the demo application.
//!
//! Everything here is toolkit-independent: which modules are bundled, where
//! packages and state live, and how the command line is read. `main.rs` adds
//! the window.

mod custom_view;

use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

use retrowork_cargo::CargoModule;
use retrowork_log_stats::LogStatsModule;
use retrowork_logs::LogsModule;
use retrowork_platform::{Platform, StateStore, Tone, load_packages};
use retrowork_projects::ProjectsModule;

/// Help text printed for `--help` and after a command-line error.
pub const USAGE: &str = "\
RetroWork: a native project workbench built on the RetroWork platform.

Usage: retrowork-demo [OPTIONS]

Options:
  --samples                  Enable the optional log and native-view demonstrations
  --packages <DIR>            Directory of packages to load
                              [default: $RETROWORK_PACKAGES, otherwise none]
  --state-dir <DIR>           Directory for persisted state
                              [default: $XDG_STATE_HOME/retrowork-demo]
  --no-state                  Do not restore or save state
  --window-size <W>x<H>       Initial window size in logical pixels [default: 1200x780]
  --automation-socket <PATH>  Serve the UI automation channel on this Unix socket.
                              For tests: lets a local process observe the window
                              and inject input.
  --exit-report <PATH>        On normal exit, write the event trace and the list of
                              contributed controls to this JSON file. For tests.
  -h, --help                  Print this help
";

/// How the demo was asked to start, from the command line and environment.
#[derive(Clone, Debug, PartialEq)]
pub struct Options {
    pub samples: bool,
    pub packages: Option<PathBuf>,
    pub state_dir: Option<PathBuf>,
    pub window_size: (f32, f32),
    pub automation_socket: Option<PathBuf>,
    pub exit_report: Option<PathBuf>,
    pub help: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            samples: false,
            packages: None,
            state_dir: None,
            window_size: (1200., 780.),
            automation_socket: None,
            exit_report: None,
            help: false,
        }
    }
}

/// Where things are when the command line does not say.
#[derive(Clone, Debug, Default)]
pub struct Environment {
    pub packages: Option<OsString>,
    pub xdg_state_home: Option<OsString>,
    pub home: Option<OsString>,
}

impl Environment {
    /// Reads the variables of the running process.
    pub fn from_process() -> Self {
        Self {
            packages: std::env::var_os("RETROWORK_PACKAGES"),
            xdg_state_home: std::env::var_os("XDG_STATE_HOME"),
            home: std::env::var_os("HOME"),
        }
    }
}

impl Options {
    /// Parses command-line arguments (without the program name).
    pub fn parse(
        args: impl IntoIterator<Item = String>,
        environment: &Environment,
    ) -> Result<Self, String> {
        let mut options = Options {
            packages: environment
                .packages
                .as_ref()
                .filter(|value| !value.is_empty())
                .map(PathBuf::from),
            state_dir: environment
                .xdg_state_home
                .as_ref()
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
                .or_else(|| {
                    environment
                        .home
                        .as_ref()
                        .filter(|value| !value.is_empty())
                        .map(|home| Path::new(home).join(".local/state"))
                })
                .map(|base| base.join("retrowork-demo")),
            ..Options::default()
        };
        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            let mut value =
                |name: &str| args.next().ok_or_else(|| format!("`{name}` needs a value"));
            match arg.as_str() {
                "-h" | "--help" => options.help = true,
                "--samples" => options.samples = true,
                "--packages" => options.packages = Some(value("--packages")?.into()),
                "--state-dir" => options.state_dir = Some(value("--state-dir")?.into()),
                "--no-state" => options.state_dir = None,
                "--automation-socket" => {
                    options.automation_socket = Some(value("--automation-socket")?.into());
                }
                "--exit-report" => options.exit_report = Some(value("--exit-report")?.into()),
                "--window-size" => {
                    let text = value("--window-size")?;
                    options.window_size = text
                        .split_once('x')
                        .and_then(|(width, height)| {
                            Some((width.parse::<f32>().ok()?, height.parse::<f32>().ok()?))
                        })
                        .filter(|(width, height)| {
                            (200.0..=10_000.0).contains(width) && (200.0..=10_000.0).contains(height)
                        })
                        .ok_or_else(|| {
                            format!("`--window-size {text}` is not <width>x<height> between 200 and 10000")
                        })?;
                }
                other => return Err(format!("unknown argument `{other}`")),
            }
        }
        Ok(options)
    }
}

/// Builds the platform of the demo: bundled modules, then packages from
/// disk, then persisted state.
pub fn build_platform(options: &Options) -> Platform {
    let mut builder = Platform::builder()
        .module(retrowork_workbench::views::WorkbenchModule)
        .module(ProjectsModule)
        .module(CargoModule)
        .module(retrowork_ninja::NinjaModule);
    if options.samples {
        builder = builder
            .module(LogsModule)
            .module(LogStatsModule)
            .module(custom_view::CustomViewModule);
    }
    let mut problems = Vec::new();
    if let Some(directory) = &options.packages {
        let (packages, errors) = load_packages(directory);
        problems.extend(errors.into_iter().map(|error| error.to_string()));
        for package in packages {
            builder = builder.module(package);
        }
    }
    if let Some(directory) = &options.state_dir {
        builder = builder.state_store(StateStore::new(directory.join("state.json")));
    }
    let platform = builder.build();
    for problem in problems {
        platform
            .shell()
            .output()
            .append(format!("Package was not loaded: {problem}"), Tone::Warning);
    }
    platform
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str], environment: &Environment) -> Result<Options, String> {
        Options::parse(args.iter().map(|arg| (*arg).to_owned()), environment)
    }

    #[test]
    fn ordinary_startup_loads_no_sample_plugins() {
        let empty = tempfile::tempdir().unwrap();
        let platform = build_platform(&Options {
            packages: Some(empty.path().into()),
            ..Options::default()
        });
        assert!(platform.modules().iter().all(|report| report.is_active()));
        assert!(
            platform.modules().iter().all(|report| !report
                .manifest
                .id
                .as_str()
                .starts_with("sample.")
                && report.manifest.id.as_str() != "retrowork.logs")
        );
        assert!(
            platform
                .services()
                .get::<retrowork_projects::ProjectWorkspace>()
                .is_some()
        );
    }

    #[test]
    fn defaults_come_from_the_environment() {
        let options = parse(&[], &Environment::default()).unwrap();
        assert_eq!(options.packages, None);
        assert_eq!(options.state_dir, None, "no HOME, nowhere to save");
        assert_eq!(options.window_size, (1200., 780.));

        let environment = Environment {
            packages: Some("/opt/pkgs".into()),
            xdg_state_home: None,
            home: Some("/home/u".into()),
        };
        let options = parse(&[], &environment).unwrap();
        assert_eq!(options.packages, Some("/opt/pkgs".into()));
        assert_eq!(
            options.state_dir,
            Some("/home/u/.local/state/retrowork-demo".into())
        );

        let environment = Environment {
            xdg_state_home: Some("/state".into()),
            ..environment
        };
        assert_eq!(
            parse(&[], &environment).unwrap().state_dir,
            Some("/state/retrowork-demo".into())
        );
    }

    #[test]
    fn arguments_override_the_environment() {
        let environment = Environment {
            packages: Some("/opt/pkgs".into()),
            xdg_state_home: Some("/state".into()),
            home: None,
        };
        let options = parse(
            &[
                "--packages",
                "p",
                "--state-dir",
                "s",
                "--window-size",
                "900x600",
                "--automation-socket",
                "/tmp/a.sock",
                "--exit-report",
                "/tmp/exit.json",
            ],
            &environment,
        )
        .unwrap();
        assert_eq!(
            options,
            Options {
                samples: false,
                packages: Some("p".into()),
                state_dir: Some("s".into()),
                window_size: (900., 600.),
                automation_socket: Some("/tmp/a.sock".into()),
                exit_report: Some("/tmp/exit.json".into()),
                help: false,
            }
        );
        assert_eq!(
            parse(&["--no-state"], &environment).unwrap().state_dir,
            None
        );
        assert!(parse(&["-h"], &environment).unwrap().help);
    }

    #[test]
    fn bad_arguments_are_rejected_with_a_reason() {
        let environment = Environment::default();
        for (args, expected) in [
            (&["--frobnicate"][..], "unknown argument `--frobnicate`"),
            (&["--packages"][..], "`--packages` needs a value"),
            (&["--window-size", "big"][..], "is not <width>x<height>"),
            (&["--window-size", "10x10"][..], "between 200 and 10000"),
            (&["--window-size", "800xNaN"][..], "is not <width>x<height>"),
        ] {
            let error = parse(args, &environment).unwrap_err();
            assert!(error.contains(expected), "{args:?}: {error}");
        }
    }
}
