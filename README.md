# Vero for desktop

Vero — AI stock intelligence for everyday investors — in its own window on
macOS, Windows and Linux. It is the same account, watchlist and subscription as
[verostocks.com](https://www.verostocks.com): the app is a native window onto
the site, so every feature arrives the moment it ships on the web.

**Download:** [verostocks.com/download](https://www.verostocks.com/download), or
pick a file from the [latest release](https://github.com/OliverYuann/vero-desktop/releases/latest).

| Platform | File | Notes |
| --- | --- | --- |
| macOS 10.15+ (Apple silicon and Intel) | `Vero_<version>_universal.dmg` | One universal build. |
| Windows 10/11 (64-bit) | `Vero_<version>_x64-setup.exe` (or the `.msi`) | Installs for the current user. |
| Linux (x64) | `Vero_<version>_amd64.AppImage` (or the `.deb`) | Needs glibc 2.39+ (Ubuntu 24.04 or newer). |

## These builds are not code-signed yet

Your operating system will warn you the first time you open Vero. That is
because the installers are not yet signed with a paid developer certificate,
not because anything is wrong with them. Verify any download against
`checksums.txt` in the release.

- **macOS:** open the `.dmg` and drag Vero to Applications. The first time you
  open it, macOS says it cannot verify the app. Click **Done**, then open
  **System Settings → Privacy & Security**, scroll down to the message about
  Vero and click **Open Anyway**. You only do this once.
- **Windows:** SmartScreen shows **"Windows protected your PC"**. Click
  **More info → Run anyway**.
- **Linux:** make the AppImage executable (`chmod +x Vero_*.AppImage`) and run
  it, or install the `.deb`.

## What the app adds

A menu-bar/tray Quick View, a global shortcut that brings Vero forward with the
command palette open, companies and tickers in their own windows, `vero://`
links, your last watchlist and alerts shown instantly (and labelled with their
time when you are offline), and your session kept in the system keychain.

Sign in with your email and password. Nothing secret is compiled into the app:
your data stays protected server-side exactly as it is on the web.

## Source

`desktop/` is the [Tauri 2](https://tauri.app) shell, published here so the
installers are built in the open from this repository by GitHub Actions
(`.github/workflows/release.yml`). It is mirrored from Vero's private
repository; issues and pull requests here are not monitored — contact us via
[verostocks.com](https://www.verostocks.com) instead.

Vero provides information, not financial advice. © Vero. All rights reserved.
