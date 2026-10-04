//! Monolith's Windows installer.
//!
//! `monolith-setup.exe` carries the player executable and icon inside itself,
//! so the single file can be copied anywhere and run. It installs per-user by
//! default, which needs no administrator rights, and registers itself in
//! Add/Remove Programs with a working uninstaller.

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use slint::{ComponentHandle, SharedString};
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

slint::include_modules!();

// Shared with the player so both decode the identity the same way. This file
// is dependency-free on purpose: see `src/seal.rs`.
#[path = "../../src/seal.rs"]
mod seal;
include!(concat!(env!("OUT_DIR"), "/sealed_name.rs"));

// Read from the manifest rather than written out by hand. It was a literal
// once, and Add/Remove Programs went on reporting 0.2.0 after the app had
// already shipped 0.3.0.
const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
const APP_NAME: &str = "Monolith";
const APP_EXE: &str = "monolith-desktop.exe";
const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\Monolith";
const UNINSTALLER: &str = "Uninstall Monolith.exe";

const PLAYER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/payload/monolith-desktop.exe"));
const ICON: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/payload/monolith.ico"));

/// Appends one line to a log in the temp folder.
///
/// A silent run has no window and no console, so without this a failure is
/// completely invisible. Failures here are never fatal; logging must not be the
/// reason an install breaks.
fn log(message: &str) {
    use std::io::Write;
    let path = env::temp_dir().join("monolith-setup.log");
    if let Ok(mut file) = fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(file, "[{}] {}", exe_path().display(), message);
    }
}

fn default_path() -> PathBuf {
    let base = dirs::data_local_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("Programs").join(APP_NAME)
}

fn exe_path() -> PathBuf {
    env::current_exe().unwrap_or_else(|_| PathBuf::from(APP_NAME))
}

fn start_menu_dir() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("Microsoft/Windows/Start Menu/Programs"))
}

fn desktop_dir() -> Option<PathBuf> {
    dirs::desktop_dir()
}

/// Writes a .lnk through PowerShell's WScript.Shell, which is present on every
/// Windows install. Argument arrays are used throughout; no shell strings.
fn write_shortcut(link: &Path, target: &Path, description: &str) -> Result<(), String> {
    let script = format!(
        "$s = (New-Object -ComObject WScript.Shell).CreateShortcut({}); $s.TargetPath = {}; $s.Description = {}; $s.IconLocation = '{}'; $s.Save()",
        ps_literal(&link.to_string_lossy()),
        ps_literal(&target.to_string_lossy()),
        ps_literal(description),
        target.to_string_lossy().replace('\'', "''")
    );
    run_powershell(&script).map_err(|e| e.to_string())
}

fn ps_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn powershell() -> Command {
    let mut command = Command::new("powershell");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW; dialogs still work.
    }
    command
}

fn run_powershell(script: &str) -> Result<(), String> {
    let script = format!("$ErrorActionPreference='Stop'; {script}");
    let status = powershell()
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            &script,
        ])
        .status()
        .map_err(|e| e.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("PowerShell exited with {status}"))
    }
}

fn write_registry_install(location: &Path) -> Result<(), String> {
    let uninstaller = location.join(UNINSTALLER);
    let display_icon = format!("{}\\monolith.ico,0", location.display());
    // Assembled from plain values rather than one big format string: the
    // PowerShell text contains braces that Rust would try to interpolate.
    let entries: Vec<(&str, String)> = vec![
        ("DisplayName", ps_literal(APP_NAME)),
        ("DisplayVersion", ps_literal(APP_VERSION)),
        // Decoded from the sealed payload, so no name is compiled in.
        ("Publisher", ps_literal(&seal::decode(&SEALED_NAME, 0))),
        ("InstallLocation", ps_literal(&location.to_string_lossy())),
        ("DisplayIcon", ps_literal(&display_icon)),
        (
            "UninstallString",
            ps_literal(&format!("\"{}\"", uninstaller.display())),
        ),
        (
            "QuietUninstallString",
            ps_literal(&format!("\"{}\" /S", uninstaller.display())),
        ),
    ];
    let mut script = format!(
        "$p = 'HKCU:\\{}'; New-Item -Path $p -Force | Out-Null;",
        UNINSTALL_KEY
    );
    for (name, value) in &entries {
        script.push_str(&format!(
            " Set-ItemProperty -Path $p -Name '{name}' -Value {value};"
        ));
    }
    script.push_str(" Set-ItemProperty -Path $p -Name 'NoModify' -Value 1 -Type DWord;");
    script.push_str(" Set-ItemProperty -Path $p -Name 'NoRepair' -Value 1 -Type DWord;");
    run_powershell(&script)
}

