// Micro Music Room v2. Copyright (C) 2026 Marko Idžan. Licensed under GPL-3.0-or-later.
// Rewritten in Rust from Micro Music Room (WPF) 1.0.3. Modified November 24, 2026.

//! The Windows side of [`MediaSession`]:
//!
//! * the System Media Transport Controls (SMTC): the volume flyout and lock-screen "now playing"
//!   card, and the keyboard and Bluetooth-headset media keys that arrive through it;
//! * track-change toast notifications.
//!
//! Windows calls the SMTC handlers on one of its own threads, so they only push a [`MediaCommand`]
//! into a channel; the UI thread picks them up from [`WinMedia::take_commands`].
//!
//! Notifications need the app to have an identity. An MSIX-packaged app has one automatically. A
//! plain exe (the zip version) registers one under `HKCU\Software\Classes\AppUserModelId`, which
//! touches nothing outside the current user.

use crate::{
    media::{MediaCommand, MediaSession, NowPlaying, Playback},
    winthumb::ThumbBar,
};
use std::{
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver},
    time::Duration,
};
use windows::{
    Data::Xml::Dom::XmlDocument,
    Foundation::TypedEventHandler,
    Media::{
        MediaPlaybackStatus, MediaPlaybackType, PlaybackPositionChangeRequestedEventArgs,
        SystemMediaTransportControls, SystemMediaTransportControlsButton,
        SystemMediaTransportControlsButtonPressedEventArgs,
        SystemMediaTransportControlsTimelineProperties,
    },
    Storage::Streams::{DataWriter, InMemoryRandomAccessStream, RandomAccessStreamReference},
    UI::Notifications::{ToastNotification, ToastNotificationManager},
    Win32::{
        Foundation::{ERROR_INSUFFICIENT_BUFFER, HWND},
        Storage::Packaging::Appx::GetCurrentPackageFullName,
        System::{
            Com::{
                CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
                CoTaskMemAlloc, IPersistFile,
                StructuredStorage::{PROPVARIANT, PROPVARIANT_0_0, PropVariantClear},
            },
            Registry::{
                HKEY, HKEY_CURRENT_USER, KEY_WRITE, REG_DWORD, REG_OPTION_NON_VOLATILE, REG_SZ,
                RegCloseKey, RegCreateKeyExW, RegSetValueExW,
            },
            Variant::VT_LPWSTR,
            WinRT::{ISystemMediaTransportControlsInterop, RO_INIT_SINGLETHREADED, RoInitialize},
        },
        UI::{
            Shell::{
                IShellLinkW,
                PropertiesSystem::{IPropertyStore, PROPERTYKEY},
                SetCurrentProcessExplicitAppUserModelID, ShellLink,
            },
            WindowsAndMessaging::GetForegroundWindow,
        },
    },
    core::{HSTRING, Interface, PCWSTR, PWSTR, factory},
};

/// The identity of the unpackaged (zip) app. The MSIX build has its own, assigned by Windows.
const AUMID: &str = "IdzanMarko.MicroMusicRoom";
const APP_NAME: &str = "Micro Music Room";
/// The icon shown on notifications of the unpackaged app.
const LOGO_PNG: &[u8] = include_bytes!("../ui/icons/logo.png");

/// Whether the process runs with MSIX package identity.
fn is_packaged() -> bool {
    let mut length = 0u32;
    // SAFETY: a null buffer with length 0 only asks how long the name is.
    let result = unsafe { GetCurrentPackageFullName(&mut length, None) };
    // Packaged: the call says the buffer is too small. Unpackaged: APPMODEL_ERROR_NO_PACKAGE.
    result == ERROR_INSUFFICIENT_BUFFER
}

fn data_dir() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|p| PathBuf::from(p).join("MusicRoom"))
}

fn utf16_bytes(text: &str) -> Vec<u8> {
    text.encode_utf16()
        .chain(std::iter::once(0))
        .flat_map(u16::to_le_bytes)
        .collect()
}

