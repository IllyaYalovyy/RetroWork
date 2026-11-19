//! End-to-end testing of the real executable in a real window.
//!
//! Tier: **rendered-window E2E with operating-system input**.
//!
//! - The application under test is the demo executable, started as a separate
//!   process with its normal command line.
//! - It runs as an X11 client of a private X server: by default a nested
//!   `Xwayland` started for the run with its own authority cookie, so input
//!   and screenshots never touch the user's desktop session. An existing X
//!   display (for example Xvfb in CI) can be used instead through
//!   `RETROWORK_E2E_DISPLAY`.
//! - Input is synthesised with the XTEST extension: the X server delivers
//!   real pointer and key events to the application's window, so the
//!   platform layer, hit testing, focus and key dispatch are all exercised.
//! - Observation uses the application's automation channel (element ids,
//!   bounds, state, event trace) and screenshots read back from the X server.
//!
//! There is no window manager in the private server. The driver therefore
//! assigns keyboard focus to the application window itself, which is what a
//! window manager would do when the window is mapped.

mod portal;

use std::{
    fs::{self, File},
    io::{BufRead, BufReader, BufWriter, Read, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use image::{
    ExtendedColorType, ImageEncoder as _,
    codecs::png::{CompressionType, FilterType, PngEncoder},
};
use retrowork_platform::TraceEvent;
use retrowork_workbench::{
    automation::{ExitReport, InjectedInput, Request, Response},
    semantics::UiSnapshot,
};
use x11rb::{
    connection::Connection as _,
    protocol::{
        xproto::{
            self, BUTTON_PRESS_EVENT, BUTTON_RELEASE_EVENT, ConnectionExt as _, ImageFormat,
            InputFocus, KEY_PRESS_EVENT, KEY_RELEASE_EVENT, MOTION_NOTIFY_EVENT, MapState,
        },
        xtest::ConnectionExt as _,
    },
    rust_connection::{DefaultStream, RustConnection},
};

use crate::driver::{Outcome, UiDriver};

const STARTUP_PATIENCE: Duration = Duration::from_secs(30);
const WINDOW_SIZE: (u16, u16) = (1200, 780);
const SERVER_SIZE: (u16, u16) = (1280, 860);

fn text<T, E: std::fmt::Display>(result: Result<T, E>) -> Outcome<T> {
    result.map_err(|error| error.to_string())
}

/// Polls `probe` until it yields a value. Used only for conditions outside
/// the application (a socket appearing, a process exiting).
fn poll<T>(
    what: &str,
    patience: Duration,
    mut probe: impl FnMut() -> Outcome<Option<T>>,
) -> Outcome<T> {
    let deadline = Instant::now() + patience;
    loop {
        if let Some(value) = probe()? {
            return Ok(value);
        }
        if Instant::now() >= deadline {
            return Err(format!("timed out after {patience:?} waiting for {what}"));
        }
        thread::sleep(Duration::from_millis(20));
    }
}

/// How pointer and key input reaches the application.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputBackend {
    /// X server events synthesised with XTEST: operating-system input.
    XTest,
    /// Events handed to the window through the automation channel. Not
    /// operating-system input; the report says so.
    Injected,
}

impl InputBackend {
    /// The backend's name in reports and on the command line.
    pub fn name(self) -> &'static str {
        match self {
            InputBackend::XTest => "xtest",
            InputBackend::Injected => "injected",
        }
    }
}

/// The X server the application under test connects to.
pub struct XServer {
    pub display: String,
    /// `nested-xwayland` or `external`.
    pub kind: &'static str,
    authority: Option<PathBuf>,
    child: Option<Child>,
    connection: RustConnection,
    root: xproto::Window,
    keymap: Keymap,
    portal: Option<portal::Portal>,
}

