# Wispr Flow for Linux

Run the **official [Wispr Flow](https://wisprflow.ai) app on Linux**. You get the same minimal UI and floating dictation bar, the same AI transcription and formatting, and you use **your existing Wispr Flow account and subscription**.

Wispr only ships macOS and Windows builds. The desktop app is built on Electron, so its UI and everything that talks to Wispr's servers already runs on Linux. The missing piece is the small native helper that listens for the dictation shortcut and types text into other apps (Swift on macOS, .NET on Windows). This project fills that gap:

- **`helper/`**: a Rust helper for Linux that speaks Wispr's helper protocol.
  - It reports key presses; Wispr matches its own shortcuts.
  - It pastes dictated text into the focused app.
  - It reports which app is focused, so per-app styles keep working.
  - It ships as a static binary for X11 and Wayland.
- **`patcher/`**: four small, verified patches to the app's main bundle. They skip the macOS "move to Applications" check, launch the Linux helper, pass your desktop session's environment to it, and accept `wispr-flow://` sign-in links.
- **`install.sh`**: downloads the latest official macOS release from wisprflow.ai **on your machine**, unpacks it, patches it, adds Linux builds of Electron and SQLite, and installs a launcher, a menu entry, autostart and the `wispr-flow://` URL handler.

**This repository and its releases contain no Wispr code.** The installer downloads the app from Wispr's own server, the same way you would on a Mac.

## Install

One line, from any shell (bash, zsh, fish):

```bash
curl -fsSL https://raw.githubusercontent.com/cufelix/wispr-flow-linux/main/install.sh | bash
```

or from a clone:

```bash
git clone https://github.com/cufelix/wispr-flow-linux && cd wispr-flow-linux && ./install.sh
```

The installer detects your package manager (apt, dnf, pacman, zypper) and installs what it needs, asking for your password once. Tested in clean containers on Debian 12/13, Ubuntu 22.04/24.04, Linux Mint 22, Arch, Fedora, openSUSE Tumbleweed and Leap 15.6, run as a normal sudo user from both bash and zsh (`tests/distro-test.sh`). That covers their derivatives too (Pop!_OS, Zorin, elementary, Manjaro, EndeavourOS, Nobara...). Builds exist for x86_64 and arm64.

Then start **Wispr Flow** from your app menu (or run `wispr-flow`), click **Sign in via browser**; the browser hands the login back to the app through `wispr-flow://`. Hold the shortcut (default **Ctrl+Super**), speak, release.

- **Update**: run the installer again. It always pulls Wispr's latest release; if an update ever changes the code a patch targets, it stops with a clear error and leaves your current install untouched.
- **Uninstall**: `./install.sh uninstall` (or `curl -fsSL https://raw.githubusercontent.com/cufelix/wispr-flow-linux/main/install.sh | bash -s uninstall`). Your Wispr settings stay in `~/.config/Wispr Flow`.
- **Helper binary**: built with `cargo` if you have Rust, otherwise a prebuilt static binary is downloaded from this repo's Releases and its SHA-256 is verified.

### Wayland

On X11 everything works without extra permissions. On Wayland, the helper listens for keys and sends Ctrl+V through the kernel input layer, so you need to grant your user access once:

```bash
sudo usermod -aG input $USER
echo 'KERNEL=="uinput", GROUP="input", MODE="0660"' | sudo tee /etc/udev/rules.d/70-wispr-flow-uinput.rules
sudo udevadm control --reload && sudo udevadm trigger   # then log out and back in
```

## What works

| Feature | Linux |
|---|---|
| Sign-in, subscription, settings, history, dictionary, styles | ✅ the original app |
| Push-to-talk and hands-free dictation with Wispr's cloud models | ✅ |
| Inserting text into any app | ✅ via clipboard + paste. On X11, terminals get Ctrl+Shift+V; on Wayland every app gets Ctrl+V |
| Per-app context (name of the focused app) | ✅ X11, ❌ Wayland |
| Reading text around the cursor, editing selected text, meeting detection, Jabra/BLE headsets | ❌ these depend on macOS/Windows accessibility APIs |
| Built-in auto-update | ❌ run `./install.sh` again |

## How it works

```
Wispr Flow (Electron, original UI) ──stdin──▶ wispr-flow-linux-helper
                                   ◀──fd 3──  KeypressEvent / PasteOutcome / AppInfoUpdate
```

- **Message format**: JSON, with `+` escaped as `+1` and `|` as `+2`; each message ends with `|`.
- **Key codes**: key presses are sent as Windows virtual-key codes, because Wispr uses those on every platform except macOS.
- **Requests**: the helper answers `IsReady`, `PasteText`, `GetActiveAppInfo` and similar requests. Requests that only make sense on macOS or Windows get a plain `ACK`.

## Offline mode (no subscription)

`offline/` contains a separate, fully local alternative. Hold Right Ctrl, speak, and release. Transcription runs on your GPU with whisper.cpp (`large-v3-turbo`) via Vulkan. Install it with `offline/install.sh`.

## Disclaimer

This is an unofficial community project. It is not affiliated with or endorsed by Wispr AI, Inc., and "Wispr Flow" is their trademark. You need your own Wispr Flow account, and Wispr's terms govern your use of the app. This repository is MIT-licensed (see `LICENSE`); the license covers only the code in this repository.
