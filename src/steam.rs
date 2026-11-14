//! Finding Steam, its Proton builds, and the running client.

use crate::vdf;
use std::cmp::Ordering;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct Steam {
    pub root: PathBuf,
    /// Whose namespaces games are launched in: 0 for the running Steam client. Tests point
    /// it at a fake Steam.
    pub sandbox_pid: i32,
}

/// A compatibility tool games can run with.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Proton {
    /// Display name, also the key saved in settings.
    pub name: String,
    /// Contains the `proton` script.
    pub dir: PathBuf,
    /// The Steam Linux Runtime entry point the tool expects to run inside, if any.
    pub runtime: Option<PathBuf>,
}

pub fn detect(home: &Path) -> Option<Steam> {
    [".local/share/Steam", ".steam/steam", ".steam/root"].iter().find_map(|p| {
        let root = std::fs::canonicalize(home.join(p)).ok()?;
        root.join("steamapps").is_dir().then_some(Steam { root, sandbox_pid: 0 })
    })
}

/// A Steam asterofix can't use. Flatpak's and Snap's Steam run in a sandbox of their own, and
/// their Proton can't be started from outside it with the overlay.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unsupported {
    Flatpak,
    Snap,
}

impl Unsupported {
    pub fn name(self) -> &'static str {
        match self {
            Unsupported::Flatpak => "Flatpak",
            Unsupported::Snap => "Snap",
        }
    }
}

/// The Flatpak or Snap Steam in `home`, which says why no native Steam was found.
pub fn unsupported(home: &Path) -> Option<Unsupported> {
    if home.join(".var/app/com.valvesoftware.Steam").is_dir() {
        Some(Unsupported::Flatpak)
    } else if home.join("snap/steam").is_dir() {
        Some(Unsupported::Snap)
    } else {
        None
    }
}

/// "not found", and why when that's a Steam asterofix can't use, in English for the command
/// line, the log and diagnostics.
pub fn not_found(unsupported: Option<Unsupported>) -> String {
    match unsupported {
        None => "not found".into(),
        Some(u) => format!(
            "not found (Steam is installed as a {}, which asterofix doesn't support: install the native Steam)",
            u.name()
        ),
    }
}

/// The command line's error when `home` has no Steam to use.
pub fn no_steam_error(home: &Path) -> String {
    format!("Steam installation {}", not_found(unsupported(home)))
}

impl Steam {
    pub fn target_pid(&self) -> i32 {
        if self.sandbox_pid != 0 { self.sandbox_pid } else { steam_client_pid() }
    }

    /// Custom tools (newest first), then Valve's builds.
    pub fn protons(&self) -> Vec<Proton> {
        let mut seen = std::collections::HashSet::new();
        let mut add = |list: &mut Vec<Proton>, name: String, dir: PathBuf| {
            if !seen.contains(&name) && dir.join("proton").exists() {
                seen.insert(name.clone());
                list.push(Proton { runtime: self.runtime_for(&dir), name, dir });
            }
        };

        let mut custom = Vec::new();
        for base in [self.root.join("compatibilitytools.d"), PathBuf::from("/usr/share/steam/compatibilitytools.d")] {
            for dir in crate::scan::read_dir_sorted(&base) {
                let manifest = vdf::read(&dir.join("compatibilitytool.vdf"));
                for (key, tool) in &manifest.path(&["compatibilitytools", "compat_tools"]).0 {
                    let vdf::Value::Obj(tool) = tool else { continue };
                    let name = match tool.str("display_name") {
                        "" => key.clone(),
                        n => n.to_owned(),
                    };
                    let tool_dir = match tool.str("install_path") {
                        "" | "." => dir.clone(),
                        install => dir.join(install),
                    };
                    add(&mut custom, name, tool_dir);
                }
            }
        }
        custom.sort_by(|a, b| natural_cmp(&b.name, &a.name));

        let mut valve = Vec::new();
        let common = self.root.join("steamapps/common");
        for dir in crate::scan::read_dir_sorted(&common) {
            let name = crate::scan::file_name(&dir);
            if name.starts_with("Proton") {
                add(&mut valve, name, dir);
            }
        }
        valve.sort_by(|a, b| natural_cmp(&b.name, &a.name));
        custom.extend(valve);
        custom
    }