impl Drop for XServer {
    fn drop(&mut self) {
        self.portal.take();
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl XServer {
    /// Starts a private nested Xwayland, or connects to the display named by
    /// `RETROWORK_E2E_DISPLAY`. The error text says what to install or set.
    pub fn start(artifacts: &Path) -> Outcome<Self> {
        let external = std::env::var("RETROWORK_E2E_DISPLAY")
            .ok()
            .filter(|display| !display.is_empty());
        let mut server = match external {
            Some(display) => {
                let (connection, screen) = RustConnection::connect(Some(&display)).map_err(|error| {
                    format!("cannot connect to X display `{display}` from RETROWORK_E2E_DISPLAY: {error}")
                })?;
                let root = connection.setup().roots[screen].root;
                Self {
                    display,
                    kind: "external",
                    authority: None,
                    child: None,
                    connection,
                    root,
                    keymap: Keymap::default(),
                    portal: None,
                }
            }
            None => Self::start_nested(artifacts)?,
        };
        text(server.connection.xtest_get_version(2, 2))
            .and_then(|cookie| text(cookie.reply()))
            .map_err(|error| {
                format!(
                    "X display `{}` does not offer the XTEST extension needed for input: {error}",
                    server.display
                )
            })?;
        server.keymap = Keymap::load(&server.connection)?;
        server.portal = Some(portal::Portal::start(&server, artifacts)?);
        Ok(server)
    }

    fn start_nested(artifacts: &Path) -> Outcome<Self> {
        if std::env::var_os("WAYLAND_DISPLAY").is_none_or(|value| value.is_empty()) {
            return Err(
                "no X server available for UI tests: this is not a Wayland session, so a nested \
                 Xwayland cannot be started. Set RETROWORK_E2E_DISPLAY to an X display with \
                 XTEST and a Vulkan-capable renderer (for example `Xvfb :99 -screen 0 1280x860x24`)."
                    .to_owned(),
            );
        }
        // A private cookie: only processes given this file can connect.
        let mut cookie = [0_u8; 16];
        text(File::open("/dev/urandom").and_then(|mut random| random.read_exact(&mut cookie)))?;
        let authority = artifacts.join("Xauthority");
        let mut entry = Vec::new();
        entry.extend_from_slice(&0xFFFF_u16.to_be_bytes()); // FamilyWild
        for field in [&b""[..], b"", b"MIT-MAGIC-COOKIE-1", &cookie] {
            entry.extend_from_slice(&(field.len() as u16).to_be_bytes());
            entry.extend_from_slice(field);
        }
        text(fs::write(&authority, entry))?;

        let log = text(File::create(artifacts.join("xwayland.log")))?;
        let mut child = Command::new("Xwayland")
            .args(["-displayfd", "1", "-noreset", "-geometry"])
            .arg(format!("{}x{}", SERVER_SIZE.0, SERVER_SIZE.1))
            .arg("-auth")
            .arg(&authority)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(log)
            .spawn()
            .map_err(|error| {
                format!(
                    "cannot start Xwayland for UI tests: {error}. Install it (Fedora: `sudo dnf \
                     install xorg-x11-server-Xwayland`) or set RETROWORK_E2E_DISPLAY to an X display."
                )
            })?;

        // Xwayland writes the display number it chose once it accepts clients.
        let stdout = child.stdout.take().expect("stdout was piped");
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let mut line = String::new();
            let _ = BufReader::new(stdout).read_line(&mut line);
            let _ = sender.send(line);
        });
        let number = receiver
            .recv_timeout(STARTUP_PATIENCE)
            .ok()
            .and_then(|line| line.trim().parse::<u32>().ok());
        let Some(number) = number else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "Xwayland did not become ready; see {}",
                artifacts.join("xwayland.log").display()
            ));
        };

        let connect = || -> Outcome<RustConnection> {
            let socket = text(UnixStream::connect(format!("/tmp/.X11-unix/X{number}")))?;
            let (stream, _) = text(DefaultStream::from_unix_stream(socket))?;
            text(RustConnection::connect_to_stream_with_auth_info(
                stream,
                0,
                b"MIT-MAGIC-COOKIE-1".to_vec(),
                cookie.to_vec(),
            ))
        };
        let connection = match connect() {
            Ok(connection) => connection,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("cannot connect to the nested Xwayland: {error}"));
            }
        };
        let root = connection.setup().roots[0].root;
        Ok(Self {
            display: format!(":{number}"),
            kind: "nested-xwayland",
            authority: Some(authority),
            child: Some(child),
            connection,
            root,
            keymap: Keymap::default(),
            portal: None,
        })
    }

    /// Round trip: returns once the server has processed everything sent.
    fn sync(&self) -> Outcome {
        text(text(self.connection.get_input_focus())?.reply()).map(drop)
    }

    fn fake(&self, kind: u8, detail: u8, x: i16, y: i16) -> Outcome {
        text(self.connection.xtest_fake_input(
            kind,
            detail,
            x11rb::CURRENT_TIME,
            self.root,
            x,
            y,
            0,
        ))?;
        self.sync()
    }

    fn press_key(&self, keysym: u32, extra_shift: bool) -> Outcome {
        let (keycode, needs_shift) = self.keymap.lookup(keysym)?;
        let shift = if extra_shift || needs_shift {
            Some(self.keymap.lookup(KEYSYM_SHIFT)?.0)
        } else {
            None
        };
        if let Some(shift) = shift {
            self.fake(KEY_PRESS_EVENT, shift, 0, 0)?;
        }
        self.fake(KEY_PRESS_EVENT, keycode, 0, 0)?;
        self.fake(KEY_RELEASE_EVENT, keycode, 0, 0)?;
        if let Some(shift) = shift {
            self.fake(KEY_RELEASE_EVENT, shift, 0, 0)?;
        }
        Ok(())
    }
}