fn write_registry_uninstall() {
    let _ = run_powershell(&format!(
        "Remove-Item -Path 'HKCU:\\{}' -Recurse -Force -ErrorAction SilentlyContinue",
        UNINSTALL_KEY
    ));
}

/// The uninstaller is a byte-for-byte copy of this installer. Invoked with /S
/// it removes the install folder and its registration, then exits.
fn install_uninstaller(location: &Path) -> Result<(), String> {
    let current = exe_path();
    let target = location.join(UNINSTALLER);
    if current != target {
        fs::copy(&current, &target).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn install_to(
    location: &Path,
    start_menu: bool,
    desktop: bool,
    progress: impl Fn(f32, &str),
) -> Result<(), String> {
    if !location.is_absolute() {
        return Err("Choose an absolute folder path for Monolith.".into());
    }
    progress(
        0.0,
        "Preparing your update. Your library and music stay intact.",
    );
    // Ask only this installed copy to close. Never force-kill playback or
    // discard pending database writes to make an update appear successful.
    let close = r#"$ErrorActionPreference='Stop'; Get-Process -Name monolith-desktop -ErrorAction SilentlyContinue | ForEach-Object {
        if ($_.Path -eq $env:MONOLITH_UPDATE_EXE) {
            if (-not $_.CloseMainWindow() -or -not $_.WaitForExit(15000)) { throw 'Close Monolith (including its tray icon), then retry setup.' }
        }
    }; exit 0"#;
    let output = powershell()
        .args(["-NoProfile", "-NonInteractive", "-Command", close])
        .env("MONOLITH_UPDATE_EXE", location.join(APP_EXE))
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
    }
    fs::create_dir_all(location).map_err(|e| e.to_string())?;
    let app = location.join(APP_EXE);
    let staged = location.join(format!(".monolith-update-{}.exe", std::process::id()));
    let backup = location.join(format!(".monolith-previous-{}.exe", std::process::id()));
    if staged.exists() || backup.exists() {
        return Err("An update is already staged. Close setup and retry.".into());
    }
    progress(0.2, "Writing the new player.");
    fs::write(&staged, PLAYER).map_err(|e| e.to_string())?;
    let had_old = app.exists();
    if had_old {
        if let Err(error) = fs::rename(&app, &backup) {
            let _ = fs::remove_file(&staged);
            return Err(format!(
                "Close Monolith and retry. Your existing app was kept. ({error})"
            ));
        }
    }
    if let Err(error) = fs::rename(&staged, &app) {
        if had_old {
            let _ = fs::rename(&backup, &app);
        }
        return Err(format!("Could not replace the player: {error}"));
    }
    if had_old {
        let _ = fs::remove_file(&backup);
    }
    progress(0.6, "Registering Monolith and its uninstaller.");
    install_uninstaller(location)?;
    fs::write(location.join("monolith.ico"), ICON).map_err(|e| e.to_string())?;
    write_registry_install(location)?;
    progress(0.85, "Applying your shortcut choices.");
    for (enabled, directory) in [(start_menu, start_menu_dir()), (desktop, desktop_dir())] {
        if let Some(directory) = directory {
            let link = directory.join("Monolith.lnk");
            if enabled {
                write_shortcut(&link, &app, "Monolith")?;
            } else if link.exists() {
                fs::remove_file(&link).map_err(|e| e.to_string())?;
            }
        } else if enabled {
            return Err("Windows could not locate the requested shortcut folder.".into());
        }
    }
    progress(1.0, "Installation complete.");
    log(&format!(
        "installed {} into {}",
        APP_VERSION,
        location.display()
    ));
    Ok(())
}