fn set_registry_text(key: HKEY, name: &str, value: &str) -> windows::core::Result<()> {
    // SAFETY: `key` is open for writing and the data is a NUL-terminated UTF-16 string.
    unsafe {
        RegSetValueExW(
            key,
            &HSTRING::from(name),
            None,
            REG_SZ,
            Some(&utf16_bytes(value)),
        )
        .ok()
    }
}

/// Tell Windows who this app is, so notifications have a name and an icon and the taskbar groups
/// its windows. Call once at start-up, before the window opens.
///
/// Returns the identity to notify under, or `None` for a packaged app (which needs none).
pub fn init_app_identity() -> Option<HSTRING> {
    if is_packaged() {
        return None;
    }
    let id = HSTRING::from(AUMID);
    // SAFETY: plain call with a valid string.
    let _ = unsafe { SetCurrentProcessExplicitAppUserModelID(&id) };
    let _ = register_notification_identity();
    // Windows names the media card in the volume flyout after the Start Menu entry that carries
    // this identity; without one it says "Unknown app".
    let _ = ensure_start_menu_entry();
    Some(id)
}

/// `System.AppUserModel.ID`, the property that ties a shortcut to an app identity.
const PKEY_APP_USER_MODEL_ID: PROPERTYKEY = PROPERTYKEY {
    fmtid: windows::core::GUID::from_u128(0x9F4C2855_9F79_4B39_A8D0_E1D42DE1D5F3),
    pid: 5,
};

/// A Start Menu shortcut to this exe that carries the app identity. Created only when missing or
/// older than the exe (so moving the exe repairs it), and only in the current user's own menu.
fn ensure_start_menu_entry() -> windows::core::Result<()> {
    let exe = std::env::current_exe().map_err(|_| windows::core::Error::from_thread())?;
    let Some(programs) = std::env::var_os("APPDATA").map(|p| {
        PathBuf::from(p)
            .join("Microsoft")
            .join("Windows")
            .join("Start Menu")
            .join("Programs")
    }) else {
        return Ok(());
    };
    let link = programs.join(format!("{APP_NAME}.lnk"));
    let modified = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    if let (Some(l), Some(e)) = (modified(&link), modified(&exe)) {
        if l >= e {
            return Ok(());
        }
    }
    std::fs::create_dir_all(&programs).map_err(|_| windows::core::Error::from_thread())?;
    // SAFETY: COM calls on the UI thread; every pointer passed is a live NUL-terminated string,
    // and the PROPVARIANT's string is freed by `PropVariantClear`.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let shell_link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
        shell_link.SetPath(&HSTRING::from(exe.as_os_str()))?;
        shell_link.SetIconLocation(&HSTRING::from(exe.as_os_str()), 0)?;
        shell_link.SetDescription(&HSTRING::from(APP_NAME))?;
        if let Some(dir) = exe.parent() {
            shell_link.SetWorkingDirectory(&HSTRING::from(dir.as_os_str()))?;
        }
        let store: IPropertyStore = shell_link.cast()?;
        let units: Vec<u16> = AUMID.encode_utf16().chain(std::iter::once(0)).collect();
        let bytes = units.len() * 2;
        let text = CoTaskMemAlloc(bytes).cast::<u16>();
        if text.is_null() {
            return Err(windows::core::Error::from_thread());
        }
        std::ptr::copy_nonoverlapping(units.as_ptr(), text, units.len());
        let mut value = PROPVARIANT::default();
        value.Anonymous.Anonymous = std::mem::ManuallyDrop::new(PROPVARIANT_0_0 {
            vt: VT_LPWSTR,
            ..PROPVARIANT_0_0::default()
        });
        value.Anonymous.Anonymous.Anonymous.pwszVal = PWSTR(text);
        let result = store
            .SetValue(&PKEY_APP_USER_MODEL_ID, &value)
            .and_then(|()| store.Commit());
        let _ = PropVariantClear(&mut value);
        result?;
        let file: IPersistFile = shell_link.cast()?;
        file.Save(&HSTRING::from(link.as_os_str()), true)
    }
}