const KEYSYM_SHIFT: u32 = 0xffe1;
const KEYSYM_CONTROL: u32 = 0xffe3;
const KEYSYM_ALT: u32 = 0xffe9;

/// The server's keyboard mapping: which key produces which symbol.
#[derive(Default)]
struct Keymap {
    min_keycode: u8,
    per_keycode: usize,
    keysyms: Vec<u32>,
}

impl Keymap {
    fn load(connection: &RustConnection) -> Outcome<Self> {
        let setup = connection.setup();
        let (min, max) = (setup.min_keycode, setup.max_keycode);
        let mapping = text(text(connection.get_keyboard_mapping(min, max - min + 1))?.reply())?;
        Ok(Self {
            min_keycode: min,
            per_keycode: usize::from(mapping.keysyms_per_keycode),
            keysyms: mapping.keysyms,
        })
    }

    /// The key that produces `keysym` and whether Shift must be held.
    fn lookup(&self, keysym: u32) -> Outcome<(u8, bool)> {
        for level in 0..2.min(self.per_keycode) {
            let found = self
                .keysyms
                .chunks(self.per_keycode.max(1))
                .position(|symbols| symbols.get(level) == Some(&keysym));
            if let Some(index) = found {
                return Ok((self.min_keycode + index as u8, level == 1));
            }
        }
        Err(format!(
            "the X keyboard mapping has no key for keysym {keysym:#x}"
        ))
    }
}

/// X keysym for a key name in the chord notation of key bindings.
fn keysym(key: &str) -> Outcome<u32> {
    let named = match key {
        "enter" => 0xff0d,
        "escape" => 0xff1b,
        "tab" => 0xff09,
        "space" => 0x20,
        "backspace" => 0xff08,
        "delete" => 0xffff,
        "up" => 0xff52,
        "down" => 0xff54,
        "left" => 0xff51,
        "right" => 0xff53,
        "home" => 0xff50,
        "end" => 0xff57,
        "pageup" => 0xff55,
        "pagedown" => 0xff56,
        _ => 0,
    };
    if named != 0 {
        return Ok(named);
    }
    if let Some(number) = key.strip_prefix('f').and_then(|n| n.parse::<u32>().ok())
        && (1..=12).contains(&number)
    {
        return Ok(0xffbe + number - 1);
    }
    let mut chars = key.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if (' '..='~').contains(&c) => Ok(c as u32),
        _ => Err(format!("no X keysym known for key `{key}`")),
    }
}

/// Client side of the application's automation channel.
struct Automation {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
}

impl Automation {
    fn connect(socket: &Path) -> Outcome<Self> {
        let stream = text(UnixStream::connect(socket))?;
        text(stream.set_read_timeout(Some(Duration::from_secs(15))))?;
        Ok(Self {
            writer: text(stream.try_clone())?,
            reader: BufReader::new(stream),
        })
    }

    fn request(&mut self, request: &Request) -> Outcome<Response> {
        let mut line = text(serde_json::to_vec(request))?;
        line.push(b'\n');
        self.writer
            .write_all(&line)
            .map_err(|error| format!("the application closed the automation channel: {error}"))?;
        let mut reply = String::new();
        let read = self
            .reader
            .read_line(&mut reply)
            .map_err(|error| format!("no answer on the automation channel: {error}"))?;
        if read == 0 {
            return Err("the application closed the automation channel".to_owned());
        }
        match text(serde_json::from_str(&reply))? {
            Response::Error { message } => Err(format!("automation: {message}")),
            response => Ok(response),
        }
    }
}

/// Where the things a run needs are.
pub struct Setup {
    pub demo: PathBuf,
    pub packages: PathBuf,
    pub input: InputBackend,
    /// A private directory with a short path for automation sockets: Unix
    /// socket paths are limited to about a hundred bytes.
    pub sockets: PathBuf,
    /// Projects copied into each instance's working directory as
    /// `fixtures/<name>`, so scenarios can open them by relative path and
    /// build them without touching the originals.
    pub fixtures: PathBuf,
}

/// Variables of the test environment that must not reach Cargo inside the
/// application: they would instrument fixture builds or share a target
/// directory with this workspace.
pub const ISOLATED_CARGO_VARIABLES: &[&str] = &[
    "RUSTFLAGS",
    "CARGO_ENCODED_RUSTFLAGS",
    "RUSTDOCFLAGS",
    "RUSTC_WRAPPER",
    "RUSTC_WORKSPACE_WRAPPER",
    "CARGO_TARGET_DIR",
    "CARGO_BUILD_TARGET_DIR",
    "LLVM_PROFILE_FILE",
    "CARGO_LLVM_COV",
    "CARGO_LLVM_COV_TARGET_DIR",
];

