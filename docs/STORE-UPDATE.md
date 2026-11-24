# Updating Micro Music Room on the Microsoft Store (v1.0.3.0 to v2)

This is the checklist for replacing the Store version with v2. The package uses the **same identity as v1**
(`IdzanMarko.MicroMusicRoom`, publisher `CN=FB162C28-AD03-40AA-9AFC-26E690AFDEDF`, taken from v1's own
manifest), so the Store sees v2 as an update, and people who have v1 get it through normal Store updates.

Partner Center's screens change from time to time; the names below are the ones it has used, so if one
looks different, look for the closest match.

## 1. Build the package (no tools needed on your PC)

1. On GitHub open the repository, then **Actions**, then the latest **MSIX** run (it runs whenever the app
   code changes; it can also be started with *Run workflow*).
2. Download the two artifacts at the bottom of the run:
   - **`MicroMusicRoom-v2-MSIX-Store-upload`**: `MicroMusicRoom-v2.0.0.0-x64-Store.msix`. This is the one for the Store.
     It is unsigned on purpose; Microsoft signs it after certification (v1 was submitted the same way).
   - **`MicroMusicRoom-v2-MSIX-Test-install`**: a test copy for trying the packaged app first (step 2).

The version comes from `Cargo.toml` (`2.0.0` becomes `2.0.0.0`). The Store needs it to be **higher than the
version already published (1.0.3.0)** and the last number to be **0**. For every later update, raise the version
in `Cargo.toml` (for example `2.0.1`) and build again.

## 2. Try it on a real PC first (recommended)

The test package has a **different identity** (`...MicroMusicRoomTest`, shown as "Micro Music Room (Test)"), so
it installs **next to** your Store v1 and does not touch it or its library.

1. Unzip the *Test-install* artifact into a folder.
2. Right-click `Install-Test.ps1`, choose **Run with PowerShell**, answer **Yes** to the admin prompt. It trusts a
   throw-away test certificate and installs the package.
3. Start **Micro Music Room (Test)** from the Start Menu and check, as packaged:
   playback, the media flyout and keys, the song-change notification (it should say "Micro Music Room" with no
   registry or Start Menu tricks), the taskbar buttons and the Volume Mixer slider.
4. Remove it afterwards: Settings, Apps, uninstall "Micro Music Room (Test)", and delete "Micro Music Room test
   signing" from `certmgr.msc` under *Trusted People* (Local Computer).

Do **not** try to sideload the Store-identity package over your Store v1: Windows refuses (different signer),
and uninstalling v1 would delete its library (see "Your library" below).

## 3. Submit the update in Partner Center

1. Go to Partner Center, **Apps and games**, **Micro Music Room**, then **Update** (start a new submission).
2. **Packages**: drag in `MicroMusicRoom-v2.0.0.0-x64-Store.msix`. It must show identity
   `IdzanMarko.MicroMusicRoom` and publisher `CN=FB162C28-AD03-40AA-9AFC-26E690AFDEDF`; if Partner Center reports a
   mismatch, compare with *Product management, Product identity*. Remove the old 1.0.3.0 package from this
   submission if the page offers to.
3. **Store listings**: update the description, screenshots and the **What's new** text (see the draft below).
4. **Properties** and **Age ratings**: re-check them; nothing changes in what the app does with data: it is local only.
   Keep the privacy statement consistent: no accounts, no telemetry, no network use.
5. **Submit**. Certification usually takes from a few hours to a few days. When it passes, Store users get v2 as an update.

Draft *What's new*:

> Micro Music Room 2 is rebuilt from the ground up in Rust: smaller, faster and lighter. Your library and
> playlists carry over. New: media keys and headset buttons, a Windows media flyout with cover art, song-change
> notifications (can be turned off), previous / play-pause / next buttons on the taskbar preview, and one
> shared volume with the Windows Volume Mixer.

## Your library across the update

Windows redirects a packaged desktop app's writes under `%LOCALAPPDATA%` to a private per-app folder
(`%LOCALAPPDATA%\Packages\IdzanMarko.MicroMusicRoom_<id>\LocalCache\Local\MusicRoom\`). A Store update keeps
that folder, and v2 reads `MusicRoom\library.json` through the same path as v1, so playlists carry over.
v2 never rewrites the file on start-up, and when it saves it keeps backups (`library.v1.json.bak`).

- **Uninstalling** the Store app deletes that private folder, library included. Back up `library.json` first.
- Before the very first public update, copy that `library.json` somewhere safe as a precaution.
- If you also used the portable zip, its data is in the real `%LOCALAPPDATA%\MusicRoom`, separate from the Store copy.

## Later updates

1. Raise the version in `Cargo.toml`, push to `main`.
2. Download the new *Store-upload* artifact from the MSIX run.
3. Partner Center, Update, upload, edit *What's new*, Submit.

## Notes

- Windows 11 only (`MinVersion 10.0.22000`), 64-bit, like v1's listing. To support Windows 10 too, lower
  `MinVersion` in `msix/AppxManifest.xml` (for example `10.0.19041.0`) before building.
- Unlike the zip, the package does not create the Start Menu shortcut or registry entries: package identity gives
  notifications and the flyout their name and icon already.
- The exe inside the package is the same one as in the zip; the portable zip stays on GitHub Releases.
