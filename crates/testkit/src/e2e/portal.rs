//! Real desktop portal services isolated from the user's session for native tests.

use super::*;

/// Private D-Bus and the installed GTK file chooser portal on the test display.
pub(super) struct Portal {
    pub(super) address: String,
    children: Vec<Child>,
}
impl Drop for Portal {
    fn drop(&mut self) {
        for child in self.children.iter_mut().rev() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
impl Portal {
    /// Starts existing system services; no test implementation replaces the chooser.
    pub(super) fn start(server: &XServer, artifacts: &Path) -> Outcome<Self> {
        let config = artifacts.join("portal-config");
        text(fs::create_dir_all(config.join("xdg-desktop-portal")))?;
        text(fs::write(
            config.join("xdg-desktop-portal/portals.conf"),
            "[preferred]\ndefault=gtk\norg.freedesktop.impl.portal.Secret=none\n",
        ))?;
        let mut bus_command = Command::new("dbus-daemon");
        Self::environment(&mut bus_command, server, &config);
        let mut bus = text(
            bus_command
                .args(["--session", "--nofork", "--print-address=1"])
                .stdout(Stdio::piped())
                .stderr(text(File::create(artifacts.join("dbus.log")))?)
                .spawn(),
        )?;
        let mut address = String::new();
        text(
            BufReader::new(bus.stdout.take().ok_or("missing D-Bus address pipe")?)
                .read_line(&mut address),
        )?;
        let mut portal = Self {
            address: address.trim().into(),
            children: vec![bus],
        };
        for program in ["xdg-desktop-portal-gtk", "xdg-desktop-portal"] {
            let mut command = Command::new(format!("/usr/libexec/{program}"));
            command
                .env("DBUS_SESSION_BUS_ADDRESS", &portal.address)
                .stdout(Stdio::null())
                .stderr(text(File::create(
                    artifacts.join(format!("{program}.log")),
                ))?);
            Self::environment(&mut command, server, &config);
            portal.children.push(text(command.spawn())?);
            let name = if program.ends_with("-gtk") {
                "org.freedesktop.impl.portal.desktop.gtk"
            } else {
                "org.freedesktop.portal.Desktop"
            };
            poll("desktop portal registration", STARTUP_PATIENCE, || {
                let output = text(
                    Command::new("gdbus")
                        .args([
                            "call",
                            "--address",
                            &portal.address,
                            "--dest",
                            "org.freedesktop.DBus",
                            "--object-path",
                            "/org/freedesktop/DBus",
                            "--method",
                            "org.freedesktop.DBus.NameHasOwner",
                            name,
                        ])
                        .output(),
                )?;
                Ok(String::from_utf8_lossy(&output.stdout)
                    .contains("true")
                    .then_some(()))
            })?;
        }
        Ok(portal)
    }

    /// Keeps both directly launched and bus-activated services on the private display.
    fn environment(command: &mut Command, server: &XServer, config: &Path) {
        command
            .env("DISPLAY", &server.display)
            .env("GDK_BACKEND", "x11")
            .env("XDG_CONFIG_HOME", config)
            .env("XDG_DATA_HOME", config.join("data"))
            .env("XDG_CACHE_HOME", config.join("cache"))
            .env("XDG_CURRENT_DESKTOP", "GNOME")
            .env("GSETTINGS_BACKEND", "memory")
            .env("GIO_USE_VFS", "local")
            .env("GIO_USE_VOLUME_MONITOR", "unix")
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("WAYLAND_SOCKET");
        if let Some(authority) = &server.authority {
            command.env("XAUTHORITY", authority);
        }
    }
}