/// Copies directory `from` to `to`, skipping build output in `target`.
pub fn copy_dir(from: &Path, to: &Path) -> Outcome {
    text(fs::create_dir_all(to))?;
    for entry in text(fs::read_dir(from))? {
        let entry = text(entry)?;
        let target = to.join(entry.file_name());
        if text(entry.file_type())?.is_dir() {
            if entry.file_name() != "target" {
                copy_dir(&entry.path(), &target)?;
            }
        } else {
            text(fs::copy(entry.path(), &target))?;
        }
    }
    Ok(())
}

/// An OS picker ready for confirmation; owned by one running test application.
pub struct PathSelection {
    kind: &'static str,
    window: xproto::Window,
    trace: u64,
    accept: bool,
}

/// A running instance of the application under test.
pub struct App<'a> {
    server: &'a XServer,
    child: Child,
    automation: Automation,
    window: xproto::Window,
    /// Window origin on the root window, in device pixels.
    origin: (i32, i32),
    size: (u16, u16),
    scale: f32,
    input: InputBackend,
    /// Directory for this instance's evidence.
    evidence: PathBuf,
    screenshots: Vec<String>,
    /// From starting the process to a drawn, focused window.
    pub startup: Duration,
    /// Where the application writes its [`ExitReport`] when it quits.
    exit_report: PathBuf,
}