    /// The Steam Linux Runtime a tool declares in its toolmanifest.vdf (e.g. sniper), as that
    /// runtime's entry point.
    fn runtime_for(&self, tool_dir: &Path) -> Option<PathBuf> {
        let manifest = vdf::read(&tool_dir.join("toolmanifest.vdf"));
        let app_id = manifest.obj("manifest").str("require_tool_appid");
        if app_id.is_empty() {
            return None;
        }
        let app = vdf::read(&self.root.join(format!("steamapps/appmanifest_{app_id}.acf")));
        let install_dir = app.obj("AppState").str("installdir");
        if install_dir.is_empty() {
            return None;
        }
        let entry = self.root.join("steamapps/common").join(install_dir).join("_v2-entry-point");
        entry.exists().then_some(entry)
    }

    /// The tool Steam uses for "all other titles", if we have it.
    pub fn default_proton(&self, protons: &[Proton]) -> String {
        let config = vdf::read(&self.root.join("config/config.vdf"));
        let mapping = config.path(&["InstallConfigStore", "Software", "Valve", "Steam", "CompatToolMapping", "0"]);
        let name = mapping.str("name");
        protons
            .iter()
            .find(|p| {
                let dir = crate::scan::file_name(&p.dir);
                p.name == name || dir == name || valve_tool_name(&dir).is_some_and(|n| n == name)
            })
            .or(protons.first())
            .map(|p| p.name.clone())
            .unwrap_or_default()
    }
}

/// Steam's internal name for one of Valve's Proton builds, from its folder: "Proton 9.0" is
/// proton_9, "Proton 5.13" proton_513.
fn valve_tool_name(dir: &str) -> Option<String> {
    match dir {
        "Proton - Experimental" => return Some("proton_experimental".into()),
        "Proton Hotfix" => return Some("proton_hotfix".into()),
        _ => {}
    }
    let (major, minor) = dir.strip_prefix("Proton ")?.split_once('.')?;
    if !(major.chars().chain(minor.chars()).all(|c| c.is_ascii_digit())) {
        return None;
    }
    Some(if minor == "0" { format!("proton_{major}") } else { format!("proton_{major}{minor}") })
}

/// Whether a Steam client process exists.
pub fn steam_running() -> bool {
    steam_pids().next().is_some()
}

/// The PID of the running Steam client binary, or 0.
pub fn steam_client_pid() -> i32 {
    client_pid(steam_pids().map(|pid| {
        let exe = std::fs::read_link(format!("/proc/{pid}/exe")).unwrap_or_default();
        let ppid = std::fs::read_to_string(format!("/proc/{pid}/status"))
            .ok()
            .and_then(|s| s.lines().find_map(|l| l.strip_prefix("PPid:")?.trim().parse::<i32>().ok()))
            .unwrap_or(0);
        let parent = std::fs::read_to_string(format!("/proc/{ppid}/comm")).unwrap_or_default();
        (pid, exe, parent.trim().to_string())
    }))
}

/// The Steam client among processes named steam, each with its executable and its parent's
/// name. The 32-bit client is ubuntu12_32/steam, the 64-bit one steamrt64/steam; anything
/// else (NixOS's steam wrapper script) isn't the client. The 64-bit client runs itself again
/// inside a pressure-vessel container (under pv-adverb), whose /usr is the Steam Runtime's,
/// so games join the outer one.
fn client_pid(procs: impl Iterator<Item = (i32, PathBuf, String)>) -> i32 {
    procs
        .filter(|(_, exe, parent)| {
            let exe = exe.to_string_lossy();
            (exe.ends_with("/ubuntu12_32/steam") || exe.ends_with("/steamrt64/steam")) && parent != "pv-adverb"
        })
        .map(|(pid, ..)| pid)
        .next()
        .unwrap_or(0)
}

fn steam_pids() -> impl Iterator<Item = i32> {
    std::fs::read_dir("/proc").into_iter().flatten().filter_map(|e| {
        let pid: i32 = e.ok()?.file_name().to_str()?.parse().ok()?;
        let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
        (comm.trim() == "steam").then_some(pid)
    })
}

/// An nsenter prefix that runs a command inside the namespaces of `pid` (the Steam client),
/// or None when they match ours. On NixOS Steam lives in a bubblewrap FHS sandbox with a
/// private /tmp and its own PID namespace; the overlay exchanges frames through /tmp and
/// tracks games by PID, so a game started outside that sandbox grabs input but never
/// renders the overlay.
pub fn namespace_wrapper(pid: i32) -> Option<Vec<String>> {
    if pid <= 0 {
        return None;
    }
    let nsenter = look_path("nsenter")?;
    let flags = namespace_flags(|ns| {
        match (std::fs::read_link(format!("/proc/self/ns/{ns}")), std::fs::read_link(format!("/proc/{pid}/ns/{ns}"))) {
            (Ok(me), Ok(them)) => me != them,
            _ => false,
        }
    });
    if flags.is_empty() {
        return None;
    }
    let mut argv = vec![nsenter.to_string_lossy().into_owned(), "--target".into(), pid.to_string()];
    argv.extend(flags.iter().map(|f| f.to_string()));
    argv.push("--".into());
    Some(argv)
}