/// `HKCU\Software\Classes\AppUserModelId\<id>` with a display name and icon.
fn register_notification_identity() -> windows::core::Result<()> {
    let icon = data_dir().and_then(|dir| {
        std::fs::create_dir_all(&dir).ok()?;
        let path = dir.join("logo.png");
        std::fs::write(&path, LOGO_PNG).ok()?;
        Some(path)
    });
    let subkey = HSTRING::from(format!("Software\\Classes\\AppUserModelId\\{AUMID}"));
    let mut key = HKEY::default();
    // SAFETY: standard registry calls; the key is closed again below.
    unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            &subkey,
            None,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &mut key,
            None,
        )
        .ok()?;
        let result = set_registry_text(key, "DisplayName", APP_NAME)
            .and_then(|()| match &icon {
                Some(path) => set_registry_text(key, "IconUri", &path.to_string_lossy()),
                None => Ok(()),
            })
            // Makes the app appear in Settings > System > Notifications, where it can be muted.
            .and_then(|()| {
                RegSetValueExW(
                    key,
                    &HSTRING::from("ShowInSettings"),
                    None,
                    REG_DWORD,
                    Some(&1u32.to_le_bytes()),
                )
                .ok()
            });
        let _ = RegCloseKey(key);
        result
    }
}

fn timespan(d: Duration) -> windows::Foundation::TimeSpan {
    // TimeSpan counts 100-nanosecond ticks.
    windows::Foundation::TimeSpan {
        Duration: i64::try_from(d.as_nanos() / 100).unwrap_or(i64::MAX),
    }
}

fn from_timespan(t: windows::Foundation::TimeSpan) -> Duration {
    Duration::from_nanos(u64::try_from(t.Duration).unwrap_or(0).saturating_mul(100))
}