impl Drop for App<'_> {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl<'a> App<'a> {
    /// Starts the application with `state_dir` and waits until its window is
    /// drawn and has keyboard focus. `tag` names its log files.
    ///
    /// The application runs in `evidence` as its working directory, with the
    /// fixtures copied there the first time, Cargo offline and the
    /// [`ISOLATED_CARGO_VARIABLES`] removed.
    pub fn launch(
        server: &'a XServer,
        setup: &Setup,
        state_dir: &Path,
        evidence: &Path,
        tag: &str,
    ) -> Outcome<Self> {
        text(fs::create_dir_all(evidence))?;
        let fixtures = evidence.join("fixtures");
        if !fixtures.exists() && setup.fixtures.is_dir() {
            copy_dir(&setup.fixtures, &fixtures)?;
        }
        static LAUNCHES: AtomicUsize = AtomicUsize::new(0);
        let socket = setup
            .sockets
            .join(format!("{}.sock", LAUNCHES.fetch_add(1, Ordering::Relaxed)));
        let mut command = Command::new(&setup.demo);
        command
            .arg("--samples")
            .arg("--packages")
            .arg(&setup.packages)
            .arg("--state-dir")
            .arg(state_dir)
            .arg("--automation-socket")
            .arg(&socket)
            .arg("--exit-report")
            .arg(evidence.join(format!("{tag}.exit-report.json")))
            .arg("--window-size")
            .arg(format!("{}x{}", WINDOW_SIZE.0, WINDOW_SIZE.1))
            .env(
                "DBUS_SESSION_BUS_ADDRESS",
                &server
                    .portal
                    .as_ref()
                    .ok_or("test portal is unavailable")?
                    .address,
            )
            .env("DISPLAY", &server.display)
            // The application must be an X11 client of the test server, not
            // a Wayland client of the desktop session.
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("WAYLAND_SOCKET")
            .env("CARGO_NET_OFFLINE", "true")
            .current_dir(evidence)
            .stdin(Stdio::null())
            .stdout(text(File::create(
                evidence.join(format!("{tag}.stdout.log")),
            ))?)
            .stderr(text(File::create(
                evidence.join(format!("{tag}.stderr.log")),
            ))?);
        if let Some(authority) = &server.authority {
            command.env("XAUTHORITY", authority);
        }
        for name in ISOLATED_CARGO_VARIABLES {
            command.env_remove(name);
        }
        let spawned = Instant::now();
        let mut child = command
            .spawn()
            .map_err(|error| format!("cannot start {}: {error}", setup.demo.display()))?;

        let started = (|| -> Outcome<_> {
            poll("the automation socket", STARTUP_PATIENCE, || {
                if let Some(status) = text(child.try_wait())? {
                    return Err(format!(
                        "the application exited during startup with {status}"
                    ));
                }
                Ok(socket.exists().then_some(()))
            })?;
            let mut automation = Automation::connect(&socket)?;
            let snapshot = poll("the first frame", STARTUP_PATIENCE, || {
                match automation.request(&Request::Snapshot)? {
                    Response::Snapshot { snapshot } if !snapshot.nodes.is_empty() => {
                        Ok(Some(snapshot))
                    }
                    _ => Ok(None),
                }
            })?;
            let (window, origin, size) =
                poll("the application's X window", STARTUP_PATIENCE, || {
                    find_window(server)
                })?;
            Ok((automation, snapshot, window, origin, size))
        })();
        let (automation, snapshot, window, origin, size) = match started {
            Ok(started) => started,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                let stderr = fs::read_to_string(evidence.join(format!("{tag}.stderr.log")))
                    .unwrap_or_default();
                let tail: Vec<&str> = stderr.lines().rev().take(12).collect();
                return Err(format!(
                    "{error}\n  application stderr (last lines):\n    {}",
                    tail.into_iter().rev().collect::<Vec<_>>().join("\n    ")
                ));
            }
        };

        // No window manager runs in the test server; give the window the
        // keyboard focus one would assign.
        text(server.connection.set_input_focus(
            InputFocus::POINTER_ROOT,
            window,
            x11rb::CURRENT_TIME,
        ))?;
        server.sync()?;

        Ok(Self {
            server,
            child,
            automation,
            window,
            origin,
            size,
            scale: snapshot.scale,
            input: setup.input,
            evidence: evidence.to_owned(),
            screenshots: Vec::new(),
            startup: spawned.elapsed(),
            exit_report: evidence.join(format!("{tag}.exit-report.json")),
        })
    }

    /// Reads what the application wrote about itself as it quit. Fails if it
    /// has not quit normally.
    pub fn exit_report(&self) -> Outcome<ExitReport> {
        let bytes = fs::read(&self.exit_report).map_err(|error| {
            format!(
                "no exit report at {}: the application did not quit normally ({error})",
                self.exit_report.display()
            )
        })?;
        text(serde_json::from_slice(&bytes))
    }

    /// Waits for the application to exit by itself and returns its status.
    pub fn wait_for_exit(&mut self, patience: Duration) -> Outcome<ExitStatus> {
        let child = &mut self.child;
        poll("the application to exit", patience, || {
            text(child.try_wait())
        })
    }

    /// File names of the screenshots taken so far, relative to the evidence
    /// directory.
    pub fn screenshots(&self) -> &[String] {
        &self.screenshots
    }

    /// Types a directory into the actual system chooser, outside any measured confirmation interval.
    pub fn prepare_folder(&mut self, path: Option<&str>) -> Outcome<PathSelection> {
        self.prepare_picker(path, "folder")
    }

    /// Types a path into the system chooser and records its actual drawable.
    fn prepare_picker(&mut self, path: Option<&str>, kind: &'static str) -> Outcome<PathSelection> {
        if !matches!(self.input, InputBackend::XTest) {
            return Err("system dialogs require operating-system input".into());
        }
        let mark = self.snapshot()?.trace_seq;
        let dialog = poll("the system folder picker", STARTUP_PATIENCE, || {
            let tree = text(text(self.server.connection.query_tree(self.server.root))?.reply())?;
            for child in tree.children {
                if child == self.window {
                    continue;
                }
                let attributes =
                    text(text(self.server.connection.get_window_attributes(child))?.reply())?;
                let geometry = text(text(self.server.connection.get_geometry(child))?.reply())?;
                if attributes.map_state == MapState::VIEWABLE
                    && geometry.width >= 200
                    && geometry.height >= 200
                {
                    return Ok(Some(child));
                }
            }
            Ok(None)
        })?;
        text(self.server.connection.configure_window(
            dialog,
            &xproto::ConfigureWindowAux::new().stack_mode(xproto::StackMode::ABOVE),
        ))?;
        text(self.server.connection.set_input_focus(
            InputFocus::POINTER_ROOT,
            dialog,
            x11rb::CURRENT_TIME,
        ))?;
        self.server.sync()?;
        poll("the system chooser to paint", STARTUP_PATIENCE, || {
            let (_, _, pixels) = self.dialog_pixels(dialog)?;
            let colours: std::collections::BTreeSet<_> =
                pixels.as_chunks::<4>().0.iter().step_by(16).collect();
            Ok((colours.len() > 8).then_some(()))
        })?;
        if let Some(path) = path {
            self.key("alt-home")?;
            thread::sleep(Duration::from_millis(200));
            self.key("ctrl-l")?;
            // GTK gives its newly shown location entry focus on the next main-loop turn.
            thread::sleep(Duration::from_millis(200));
            self.key("ctrl-a")?;
            self.text(&self.evidence.join(path).display().to_string())?;
            thread::sleep(Duration::from_millis(200));
        }
        let file = format!("system-picker-{mark}.png");
        let (width, height, pixels) = self.dialog_pixels(dialog)?;
        text(image::save_buffer(
            self.evidence.join(&file),
            &pixels,
            width.into(),
            height.into(),
            image::ColorType::Rgba8,
        ))?;
        self.screenshots.push(file);
        Ok(PathSelection {
            kind,
            window: dialog,
            trace: mark,
            accept: path.is_some(),
        })
    }

    /// Confirms or cancels the prepared chooser and waits for the application's response.
    pub fn finish_picker(&mut self, prompt: PathSelection) -> Outcome {
        if prompt.accept {
            let geometry = text(text(self.server.connection.get_geometry(prompt.window))?.reply())?;
            let origin = text(
                text(self.server.connection.translate_coordinates(
                    prompt.window,
                    self.server.root,
                    0,
                    0,
                ))?
                .reply(),
            )?;
            let x =
                (f32::from(origin.dst_x) + f32::from(geometry.width) - 48. - self.origin.0 as f32)
                    / self.scale;
            let y =
                (f32::from(origin.dst_y) + f32::from(geometry.height) - 23. - self.origin.1 as f32)
                    / self.scale;
            self.pointer_click(x, y, 1)?;
        } else {
            self.key("escape")?;
        }
        poll("the system picker to close", STARTUP_PATIENCE, || {
            let closed = self
                .server
                .connection
                .get_window_attributes(prompt.window)
                .ok()
                .and_then(|cookie| cookie.reply().ok())
                .is_none_or(|attributes| attributes.map_state != MapState::VIEWABLE);
            Ok(closed.then_some(()))
        })?;
        poll(
            "the application to receive the picker result",
            STARTUP_PATIENCE,
            || {
                Ok(self
                    .trace_since(prompt.trace)?
                    .iter()
                    .any(|event| event.kind == "dialog.closed" && event.detail == prompt.kind)
                    .then_some(()))
            },
        )?;
        text(self.server.connection.set_input_focus(
            InputFocus::POINTER_ROOT,
            self.window,
            x11rb::CURRENT_TIME,
        ))?;
        self.server.sync()
    }

    /// Reads the system chooser's drawable after it has mapped on the test display.
    fn dialog_pixels(&self, window: xproto::Window) -> Outcome<(u16, u16, Vec<u8>)> {
        let geometry = text(text(self.server.connection.get_geometry(window))?.reply())?;
        let reply = text(
            text(self.server.connection.get_image(
                ImageFormat::Z_PIXMAP,
                window,
                0,
                0,
                geometry.width,
                geometry.height,
                !0,
            ))?
            .reply(),
        )?;
        if reply.data.len() != usize::from(geometry.width) * usize::from(geometry.height) * 4 {
            return Err("unsupported system chooser image format".into());
        }
        Ok((
            geometry.width,
            geometry.height,
            reply
                .data
                .as_chunks::<4>()
                .0
                .iter()
                .flat_map(|pixel| [pixel[2], pixel[1], pixel[0], 255])
                .collect(),
        ))
    }

    /// Reads the window pixels from the X server.
    pub fn capture_pixels(&self) -> Outcome<(u16, u16, Vec<u8>)> {
        let geometry = text(text(self.server.connection.get_geometry(self.server.root))?.reply())?;
        let x = self.origin.0.clamp(0, i32::from(geometry.width)) as i16;
        let y = self.origin.1.clamp(0, i32::from(geometry.height)) as i16;
        let width = self.size.0.min(geometry.width.saturating_sub(x as u16));
        let height = self.size.1.min(geometry.height.saturating_sub(y as u16));
        let image = text(
            text(self.server.connection.get_image(
                ImageFormat::Z_PIXMAP,
                self.server.root,
                x,
                y,
                width,
                height,
                !0,
            ))?
            .reply(),
        )?;
        if image.data.len() != usize::from(width) * usize::from(height) * 4 {
            return Err(format!(
                "unexpected screenshot format: depth {}, {} bytes for {width}x{height}",
                image.depth,
                image.data.len()
            ));
        }
        Ok((
            width,
            height,
            image
                .data
                .as_chunks::<4>()
                .0
                .iter()
                .flat_map(|pixel| [pixel[2], pixel[1], pixel[0], 255])
                .collect(),
        ))
    }

    /// Saves pixels, semantics and input trace belonging to the same prepared frame.
    pub fn screenshot(&mut self, name: &str) -> Outcome<PathBuf> {
        let deadline = Instant::now() + Duration::from_secs(10);
        let (width, height, rgba, snapshot) = loop {
            let snapshot = self.snapshot()?;
            let (width, height, rgba) = self.capture_pixels()?;
            if pixels_match_frame(&rgba, width, height, &snapshot) {
                break (width, height, rgba, snapshot);
            }
            if Instant::now() >= deadline {
                let after = self.snapshot()?;
                let path = self.evidence.join(format!("unmatched-{name}.png"));
                text(image::save_buffer(
                    &path,
                    &rgba,
                    u32::from(width),
                    u32::from(height),
                    image::ColorType::Rgba8,
                ))?;
                text(std::fs::write(
                    path.with_extension("snapshots.json"),
                    text(serde_json::to_vec_pretty(&(snapshot, after)))?,
                ))?;
                text(std::fs::write(
                    path.with_extension("trace.json"),
                    text(serde_json::to_vec_pretty(&self.trace_since(0)?))?,
                ))?;
                return Err(format!(
                    "screenshot `{name}` never matched its semantic frame; unmatched evidence: {}",
                    path.display()
                ));
            }
            self.pause();
        };
        // A window that is not being drawn comes back as one flat colour.
        // Sampling every 16th pixel is enough to tell.
        let colours: std::collections::BTreeSet<[u8; 3]> = rgba
            .as_chunks::<4>()
            .0
            .iter()
            .step_by(16)
            .map(|pixel| [pixel[0], pixel[1], pixel[2]])
            .collect();
        if colours.len() < 8 {
            return Err(format!(
                "screenshot `{name}` shows only {} colours: the window is not being drawn",
                colours.len()
            ));
        }
        let file = format!("{:02}-{name}.png", self.screenshots.len() + 1);
        let path = self.evidence.join(&file);
        // Fast compression: these are evidence files, written many times per run.
        let encoder = PngEncoder::new_with_quality(
            BufWriter::new(text(File::create(&path))?),
            CompressionType::Fast,
            FilterType::NoFilter,
        );
        text(encoder.write_image(
            &rgba,
            u32::from(width),
            u32::from(height),
            ExtendedColorType::Rgba8,
        ))?;
        let snapshot_path = path.with_extension("snapshot.json");
        text(std::fs::write(
            &snapshot_path,
            text(serde_json::to_vec_pretty(&snapshot))?,
        ))?;
        let trace: Vec<_> = self
            .trace_since(0)?
            .into_iter()
            .filter(|event| event.seq <= snapshot.trace_seq)
            .collect();
        text(std::fs::write(
            path.with_extension("trace.json"),
            text(serde_json::to_vec_pretty(&trace))?,
        ))?;
        self.screenshots.push(file);
        Ok(path)
    }

    fn device(&self, x: f32, y: f32) -> (i16, i16) {
        (
            (self.origin.0 as f32 + x * self.scale).round() as i16,
            (self.origin.1 as f32 + y * self.scale).round() as i16,
        )
    }
}