/// The nsenter flags for the namespaces that differ.
pub fn namespace_flags(differs: impl Fn(&str) -> bool) -> Vec<&'static str> {
    let mut flags = Vec::new();
    if differs("user") {
        flags.extend(["--user", "--preserve-credentials"]);
    }
    if differs("mnt") {
        flags.extend(["--mount", "--root"]);
    }
    if differs("ipc") {
        flags.push("--ipc");
    }
    if differs("pid") {
        flags.push("--pid");
    }
    flags
}

/// Starts the Steam client minimized to the tray.
pub fn start_steam() -> std::io::Result<()> {
    let mut child = std::process::Command::new("steam").arg("-silent").spawn()?;
    std::thread::spawn(move || child.wait());
    Ok(())
}

/// Finds an executable on PATH.
pub fn look_path(name: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(name))
        .find(|p| std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0))
}

/// Compares strings with embedded numbers numerically ("GE-Proton9-27" < "GE-Proton10-4").
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (ca, cb) = (chunks(a), chunks(b));
    for (x, y) in ca.iter().zip(&cb) {
        let order = match (x.parse::<u64>(), y.parse::<u64>()) {
            (Ok(xn), Ok(yn)) => xn.cmp(&yn),
            _ => x.to_lowercase().cmp(&y.to_lowercase()),
        };
        if order != Ordering::Equal {
            return order;
        }
    }
    ca.len().cmp(&cb.len())
}

