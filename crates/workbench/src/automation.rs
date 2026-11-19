//! Opt-in automation channel of a running workbench.
//!
//! When the application is started with an automation socket, a test driver
//! can connect to it and
//!
//! - **observe**: read the [`UiSnapshot`] of the most recently prepared
//!   frame, the event trace and the active GPU adapter, and
//! - **inject input**: hand a pointer or key event to the window, where it
//!   goes through the same hit testing, focus and key dispatch as an event
//!   from the window system.
//!
//! It cannot run commands, change state or read anything but what the window
//! shows. It is off unless requested on the command line, listens on a Unix
//! socket readable by the owner only, and serves one request per line of JSON.
//!
//! Injected input is *not* operating-system input: it enters at
//! `Window::dispatch_event`, after the platform layer. Tests that need to
//! prove the platform layer too send real X11 input instead and use this
//! channel only to observe. Injected events are marked `ui.injected` in the
//! trace so the two can never be confused.

use std::{
    io::{self, BufRead, BufReader, Read as _, Write},
    os::unix::{
        fs::PermissionsExt as _,
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    sync::mpsc as std_mpsc,
    thread,
    time::Duration,
};

use futures::{StreamExt as _, channel::mpsc};
use gpui::{
    AnyWindowHandle, App, Context, Entity, InputEvent as _, KeyDownEvent, KeyUpEvent, Keystroke,
    Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Window, point, px,
};
use retrowork_platform::TraceEvent;
use serde::{Deserialize, Serialize};

use crate::{semantics::UiSnapshot, workbench::Workbench};

/// Longest accepted request line, in bytes.
const MAX_REQUEST_BYTES: u64 = 64 * 1024;

/// A request a test driver sends, one JSON object per line.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum Request {
    /// The elements registered by the most recently prepared frame.
    Snapshot,
    /// Trace events with a sequence number above `after`.
    Trace { after: u64 },
    /// Ids of every contributed control; see [`Workbench::inventory`].
    Inventory,
    /// The GPU adapter the window renders with, as the renderer reports it.
    Renderer,
    /// Hands `event` to the window's event dispatch.
    Inject { event: InjectedInput },
}

/// Input handed to the window's event dispatch.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum InjectedInput {
    /// Move, press and release the left button at a window position.
    Click { x: f32, y: f32, count: usize },
    /// Press and release a key chord such as `ctrl-shift-s`.
    Key { keystroke: String },
    /// Type text into the focused element, one character at a time.
    Text { text: String },
}

/// The answer to one [`Request`], one JSON object per line.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "kebab-case")]
pub enum Response {
    Snapshot { snapshot: UiSnapshot },
    Trace { events: Vec<TraceEvent> },
    Inventory { controls: Vec<String> },
    Renderer { renderer: Option<RendererInfo> },
    Done,
    Error { message: String },
}

/// What the application records about itself as it quits.
///
/// A test driver cannot ask a process that has exited, so whatever the user
/// did in the last moments before quitting (choosing File > Exit, pressing
/// the quit shortcut) is only observable through this file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExitReport {
    /// The whole event trace, oldest first.
    pub trace: Vec<TraceEvent>,
    /// Ids of every contributed control; see [`Workbench::inventory`].
    pub inventory: Vec<String>,
}

/// Writes an [`ExitReport`] for `workbench` to `path` when the application
/// quits normally. A crash or a killed process writes nothing.
pub fn write_exit_report_on_quit(path: PathBuf, workbench: Entity<Workbench>, cx: &mut App) {
    cx.on_app_quit(move |cx| {
        let workbench = workbench.read(cx);
        let report = ExitReport {
            trace: workbench.platform().shell().trace().since(0),
            inventory: workbench.inventory(),
        };
        let written = serde_json::to_vec_pretty(&report)
            .map_err(io::Error::other)
            .and_then(|json| std::fs::write(&path, json));
        if let Err(error) = written {
            eprintln!(
                "cannot write the exit report to {}: {error}",
                path.display()
            );
        }
        async {}
    })
    .detach();
}

/// The GPU adapter a window renders with. `None` in a [`Response`] means
/// the renderer did not report one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RendererInfo {
    /// Device name, e.g. `AMD Radeon Vega 8 Graphics (RADV RAVEN)` or `llvmpipe`.
    pub device: String,
    pub driver: String,
    pub driver_info: String,
    /// Whether the device is a CPU implementation such as lavapipe.
    pub software: bool,
}

type Exchange = (Request, std_mpsc::Sender<Response>);