/// A window with its origin on the root window and its size, in device pixels.
type PlacedWindow = (xproto::Window, (i32, i32), (u16, u16));

/// The application's top-level window: the one large, mapped child of the root.
fn find_window(server: &XServer) -> Outcome<Option<PlacedWindow>> {
    let connection = &server.connection;
    let tree = text(text(connection.query_tree(server.root))?.reply())?;
    for child in tree.children {
        let Ok(attributes) = text(connection.get_window_attributes(child))?.reply() else {
            continue;
        };
        let Ok(geometry) = text(connection.get_geometry(child))?.reply() else {
            continue;
        };
        if attributes.map_state == MapState::VIEWABLE
            && geometry.width >= 200
            && geometry.height >= 200
        {
            let origin =
                text(text(connection.translate_coordinates(child, server.root, 0, 0))?.reply())?;
            return Ok(Some((
                child,
                (i32::from(origin.dst_x), i32::from(origin.dst_y)),
                (geometry.width, geometry.height),
            )));
        }
    }
    Ok(None)
}

impl UiDriver for App<'_> {
    fn input_route(&self) -> &'static str {
        self.input.name()
    }

    fn snapshot(&mut self) -> Outcome<UiSnapshot> {
        match self.automation.request(&Request::Snapshot)? {
            Response::Snapshot { snapshot } => {
                snapshot.validate()?;
                Ok(snapshot)
            }
            other => Err(format!("unexpected automation answer: {other:?}")),
        }
    }

    fn trace_since(&mut self, after: u64) -> Outcome<Vec<TraceEvent>> {
        match self.automation.request(&Request::Trace { after })? {
            Response::Trace { events } => Ok(events),
            other => Err(format!("unexpected automation answer: {other:?}")),
        }
    }

    fn pointer_click(&mut self, x: f32, y: f32, count: usize) -> Outcome {
        match self.input {
            InputBackend::Injected => self
                .automation
                .request(&Request::Inject {
                    event: InjectedInput::Click { x, y, count },
                })
                .map(drop),
            InputBackend::XTest => {
                let (x, y) = self.device(x, y);
                self.server.fake(MOTION_NOTIFY_EVENT, 0, x, y)?;
                for _ in 0..count {
                    self.server.fake(BUTTON_PRESS_EVENT, 1, x, y)?;
                    self.server.fake(BUTTON_RELEASE_EVENT, 1, x, y)?;
                }
                Ok(())
            }
        }
    }

    fn choose_folder(&mut self, path: Option<&str>) -> Outcome {
        let prompt = self.prepare_folder(path)?;
        self.finish_picker(prompt)
    }

    fn choose_save_path(&mut self, path: Option<&str>) -> Outcome {
        let prompt = self.prepare_picker(path, "save-file")?;
        self.finish_picker(prompt)
    }

    fn key(&mut self, chord: &str) -> Outcome {
        match self.input {
            InputBackend::Injected => self
                .automation
                .request(&Request::Inject {
                    event: InjectedInput::Key {
                        keystroke: chord.to_owned(),
                    },
                })
                .map(drop),
            InputBackend::XTest => {
                let mut parts: Vec<&str> = chord.split('-').collect();
                let key = keysym(parts.pop().unwrap_or_default())?;
                let mut held = Vec::new();
                let mut shift = false;
                for modifier in parts {
                    match modifier {
                        "ctrl" => held.push(self.server.keymap.lookup(KEYSYM_CONTROL)?.0),
                        "alt" => held.push(self.server.keymap.lookup(KEYSYM_ALT)?.0),
                        "shift" => shift = true,
                        other => return Err(format!("unknown modifier `{other}` in `{chord}`")),
                    }
                }
                for keycode in &held {
                    self.server.fake(KEY_PRESS_EVENT, *keycode, 0, 0)?;
                }
                let pressed = self.server.press_key(key, shift);
                for keycode in held.iter().rev() {
                    self.server.fake(KEY_RELEASE_EVENT, *keycode, 0, 0)?;
                }
                pressed
            }
        }
    }

    fn text(&mut self, text: &str) -> Outcome {
        match self.input {
            InputBackend::Injected => self
                .automation
                .request(&Request::Inject {
                    event: InjectedInput::Text {
                        text: text.to_owned(),
                    },
                })
                .map(drop),
            InputBackend::XTest => {
                for character in text.chars() {
                    if !(' '..='~').contains(&character) {
                        return Err(format!("cannot type {character:?}: only printable ASCII"));
                    }
                    self.server.press_key(character as u32, false)?;
                }
                Ok(())
            }
        }
    }

    fn pause(&mut self) {
        // Interval between observations of the application, not a wait for
        // a particular state.
        thread::sleep(Duration::from_millis(15));
    }

    fn patience(&self) -> Duration {
        Duration::from_secs(10)
    }

    fn checkpoint(&mut self, name: &str) -> Outcome {
        self.screenshot(name).map(drop)
    }
}