/// Closes a running player so its executable is not locked.
///
/// The installer launches the player straight after a silent install, so an
/// uninstall that follows would otherwise be unable to delete it. This only
/// ever targets our own process name.
fn stop_running_player() {
    let _ = Command::new("taskkill")
        .args(["/F", "/IM", APP_EXE])
        .stdout(std::process::Stdio::null())
        .status();
}

fn uninstall() {
    stop_running_player();
    log("asked the player to close");
    // Read the location from our own registry key before deleting it. The
    // script is concatenated because the PowerShell braces would otherwise be
    // read as format placeholders.
    let mut script = format!("$p = 'HKCU:\\{}'; ", UNINSTALL_KEY);
    script.push_str(
        "$v = (Get-ItemProperty -Path $p -Name 'InstallLocation' -ErrorAction SilentlyContinue).InstallLocation; ",
    );
    script.push_str("if ($v) { $v }");
    let output = powershell()
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            &script,
        ])
        .output();
    if let Ok(out) = output {
        let path = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        log(&format!("registry InstallLocation = {path:?}"));
        if !path.is_empty() {
            let dir = PathBuf::from(&path);
            // Only ever remove our own folder, whatever the registry claims.
            if dir.file_name().is_some_and(|n| n == APP_NAME) {
                log(&format!("scheduling removal of {}", dir.display()));
                schedule_removal(&dir);
            } else {
                log(&format!("refusing to remove unexpected path {path}"));
            }
        }
    }
    if let Some(menu) = start_menu_dir() {
        let _ = fs::remove_file(menu.join("Monolith.lnk"));
    }
    write_registry_uninstall();
    log("registry entry removed");
}

/// Removes the install folder once this process has exited.
///
/// The uninstaller is a copy of itself living inside that folder, so Windows
/// holds a lock on the running image and a direct delete always fails. A
/// detached helper waits and then removes it.
///
/// The script is a fixed literal and the path travels in an environment
/// variable, so nothing from the registry can ever be read as code.
fn schedule_removal(dir: &Path) {
    // Retried because closing the player is not instantaneous and the first
    // attempt can still catch the file lock.
    const SCRIPT: &str = "1..10 | ForEach-Object { \
        Start-Sleep -Milliseconds 700; \
        Remove-Item -LiteralPath $env:MONOLITH_INSTALL_DIR -Recurse -Force \
        -ErrorAction SilentlyContinue; \
        if (-not (Test-Path -LiteralPath $env:MONOLITH_INSTALL_DIR)) { break } }";
    let _ = powershell()
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            SCRIPT,
        ])
        .env("MONOLITH_INSTALL_DIR", dir)
        .spawn();
}

fn launch(location: &Path) {
    let app = location.join(APP_EXE);
    let _ = Command::new(app).spawn();
}