fn chunks(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut prev: Option<bool> = None;
    for (i, c) in s.char_indices() {
        let digit = c.is_ascii_digit();
        if prev.is_some_and(|p| p != digit) {
            out.push(&s[start..i]);
            start = i;
        }
        prev = Some(digit);
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{tempdir, write};

    #[test]
    fn natural_ordering() {
        assert_eq!(natural_cmp("GE-Proton9-27", "GE-Proton10-4"), Ordering::Less);
        assert_eq!(natural_cmp("GE-Proton10-34", "GE-Proton10-4"), Ordering::Greater);
        assert_eq!(natural_cmp("Proton 9.0", "proton 9.0"), Ordering::Equal);
    }

    #[test]
    fn namespace_flags_for_each_namespace() {
        assert_eq!(
            namespace_flags(|_| true),
            ["--user", "--preserve-credentials", "--mount", "--root", "--ipc", "--pid"]
        );
        assert!(namespace_flags(|_| false).is_empty());
        assert_eq!(namespace_wrapper(std::process::id() as i32), None, "our own process needs no wrapper");
        assert_eq!(namespace_wrapper(0), None, "no Steam needs no wrapper");
    }

    /// Steam's 64-bit client is steamrt64/steam. Only the 32-bit ubuntu12_32/steam was
    /// known, so with it running games fell back to steam-run outside Steam's sandbox, where
    /// a Unity game couldn't create a DXGI factory and crashed. Its second copy inside
    /// pressure-vessel's container isn't the one to join.
    #[test]
    fn finds_the_32_and_64_bit_steam_clients() {
        let root = "/home/u/.local/share/Steam";
        let proc = |pid, exe: &str, parent: &str| (pid, PathBuf::from(exe), parent.to_string());
        let wrapper = proc(10, "/nix/store/abc-bash-5.2/bin/bash", "fish");
        assert_eq!(client_pid([wrapper.clone(), proc(20, &format!("{root}/ubuntu12_32/steam"), "bash")].into_iter()), 20);
        assert_eq!(
            client_pid(
                [
                    wrapper.clone(),
                    proc(30, &format!("{root}/steamrt64/steam"), "pv-adverb"),
                    proc(40, &format!("{root}/steamrt64/steam"), "bash"),
                ]
                .into_iter()
            ),
            40
        );
        assert_eq!(client_pid([wrapper, proc(50, "", "bash")].into_iter()), 0, "no client, or its exe unreadable");
    }

    #[test]
    fn lists_protons_with_their_runtime_and_the_default() {
        let home = tempdir();
        let root = home.path().join(".local/share/Steam");
        let tools = root.join("compatibilitytools.d");
        for v in ["GE-Proton9-27", "GE-Proton10-4"] {
            write(&tools.join(v).join("proton"), "");
            write(
                &tools.join(v).join("compatibilitytool.vdf"),
                format!(
                    r#""compatibilitytools" {{ "compat_tools" {{ "{v}" {{ "install_path" "." "display_name" "{v}" }} }} }}"#
                ),
            );
            write(&tools.join(v).join("toolmanifest.vdf"), r#""manifest" { "require_tool_appid" "1628350" }"#);
        }
        write(
            &root.join("steamapps/appmanifest_1628350.acf"),
            r#""AppState" { "installdir" "SteamLinuxRuntime_sniper" }"#,
        );
        write(&root.join("steamapps/common/SteamLinuxRuntime_sniper/_v2-entry-point"), "");
        write(&root.join("steamapps/common/Proton - Experimental/proton"), "");
        write(
            &root.join("config/config.vdf"),
            r#""InstallConfigStore" { "Software" { "Valve" { "Steam" { "CompatToolMapping" { "0" { "name" "proton_experimental" } } } } } }"#,
        );

        let steam = detect(home.path()).unwrap();
        let protons = steam.protons();
        let names: Vec<_> = protons.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["GE-Proton10-4", "GE-Proton9-27", "Proton - Experimental"]);
        assert_eq!(
            protons[0].runtime,
            Some(steam.root.join("steamapps/common/SteamLinuxRuntime_sniper/_v2-entry-point"))
        );
        assert_eq!(protons[2].runtime, None);
        assert_eq!(steam.default_proton(&protons), "Proton - Experimental");
        assert!(detect(tempdir().path()).is_none());
    }

    /// Flatpak's and Snap's Steam aren't supported, and the error says so rather than that
    /// there's no Steam at all. A native Steam beside one is used.
    #[test]
    fn says_when_steam_is_a_flatpak_or_a_snap() {
        let home = tempdir();
        assert_eq!(unsupported(home.path()), None);
        assert_eq!(no_steam_error(home.path()), "Steam installation not found");
        for (dir, kind) in [
            (".var/app/com.valvesoftware.Steam/.local/share/Steam", Unsupported::Flatpak),
            ("snap/steam/common", Unsupported::Snap),
        ] {
            let home = tempdir();
            std::fs::create_dir_all(home.path().join(dir)).unwrap();
            assert_eq!(unsupported(home.path()), Some(kind));
            assert!(detect(home.path()).is_none());
            let e = no_steam_error(home.path());
            assert!(e.starts_with("Steam installation not found (Steam is installed as a "), "{e}");
            assert!(e.contains(kind.name()) && e.contains("install the native Steam"), "{e}");
        }
    }

    /// Steam saves its default as the tool's internal name: Valve's numbered builds are
    /// proton_9, proton_513 and so on. Only the Experimental and Hotfix names were known, so
    /// a default of Proton 9.0 fell back to the first custom build.
    #[test]
    fn default_proton_follows_steams_internal_names() {
        let home = tempdir();
        let root = home.path().join(".local/share/Steam");
        write(&root.join("compatibilitytools.d/GE-Proton10-4/proton"), "");
        write(
            &root.join("compatibilitytools.d/GE-Proton10-4/compatibilitytool.vdf"),
            r#""compatibilitytools" { "compat_tools" { "GE-Proton10-4" { "install_path" "." } } }"#,
        );
        for v in ["Proton 10.0", "Proton 9.0", "Proton 6.3", "Proton 5.13", "Proton Hotfix", "Proton - Experimental"] {
            write(&root.join("steamapps/common").join(v).join("proton"), "");
        }
        let steam = detect(home.path()).unwrap();
        let protons = steam.protons();
        for (internal, want) in [
            ("proton_10", "Proton 10.0"),
            ("proton_9", "Proton 9.0"),
            ("proton_63", "Proton 6.3"),
            ("proton_513", "Proton 5.13"),
            ("proton_hotfix", "Proton Hotfix"),
            ("proton_experimental", "Proton - Experimental"),
            ("GE-Proton10-4", "GE-Proton10-4"),
            ("proton_4", "GE-Proton10-4"), // not installed: the first build
        ] {
            write(
                &root.join("config/config.vdf"),
                format!(
                    r#""InstallConfigStore" {{ "Software" {{ "Valve" {{ "Steam" {{ "CompatToolMapping" {{ "0" {{ "name" "{internal}" }} }} }} }} }} }}"#
                ),
            );
            assert_eq!(steam.default_proton(&protons), want, "Steam's default {internal}");
        }
    }
}