impl App<'_> {
    /// Process id of the application.
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// The X window id of the application, for diagnostics.
    pub fn window_id(&self) -> u32 {
        self.window
    }

    /// The GPU adapter the application's window reports, as one line, or
    /// `unknown: <reason>` when it reports none.
    pub fn renderer(&mut self) -> String {
        match self.automation.request(&Request::Renderer) {
            Ok(Response::Renderer {
                renderer: Some(info),
            }) => format!(
                "{} ({}, {}){}",
                info.device,
                info.driver,
                info.driver_info,
                if info.software { ", software" } else { "" }
            ),
            Ok(Response::Renderer { renderer: None }) => {
                "unknown: the renderer reported no adapter".to_owned()
            }
            Ok(other) => format!("unknown: unexpected answer {other:?}"),
            Err(error) => format!("unknown: {error}"),
        }
    }

    /// Ids of every control the application's modules contributed.
    pub fn inventory(&mut self) -> Outcome<Vec<String>> {
        match self.automation.request(&Request::Inventory)? {
            Response::Inventory { controls } => Ok(controls),
            other => Err(format!("unexpected automation answer: {other:?}")),
        }
    }
}

/// Checks the pixels actually drawn for the snapshot, rejecting stale or missing markers.
pub fn pixels_match_frame(rgba: &[u8], width: u16, height: u16, snapshot: &UiSnapshot) -> bool {
    let Some(marker) = snapshot.marker else {
        return false;
    };
    if rgba.len() != usize::from(width) * usize::from(height) * 4 {
        return false;
    }
    for row in 0..2 {
        for cell in 0..68 {
            let x = ((marker.x + cell as f32 * 2. + 1.) * snapshot.scale).floor() as usize;
            let y = ((marker.y + row as f32 * 2. + 1.) * snapshot.scale).floor() as usize;
            if x >= usize::from(width) || y >= usize::from(height) {
                return false;
            }
            let offset = (y * usize::from(width) + x) * 4;
            let white = retrowork_workbench::semantics::frame_marker_bit(snapshot.frame, cell, row);
            if !rgba[offset..offset + 3]
                .iter()
                .all(|value| if white { *value > 240 } else { *value < 15 })
            {
                return false;
            }
        }
    }
    true
}
