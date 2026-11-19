//! The demo executable: opens the workbench window on the demo platform.

use std::process::ExitCode;

use gpui::{Bounds, TitlebarOptions, WindowBounds, WindowOptions, px, size};
use retrowork_demo::{Environment, Options, USAGE, build_platform};
use retrowork_workbench::{automation, open_workbench};

fn main() -> ExitCode {
    let options = match Options::parse(std::env::args().skip(1), &Environment::from_process()) {
        Ok(options) => options,
        Err(message) => {
            eprintln!("retrowork-demo: {message}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    if options.help {
        print!("{USAGE}");
        return ExitCode::SUCCESS;
    }

    gpui::application().run(move |cx| {
        retrowork_workbench::init(cx);
        let platform = build_platform(&options);
        let (width, height) = options.window_size;
        let window_options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                None,
                size(px(width), px(height)),
                cx,
            ))),
            titlebar: Some(TitlebarOptions {
                title: Some("RetroWork Demo".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let (window, workbench) = match open_workbench(platform, window_options, cx) {
            Ok(opened) => opened,
            Err(error) => {
                eprintln!("retrowork-demo: cannot open a window: {error:#}");
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
