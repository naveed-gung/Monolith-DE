//! Windows integrations use the OS APIs; core playback stays platform independent.
#[cfg(windows)]
mod platform {
    use crate::{AppWindow, controller::Command};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use slint::{ComponentHandle, winit_030::WinitWindowAccessor};
    use std::{cell::Cell, sync::mpsc};
    use windows::{
        Foundation::{EventRegistrationToken, TimeSpan, TypedEventHandler},
        Media::*,
        Win32::{
            Foundation::*,
            System::{LibraryLoader::GetModuleHandleW, WinRT::*},
            UI::{Shell::*, WindowsAndMessaging::*},
        },
        core::{self, HSTRING, PCWSTR, w},
    };
    const TRAY_MESSAGE: u32 = WM_APP + 41;
    const WM_SETTINGCHANGE: u32 = 0x001A;
    const WM_THEMECHANGED: u32 = 0x031A;
    struct Context {
        ui: slint::Weak<AppWindow>,
        tx: mpsc::Sender<Command>,
    }
    unsafe extern "system" fn subclass(
        hwnd: HWND,
        msg: u32,
        wp: WPARAM,
        lp: LPARAM,
        _id: usize,
        data: usize,
    ) -> LRESULT {
        if matches!(msg, WM_SETTINGCHANGE | WM_THEMECHANGED) {
            let context = unsafe { &*(data as *const Context) };
            if let Some(ui) = context.ui.upgrade() {
                ui.window().with_winit_window(|_| {});
                ui.set_system_light(system_light_theme());
            }
        }
        if msg == TRAY_MESSAGE {
            // Context is owned by Native until the subclass is removed on the UI thread.
            let context = unsafe { &*(data as *const Context) };
            let action = if lp.0 as u32 == WM_LBUTTONDBLCLK {
                1
            } else if lp.0 as u32 == WM_RBUTTONUP {
                unsafe {
                    if let Ok(menu) = CreatePopupMenu() {
                        let _ = AppendMenuW(menu, MF_STRING, 1, w!("Open Monolith"));
                        let _ = AppendMenuW(menu, MF_STRING, 2, w!("Play / pause"));
                        let _ = AppendMenuW(menu, MF_STRING, 3, w!("Next track"));
                        let _ = AppendMenuW(menu, MF_STRING, 4, w!("Stop"));
                        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
                        let _ = AppendMenuW(menu, MF_STRING, 5, w!("Quit Monolith"));
                        let mut point = POINT::default();
                        let _ = GetCursorPos(&mut point);
                        let _ = SetForegroundWindow(hwnd);
                        let action = TrackPopupMenu(
                            menu,
                            TPM_RETURNCMD | TPM_RIGHTBUTTON,
                            point.x,
                            point.y,
                            0,
                            hwnd,
                            None,
                        )
                        .0;
                        let _ = DestroyMenu(menu);
                        action
                    } else {
                        0
                    }
                }
            } else {
                0
            };
            match action {
                1 => {
                    if let Some(ui) = context.ui.upgrade() {
                        ui.window().with_winit_window(|w| {
                            w.set_visible(true);
                            w.set_minimized(false);
                            w.focus_window();
                            w.request_redraw();
                        });
                        ui.window().request_redraw();
                    }
                }
                2 => {
                    let _ = context.tx.send(Command::Toggle);
                }
                3 => {
                    let _ = context.tx.send(Command::Next(1));
                }
                4 => {
                    let _ = context.tx.send(Command::Stop);
                }
                5 => {
                    let _ = slint::quit_event_loop();
                }
                _ => {}
            }
            return LRESULT(0);
        }
        unsafe { DefSubclassProc(hwnd, msg, wp, lp) }
    }
    /// Preserve the HWND and placement while notifying Slint that its backing
    /// buffer is lost. Windows emits a zero-size resize on minimize; Slint uses
    /// that event to invalidate its software renderer's partial-paint cache.
    pub fn hide_to_tray(ui: &AppWindow) {
        ui.window().with_winit_window(|window| {
            window.set_minimized(true);
            window.set_visible(false);
        });
    }
    pub struct Native {
        hwnd: HWND,
        context: Box<Context>,
        notify: NOTIFYICONDATAW,
        media: SystemMediaTransportControls,
        button: EventRegistrationToken,
        seek: EventRegistrationToken,
        last_track: Cell<i32>,
    }
    impl Native {
        pub fn new(ui: &AppWindow, tx: mpsc::Sender<Command>) -> core::Result<Self> {
            let hwnd = ui
                .window()
                .with_winit_window(|w| {
                    w.window_handle().ok().and_then(|h| match h.as_raw() {
                        RawWindowHandle::Win32(h) => Some(HWND(h.hwnd.get())),
                        _ => None,
                    })
                })
                .flatten()
                .ok_or_else(|| core::Error::new(E_HANDLE, "Window handle is not available"))?;
            // Slint may already have initialized the apartment. S_FALSE is also success.
            let _ = unsafe { RoInitialize(RO_INIT_SINGLETHREADED) };
            let interop: ISystemMediaTransportControlsInterop = core::factory::<
                SystemMediaTransportControls,
                ISystemMediaTransportControlsInterop,
            >()?;
            let media: SystemMediaTransportControls = unsafe { interop.GetForWindow(hwnd) }?;
            media.SetIsPlayEnabled(true)?;
            media.SetIsPauseEnabled(true)?;
            media.SetIsStopEnabled(true)?;
            media.SetIsNextEnabled(true)?;
            media.SetIsPreviousEnabled(true)?;
            let sender = tx.clone();
            type ButtonArgs = SystemMediaTransportControlsButtonPressedEventArgs;
            let button = media.ButtonPressed(&TypedEventHandler::new(
                move |_: &Option<SystemMediaTransportControls>, args: &Option<ButtonArgs>| {
                    if let Some(args) = args {
                        use SystemMediaTransportControlsButton as Pressed;
                        let command = match args.Button()? {
                            Pressed::Play => Some(Command::Resume),
                            Pressed::Pause => Some(Command::Pause),
                            Pressed::Stop => Some(Command::Stop),
                            Pressed::Next => Some(Command::Next(1)),
                            Pressed::Previous => Some(Command::Next(-1)),
                            _ => None,
                        };
                        if let Some(command) = command {
                            let _ = sender.send(command);
                        }
                    }
                    Ok(())
                },
            ))?;
            let sender = tx.clone();
            let seek = media.PlaybackPositionChangeRequested(&TypedEventHandler::new(
                move |_: &Option<SystemMediaTransportControls>,
                      args: &Option<PlaybackPositionChangeRequestedEventArgs>| {
                    if let Some(args) = args {
                        let _ = sender.send(Command::Seek(
                            args.RequestedPlaybackPosition()?.Duration as f32 / 10_000_000.,
                        ));
                    }
                    Ok(())
                },
            ))?;
            let context = Box::new(Context {
                ui: ui.as_weak(),
                tx,
            });
            let mut notify = NOTIFYICONDATAW {
                cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
                hWnd: hwnd,
                uID: 1,
                uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
                uCallbackMessage: TRAY_MESSAGE,
                ..Default::default()
            };
            let module = unsafe { GetModuleHandleW(None) }?;
            notify.hIcon = unsafe { LoadIconW(HINSTANCE(module.0), PCWSTR(1 as *const u16)) }
                .or_else(|_| unsafe { LoadIconW(None, IDI_APPLICATION) })?;
            let tip = "Monolith · Your music".encode_utf16().collect::<Vec<_>>();
            notify.szTip[..tip.len()].copy_from_slice(&tip);
            if !unsafe {
                SetWindowSubclass(
                    hwnd,
                    Some(subclass),
                    41,
                    (&*context as *const Context) as usize,
                )
            }
            .as_bool()
            {
                return Err(core::Error::new(E_FAIL, "Could not attach tray messages"));
            }
            if !unsafe { Shell_NotifyIconW(NIM_ADD, &notify) }.as_bool() {
                unsafe { RemoveWindowSubclass(hwnd, Some(subclass), 41) };
                return Err(core::Error::new(E_FAIL, "Could not add the tray icon"));
            }
            Ok(Self {
                hwnd,
                context,
                notify,
                media,
                button,
                seek,
                last_track: Cell::new(-2),
            })
        }
        pub fn sync(&self, ui: &AppWindow) -> core::Result<()> {
            let active = ui.get_current_id() >= 0;
            self.media.SetIsEnabled(active)?;
            self.media.SetPlaybackStatus(if !active {
                MediaPlaybackStatus::Closed
            } else if ui.get_playing() {
                MediaPlaybackStatus::Playing
            } else {
                MediaPlaybackStatus::Paused
            })?;
            if self.last_track.replace(ui.get_current_id()) != ui.get_current_id() {
                let display = self.media.DisplayUpdater()?;
                display.SetType(MediaPlaybackType::Music)?;
                let music = display.MusicProperties()?;
                music.SetTitle(&HSTRING::from(ui.get_current_title().as_str()))?;
                music.SetArtist(&HSTRING::from(ui.get_current_artist().as_str()))?;
                display.Update()?;
            }
            let timeline = SystemMediaTransportControlsTimelineProperties::new()?;
            let time = |s: f32| TimeSpan {
                Duration: (s.max(0.) as f64 * 10_000_000.) as i64,
            };
            timeline.SetStartTime(time(0.))?;
            timeline.SetMinSeekTime(time(0.))?;
            timeline.SetEndTime(time(ui.get_duration()))?;
            timeline.SetMaxSeekTime(time(ui.get_duration()))?;
            timeline.SetPosition(time(ui.get_position().min(ui.get_duration())))?;
            self.media.UpdateTimelineProperties(&timeline)
        }
    }
    impl Drop for Native {
        fn drop(&mut self) {
            let _ = self.media.RemoveButtonPressed(self.button);
            let _ = self.media.RemovePlaybackPositionChangeRequested(self.seek);
            let _ = self.media.SetIsEnabled(false);
            unsafe {
                Shell_NotifyIconW(NIM_DELETE, &self.notify);
                RemoveWindowSubclass(self.hwnd, Some(subclass), 41);
            }
            // Keep callback context alive until both native registrations are removed.
            let _ = &self.context;
        }
    }
    /// Windows app theme; true means light. Defaults to dark if the registry is unavailable.
    pub fn system_light_theme() -> bool {
        use windows::Win32::System::Registry::{
            HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, REG_VALUE_TYPE, RegCloseKey, RegOpenKeyExW,
            RegQueryValueExW,
        };
        unsafe {
            let subkey: Vec<u16> =
                "Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize\0"
                    .encode_utf16()
                    .collect();
            let value_name: Vec<u16> = "AppsUseLightTheme\0".encode_utf16().collect();
            let mut key = HKEY(0);
            if RegOpenKeyExW(
                HKEY_CURRENT_USER,
                windows::core::PCWSTR(subkey.as_ptr()),
                0,
                KEY_QUERY_VALUE,
                &mut key,
            )
            .is_err()
            {
                return false;
            }
            let mut data = 0u32;
            let mut size = std::mem::size_of::<u32>() as u32;
            let mut value_type = REG_VALUE_TYPE(0);
            let result = RegQueryValueExW(
                key,
                windows::core::PCWSTR(value_name.as_ptr()),
                None,
                Some(&mut value_type),
                Some(&mut data as *mut u32 as *mut u8),
                Some(&mut size),
            );
            let _ = RegCloseKey(key);
            result.is_ok() && data != 0
        }
    }