/// Starts serving `workbench` in `window` on a Unix socket at `socket`.
pub fn serve(
    socket: &Path,
    window: AnyWindowHandle,
    workbench: Entity<Workbench>,
    cx: &mut App,
) -> io::Result<()> {
    workbench.update(cx, |workbench, cx| {
        workbench.automation_enabled = true;
        cx.notify();
    });
    match std::fs::remove_file(socket) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let listener = UnixListener::bind(socket)?;
    std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))?;

    let (requests, mut incoming) = mpsc::unbounded::<Exchange>();
    thread::Builder::new()
        .name("automation-accept".into())
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                let requests = requests.clone();
                let _ = thread::Builder::new()
                    .name("automation-connection".into())
                    .spawn(move || serve_connection(stream, &requests));
            }
        })?;

    cx.spawn(async move |cx| {
        while let Some((request, reply)) = incoming.next().await {
            let response = window
                .update(cx, |_, window, cx| {
                    workbench.update(cx, |workbench, cx| workbench.automate(request, window, cx))
                })
                .unwrap_or_else(|error| Response::Error {
                    message: format!("the window is gone: {error}"),
                });
            let _ = reply.send(response);
        }
    })
    .detach();
    Ok(())
}

fn serve_connection(stream: UnixStream, requests: &mpsc::UnboundedSender<Exchange>) {
    let Ok(mut writer) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(stream);
    loop {
        let mut line = String::new();
        let read = (&mut reader).take(MAX_REQUEST_BYTES).read_line(&mut line);
        if !matches!(read, Ok(n) if n > 0) {
            return;
        }
        let response = match serde_json::from_str::<Request>(&line) {
            Ok(request) => {
                let (reply, answer) = std_mpsc::channel();
                if requests.unbounded_send((request, reply)).is_err() {
                    return;
                }
                answer
                    .recv_timeout(Duration::from_secs(10))
                    .unwrap_or_else(|_| Response::Error {
                        message: "the application did not answer within 10 s".to_owned(),
                    })
            }
            Err(error) => Response::Error {
                message: format!("unreadable request: {error}"),
            },
        };
        let mut reply = serde_json::to_vec(&response).expect("responses serialise");
        reply.push(b'\n');
        if writer.write_all(&reply).is_err() {
            return;
        }
    }
}

impl Workbench {
    /// Answers one automation request. Injected input is dispatched after this
    /// returns, so handlers can update the workbench themselves.
    pub fn automate(
        &mut self,
        request: Request,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Response {
        match request {
            Request::Snapshot => Response::Snapshot {
                snapshot: self.snapshot(window),
            },
            Request::Trace { after } => Response::Trace {
                events: self.platform.shell().trace().since(after),
            },
            Request::Inventory => Response::Inventory {
                controls: self.inventory(),
            },
            Request::Renderer => Response::Renderer {
                renderer: window.gpu_specs().map(|specs| RendererInfo {
                    device: specs.device_name,
                    driver: specs.driver_name,
                    driver_info: specs.driver_info,
                    software: specs.is_software_emulated,
                }),
            },
            Request::Inject { event } => {
                self.platform
                    .shell_mut()
                    .trace_mut()
                    .record("ui.injected", format!("{event:?}"));
                // Dispatch after this update returns: handlers update the
                // workbench themselves.
                window.defer(cx, move |window, cx| {
                    if let Err(message) = inject(event, window, cx) {
                        eprintln!("automation: {message}");
                    }
                });
                Response::Done
            }
        }
    }
}

/// Feeds `event` into the window's normal event dispatch.
fn inject(event: InjectedInput, window: &mut Window, cx: &mut App) -> Result<(), String> {
    match event {
        InjectedInput::Click { x, y, count } => {
            let position = point(px(x), px(y));
            let modifiers = Modifiers::default();
            window.dispatch_event(
                MouseMoveEvent {
                    position,
                    pressed_button: None,
                    modifiers,
                }
                .to_platform_input(),
                cx,
            );
            for click_count in 1..=count.max(1) {
                window.dispatch_event(
                    MouseDownEvent {
                        button: MouseButton::Left,
                        position,
                        modifiers,
                        click_count,
                        first_mouse: false,
                    }
                    .to_platform_input(),
                    cx,
                );
                window.dispatch_event(
                    MouseUpEvent {
                        button: MouseButton::Left,
                        position,
                        modifiers,
                        click_count,
                    }
                    .to_platform_input(),
                    cx,
                );
            }
        }
        InjectedInput::Key { keystroke } => {
            let parsed = parse_keystroke(&keystroke, cx)?;
            press(parsed, window, cx);
        }
        InjectedInput::Text { text } => {
            for character in text.chars() {
                let name = match character {
                    ' ' => "space".to_owned(),
                    other => other.to_string(),
                };
                let mut keystroke = parse_keystroke(&name, cx)?;
                keystroke.key_char = Some(character.to_string());
                press(keystroke, window, cx);
            }
        }
    }
    Ok(())
}

fn parse_keystroke(text: &str, cx: &App) -> Result<Keystroke, String> {
    match Keystroke::parse_with_os(text, cx.platform_info().os) {
        Ok(keystroke) => Ok(keystroke),
        Err(error) => Err(format!("invalid keystroke `{text}`: {error}")),
    }
}

fn press(keystroke: Keystroke, window: &mut Window, cx: &mut App) {
    window.dispatch_event(
        KeyDownEvent {
            keystroke: keystroke.clone(),
            is_held: false,
            prefer_character_input: false,
        }
        .to_platform_input(),
        cx,
    );
    window.dispatch_event(KeyUpEvent { keystroke }.to_platform_input(), cx);
}