/// A cover as something the flyout can show.
fn thumbnail(png: &[u8]) -> windows::core::Result<RandomAccessStreamReference> {
    let stream = InMemoryRandomAccessStream::new()?;
    let writer = DataWriter::CreateDataWriter(&stream)?;
    writer.WriteBytes(png)?;
    writer.StoreAsync()?.join()?;
    writer.FlushAsync()?.join()?;
    writer.DetachStream()?;
    stream.Seek(0)?;
    RandomAccessStreamReference::CreateFromStream(&stream)
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// `file:///C:/a%20b/c.png`
fn file_uri(path: &Path) -> String {
    let mut out = String::from("file:///");
    for byte in path.to_string_lossy().replace('\\', "/").bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' | b':' => {
                out.push(char::from(byte));
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

pub struct WinMedia {
    smtc: SystemMediaTransportControls,
    commands: Receiver<MediaCommand>,
    window: HWND,
    /// `None` when packaged (notifications then need no explicit identity).
    identity: Option<HSTRING>,
    /// Alternates, so a new cover never overwrites a file the shell may still be reading.
    cover_slot: u8,
    /// The last notification problem, for the About box.
    last_toast_error: Option<String>,
    /// Buttons under the taskbar preview; `None` if Windows would not take them.
    thumb: Option<ThumbBar>,
}

impl WinMedia {
    /// Connect to the system media controls for this window. `window` must be the player's real
    /// window handle, so the window must already exist.
    pub fn new(window: HWND, identity: Option<HSTRING>) -> windows::core::Result<Self> {
        // Windows Runtime calls need this thread to have joined an apartment. The window library
        // may or may not have done it; asking twice is harmless (it answers "already done").
        // SAFETY: plain initialisation call on the UI thread.
        let _ = unsafe { RoInitialize(RO_INIT_SINGLETHREADED) };
        let interop =
            factory::<SystemMediaTransportControls, ISystemMediaTransportControlsInterop>()?;
        // SAFETY: `window` is a live top-level window of this process.
        let smtc: SystemMediaTransportControls = unsafe { interop.GetForWindow(window)? };
        smtc.SetIsEnabled(true)?;
        smtc.SetIsPlayEnabled(true)?;
        smtc.SetIsPauseEnabled(true)?;
        smtc.SetIsStopEnabled(true)?;
        smtc.SetIsNextEnabled(true)?;
        smtc.SetIsPreviousEnabled(true)?;

        let (tx, commands) = mpsc::channel();
        let buttons = tx.clone();
        let thumb = ThumbBar::new(window, tx.clone()).ok();
        smtc.ButtonPressed(
            &TypedEventHandler::new(
                move |_,
                      args: windows::core::Ref<
                    SystemMediaTransportControlsButtonPressedEventArgs,
                >| {
                    let command = match args.ok()?.Button()? {
                        SystemMediaTransportControlsButton::Play => Some(MediaCommand::Play),
                        SystemMediaTransportControlsButton::Pause => Some(MediaCommand::Pause),
                        SystemMediaTransportControlsButton::Stop => Some(MediaCommand::Stop),
                        SystemMediaTransportControlsButton::Next => Some(MediaCommand::Next),
                        SystemMediaTransportControlsButton::Previous => {
                            Some(MediaCommand::Previous)
                        }
                        _ => None,
                    };
                    if let Some(command) = command {
                        let _ = buttons.send(command);
                    }
                    Ok(())
                },
            ),
        )?;
        smtc.PlaybackPositionChangeRequested(&TypedEventHandler::new(
            move |_, args: windows::core::Ref<PlaybackPositionChangeRequestedEventArgs>| {
                let to = from_timespan(args.ok()?.RequestedPlaybackPosition()?);
                let _ = tx.send(MediaCommand::Seek(to));
                Ok(())
            },
        ))?;

        Ok(Self {
            smtc,
            commands,
            window,
            identity,
            cover_slot: 0,
            last_toast_error: None,
            thumb,
        })
    }

    fn show_now_playing(&self, now: Option<&NowPlaying>) -> windows::core::Result<()> {
        let updater = self.smtc.DisplayUpdater()?;
        match now {
            None => updater.ClearAll()?,
            Some(now) => {
                updater.SetType(MediaPlaybackType::Music)?;
                let music = updater.MusicProperties()?;
                music.SetTitle(&HSTRING::from(now.title.as_str()))?;
                music.SetArtist(&HSTRING::from(now.artist.as_str()))?;
                music.SetAlbumTitle(&HSTRING::from(now.album.as_str()))?;
                if let Some(png) = &now.cover_png {
                    updater.SetThumbnail(&thumbnail(png)?)?;
                }
            }
        }
        updater.Update()
    }

    fn show_playback(
        &self,
        state: Playback,
        position: Duration,
        length: Option<Duration>,
    ) -> windows::core::Result<()> {
        self.smtc.SetPlaybackStatus(match state {
            Playback::Stopped => MediaPlaybackStatus::Stopped,
            Playback::Playing => MediaPlaybackStatus::Playing,
            Playback::Paused => MediaPlaybackStatus::Paused,
        })?;
        if let (Some(length), true) = (length, state != Playback::Stopped) {
            let timeline = SystemMediaTransportControlsTimelineProperties::new()?;
            timeline.SetStartTime(timespan(Duration::ZERO))?;
            timeline.SetMinSeekTime(timespan(Duration::ZERO))?;
            timeline.SetEndTime(timespan(length))?;
            timeline.SetMaxSeekTime(timespan(length))?;
            timeline.SetPosition(timespan(position.min(length)))?;
            self.smtc.UpdateTimelineProperties(&timeline)?;
        }
        Ok(())
    }

    fn toast(&mut self, now: &NowPlaying, force: bool) -> windows::core::Result<()> {
        // No point notifying about a song the user is looking at.
        // SAFETY: a plain query.
        if !force && unsafe { GetForegroundWindow() } == self.window {
            return Ok(());
        }
        let cover = match (&now.cover_png, data_dir()) {
            (Some(png), Some(dir)) => {
                self.cover_slot ^= 1;
                let path = dir.join(format!("toast-cover-{}.png", self.cover_slot));
                std::fs::create_dir_all(&dir)
                    .and_then(|()| std::fs::write(&path, png))
                    .ok()
                    .map(|()| path)
            }
            _ => None,
        };
        let line = |text: &str| {
            if text.is_empty() {
                String::new()
            } else {
                format!("<text>{}</text>", xml_escape(text))
            }
        };
        let image = cover.as_deref().map_or(String::new(), |p| {
            format!(
                "<image placement=\"appLogoOverride\" src=\"{}\"/>",
                xml_escape(&file_uri(p))
            )
        });
        let xml = format!(
            "<toast duration=\"short\"><visual><binding template=\"ToastGeneric\">{image}{}{}{}</binding></visual><audio silent=\"true\"/></toast>",
            line(&now.title),
            line(&now.artist),
            line(&now.album),
        );
        let doc = XmlDocument::new()?;
        doc.LoadXml(&HSTRING::from(xml))?;
        let toast = ToastNotification::CreateToastNotification(&doc)?;
        // The same tag replaces the previous notification instead of stacking a new one.
        toast.SetTag(&HSTRING::from("now-playing"))?;
        toast.SetGroup(&HSTRING::from("music"))?;
        toast.SetExpiresOnReboot(true)?;
        let notifier = match &self.identity {
            Some(id) => ToastNotificationManager::CreateToastNotifierWithId(id)?,
            None => ToastNotificationManager::CreateToastNotifier()?,
        };
        notifier.Show(&toast)
    }
}

impl MediaSession for WinMedia {
    fn set_now_playing(&mut self, now: Option<&NowPlaying>) {
        // The flyout is a courtesy: if Windows refuses an update, playback carries on.
        let _ = self.show_now_playing(now);
    }

    fn set_playback(&mut self, state: Playback, position: Duration, length: Option<Duration>) {
        if let Some(thumb) = &mut self.thumb {
            thumb.set_playing(state == Playback::Playing);
        }
        let _ = self.show_playback(state, position, length);
    }

    fn take_commands(&mut self) -> Vec<MediaCommand> {
        // Called every few hundred milliseconds, which is also how often the taskbar buttons get
        // another chance to be added.
        if let Some(thumb) = &mut self.thumb {
            thumb.tick();
        }
        self.commands.try_iter().collect()
    }

    fn notify_track_change(&mut self, now: &NowPlaying) {
        self.last_toast_error = self.toast(now, false).err().map(|e| e.to_string());
    }

    fn status(&self) -> String {
        match &self.last_toast_error {
            None => "Connected to the Windows media controls.".into(),
            Some(e) => {
                format!("Connected to the Windows media controls. Last notification problem: {e}")
            }
        }
    }

    fn send_test_notification(&mut self) -> String {
        let now = NowPlaying {
            title: "Micro Music Room".into(),
            artist: "Test notification".into(),
            album: "If you can read this, notifications work.".into(),
            ..NowPlaying::default()
        };
        match self.toast(&now, true) {
            Ok(()) => {
                self.last_toast_error = None;
                "Test notification sent. If nothing appeared, check Settings > System > Notifications and Do Not Disturb.".into()
            }
            Err(e) => {
                let text = format!("Windows refused the notification: {e}");
                self.last_toast_error = Some(e.to_string());
                text
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timespans_count_in_hundred_nanosecond_ticks() {
        assert_eq!(timespan(Duration::from_secs(1)).Duration, 10_000_000);
        assert_eq!(
            from_timespan(timespan(Duration::from_millis(1500))),
            Duration::from_millis(1500)
        );
        assert_eq!(
            from_timespan(windows::Foundation::TimeSpan { Duration: -5 }),
            Duration::ZERO
        );
    }

    #[test]
    fn toast_text_is_escaped_and_paths_become_file_uris() {
        assert_eq!(
            xml_escape("Tom & \"Jerry\" <3"),
            "Tom &amp; &quot;Jerry&quot; &lt;3"
        );
        assert_eq!(
            file_uri(Path::new("C:\\Users\\Marko Idžan\\a.png")),
            "file:///C:/Users/Marko%20Id%C5%BEan/a.png"
        );
    }

    #[test]
    fn the_test_process_is_not_packaged() {
        assert!(!is_packaged());
    }
}