    /// Windows "Animation effects" setting; defaults to enabled if the query fails.
    pub fn animations_enabled() -> bool {
        let mut enabled = BOOL(1);
        let queried = unsafe {
            SystemParametersInfoW(
                SPI_GETCLIENTAREAANIMATION,
                0,
                Some(&mut enabled as *mut BOOL as _),
                SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
            )
        };
        queried.is_err() || enabled.as_bool()
    }
}
#[cfg(windows)]
pub use platform::{Native, animations_enabled, hide_to_tray, system_light_theme};

/// Desktop theme on non-Windows platforms; true means light.
///
/// GNOME and KDE both publish the preference through GSettings, and both ship
/// `gsettings` on PATH. Anything else falls back to dark, which is the product
/// default anyway. Invoked with an argument array, never a shell string.
#[cfg(not(windows))]
pub fn system_light_theme() -> bool {
    use std::process::Command;
    let mut query = Command::new("gsettings");
    query.args(["get", "org.gnome.desktop.interface", "color-scheme"]);
    let Ok(output) = query.output() else {
        return false;
    };
    let value = String::from_utf8_lossy(&output.stdout);
    let value = value.trim();
    // "prefer-dark" and "default" are the two values GNOME and KDE use.
    value.eq_ignore_ascii_case("default") || value.eq_ignore_ascii_case("prefer-light")
}

/// Animations are opt-out rather than opt-in on other desktops, and there is no
/// single API to ask. Stay enabled unless the user turns them off in Settings.
#[cfg(not(windows))]
pub fn animations_enabled() -> bool {
    true
}