/// Removes the app behind a short progress window.
///
/// The staged uninstaller is a GUI build with no console, so removing silently
/// looks to Add/Remove Programs like a hang. The window shows what is
/// happening; the deferred removal then takes the executable out from under
/// it a moment later, which is the expected way this process ends.
fn remove_with_progress() {
    uninstall();
    if slint::BackendSelector::new()
        .backend_name("winit".into())
        .renderer_name("software".into())
        .select()
        .is_err()
    {
        // Without a window the removal has still happened, which is the part
        // that matters.
        return;
    }
    let Ok(ui) = SetupWindow::new() else { return };
    ui.set_heading(SharedString::from("Removing Monolith"));
    ui.set_status(SharedString::from(
        "Closing Monolith and deleting its files. This window closes on its own.",
    ));
    ui.set_step(3);
    let _ = ui.show();
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().skip(1).collect();
    let silent = args.iter().any(|a| a == "/S" || a == "--silent");
    // The staged copy is literally named "Uninstall Monolith.exe", and Windows
    // runs the UninstallString with no arguments at all. Relying on a flag
    // alone meant that launching it from Add/Remove Programs re-installed the
    // app instead of removing it.
    let is_self_uninstaller = exe_path().file_name().is_some_and(|n| n == UNINSTALLER);
    let is_uninstall =
        is_self_uninstaller || args.iter().any(|a| a == "/uninstall" || a == "--uninstall");

    if is_uninstall {
        if silent {
            uninstall();
            return Ok(());
        }
        // Add/Remove Programs runs the UninstallString with no arguments, so
        // this is the normal interactive path. It must return either way:
        // falling through re-installed the app instead of removing it.
        remove_with_progress();
        return Ok(());
    }

    let location = default_path();

    if silent {
        // A silent run is an uninstall when invoked from Add/Remove Programs.
        if is_uninstall {
            uninstall();
        } else if let Err(error) = install_to(
            &location,
            true,
            desktop_dir().is_some_and(|d| d.join("Monolith.lnk").exists()),
            |_, message| log(message),
        ) {
            // There is no console in a GUI build, so a bare `?` would fail
            // completely silently. Record it and still exit non-zero.
            log(&format!("install failed: {error}"));
            return Err(error.into());
        } else if !args.iter().any(|a| a == "--no-launch") {
            launch(&location);
        }
        return Ok(());
    }

    slint::BackendSelector::new()
        .backend_name("winit".into())
        .renderer_name("software".into())
        .select()?;
    let ui = SetupWindow::new()?;
    ui.set_version(SharedString::from(APP_VERSION));
    ui.set_install_path(SharedString::from(location.to_string_lossy().into_owned()));
    ui.set_updating(location.join(APP_EXE).is_file());
    ui.set_start_menu(true);
    ui.set_desktop(desktop_dir().is_some_and(|d| d.join("Monolith.lnk").exists()));
    ui.set_launch(true);

    let weak = ui.as_weak();
    ui.on_browse(move || {
        let weak=weak.clone();
        std::thread::spawn(move || {
            let script = "Add-Type -AssemblyName System.Windows.Forms; $d = New-Object System.Windows.Forms.FolderBrowserDialog; $d.Description = 'Choose where Monolith should be installed'; if ($d.ShowDialog() -eq 'OK') { $d.SelectedPath }";
            let picked=powershell().args(["-NoProfile", "-NonInteractive", "-Command", script])
                .output().ok().filter(|o|o.status.success())
                .map(|o|String::from_utf8_lossy(&o.stdout).trim().to_owned()).filter(|s|!s.is_empty());
            if let Some(path)=picked { let _=weak.upgrade_in_event_loop(move |ui|ui.set_install_path(path.into())); }
        });
    });
    let weak = ui.as_weak();
    ui.on_install(move || {
        let Some(ui) = weak.upgrade() else { return };
        if ui.get_step() == 3 && !ui.get_failed() {
            return;
        }
        let path = PathBuf::from(ui.get_install_path().to_string());
        let start_menu = ui.get_start_menu();
        let desktop = ui.get_desktop();
        ui.set_failed(false);
        ui.set_progress(0.);
        ui.set_status("Preparing setup…".into());
        ui.set_step(3);
        let weak = weak.clone();
        std::thread::spawn(move || {
            let result = install_to(&path, start_menu, desktop, |progress, status| {
                let text = status.to_owned();
                let _ = weak.upgrade_in_event_loop(move |ui| {
                    ui.set_progress(progress);
                    ui.set_status(text.into());
                });
            });
            let _ = weak.upgrade_in_event_loop(move |ui| match result {
                Ok(()) => {
                    ui.set_step(4);
                    ui.set_status("Ready to listen.".into());
                }
                Err(error) => {
                    ui.set_failed(true);
                    ui.set_status(error.into());
                }
            });
        });
    });
    let weak = ui.as_weak();
    ui.window().on_close_requested(move || {
        if weak
            .upgrade()
            .is_some_and(|ui| ui.get_step() == 3 && !ui.get_failed())
        {
            slint::CloseRequestResponse::KeepWindowShown
        } else {
            slint::CloseRequestResponse::HideWindow
        }
    });

    let weak = ui.as_weak();
    ui.on_run_app(move || {
        if let Some(ui) = weak.upgrade() {
            launch(Path::new(&ui.get_install_path().to_string()));
        }
        slint::quit_event_loop().ok();
    });

    ui.on_quit(|| {
        slint::quit_event_loop().ok();
    });

    ui.run()?;
    Ok(())
}
