//! Standalone file finder; accepts repeated --project paths and --generated relative paths.

use std::process::ExitCode;

use gpui::{Bounds, TitlebarOptions, WindowBounds, WindowOptions, px, size};
use retrowork_demo::{Environment, Options, USAGE};
use retrowork_workbench::{automation, open_workbench};

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let mut paths = Vec::new();
    let mut generated = Vec::new();
    let mut remaining = Vec::new();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--project" | "--generated" => {
                let Some(path) = args.next() else {
                    eprintln!("{arg} needs a path");
                    return ExitCode::from(2);
                };
                if arg == "--project" {
                    paths.push(std::path::PathBuf::from(path));
                } else {
                    generated.push(std::path::PathBuf::from(path));
                }
            }
            _ => remaining.push(arg),
        }
    }
    let options = match Options::parse(remaining, &Environment::from_process()) {
        Ok(options) => options,
        Err(message) => {
            eprintln!("file-finder-prototype: {message}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    if options.help {
        print!(
            "File Finder Experiment\n  --project <DIR>    Repeat for each project\n  --generated <PATH> Relative generated root; defaults to generated\n\n{USAGE}"
        );
        return ExitCode::SUCCESS;
    }

    if paths.is_empty() {
        paths = if options.samples {
            vec!["fixtures/alpha".into(), "fixtures/beta".into()]
        } else {
            vec![std::env::current_dir().expect("current directory")]
        };
    }
    let roots = paths
        .into_iter()
        .map(|path| path.canonicalize().unwrap_or(path))
        .map(|path| retrowork_file_finder_prototype::ProjectRoot {
            name: path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            path,
            generated: if generated.is_empty() {
                vec!["generated".into()]
            } else {
                generated.clone()
            },
        })
        .collect();
    gpui::application().run(move |cx| {
        retrowork_workbench::init(cx);
        let platform = retrowork_platform::Platform::builder()
            .module(retrowork_projects::ProjectsModule)
            .module(retrowork_file_finder_prototype::ui::PrototypeModule(roots))
            .build();
        let (width, height) = options.window_size;
        let window_options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                None,
                size(px(width), px(height)),
                cx,
            ))),
            titlebar: Some(TitlebarOptions {
                title: Some("File Finder Experiment".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let (window, workbench) = match open_workbench(platform, window_options, cx) {
            Ok(opened) => opened,
            Err(error) => {
                eprintln!("file-finder-prototype: cannot open a window: {error:#}");
                std::process::exit(1);
            }
        };
        if let Some(path) = &options.exit_report {
            automation::write_exit_report_on_quit(path.clone(), workbench.clone(), cx);
        }
        if let Some(socket) = &options.automation_socket
            && let Err(error) = automation::serve(socket, window, workbench, cx)
        {
            // A test run that asked for automation must not continue without it.
            eprintln!(
                "retrowork-demo: cannot serve automation on {}: {error}",
                socket.display()
            );
            std::process::exit(1);
        }
        cx.activate(true);
    });
    ExitCode::SUCCESS
}
