// Patches the official Wispr Flow bundles so they run on Linux.
// Run with: ELECTRON_RUN_AS_NODE=1 electron patch.js <path/to/.webpack/main/index.js>
// Every patch must match the expected number of times, so an upstream change fails loudly instead of silently.
const fs = require("fs");

const LINUX = '"linux"===process.platform';
const LINUX_RENDERER = '"linux"===window.electron?.platform?.os'; // renderers have no `process`; the preload exposes the OS

// App-type detection matches the focused app (the helper sends its lowercased X11 WM_CLASS) by prefix.
const LINUX_TERMINALS = [
  "gnome-terminal-server", "gnome-terminal", "org.gnome.terminal", "org.gnome.ptyxis", "ptyxis", "konsole", "xterm", "xfce4-terminal",
  "tilix", "terminator", "foot", "alacritty", "kitty", "wezterm", "ghostty", "cosmic-term", "mate-terminal", "lxterminal",
  "qterminal", "guake", "yakuake", "tabby", "warp",
];
const LINUX_EMAIL = ["evolution", "geary", "org.kde.kmail2", "kmail", "claws-mail"];
const LINUX_MESSAGING = ["teams-for-linux", "whatsapp-for-linux", "telegramdesktop", "element"];
const LINUX_DOCUMENTS = ["obsidian", "logseq", "gedit", "xed", "kate", "mousepad", "pluma", "org.gnome.texteditor"];

// --- code injected into the bundle (stringified with .toString(), so it must stay self-contained) ---

// Launch at login on Linux: Electron's setLoginItemSettings is a no-op there, so manage the XDG autostart
// entry the installer writes. linuxAutostart() reports whether it exists, linuxAutostart(bool) sets/removes it.
function linuxAutostart(on) {
  const fs = require("fs"), path = require("path"), os = require("os");
  const file = path.join(process.env.XDG_CONFIG_HOME || path.join(os.homedir(), ".config"), "autostart", "wispr-flow.desktop");
  if (on === undefined) return fs.existsSync(file);
  if (!on) return fs.rmSync(file, { force: true });
  if (fs.existsSync(file)) return;
  const exec = path.join(os.homedir(), ".local", "bin", "wispr-flow").replace(/%/g, "%%"); // same entry as install.sh
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, `[Desktop Entry]\nType=Application\nName=Wispr Flow\nComment=Voice dictation in every app\nExec="${exec}" --autostart\nIcon=${path.join(process.resourcesPath, "wispr-flow.png")}\nTerminal=false\nCategories=Utility;Accessibility;\nStartupWMClass=Wispr Flow\nMimeType=x-scheme-handler/wispr-flow;\n`);
}

// Make the "Launch at login" pref reflect the real autostart entry.
function syncLinuxAutostartPref(prefs, setPrefs, autostart) { // self-contained once stringified, so linuxAutostart is passed in
  const on = autostart();
  if (!!prefs?.user?.openAtLogin !== on) setPrefs({ openAtLogin: on });
}

// Shortcuts saved on macOS-style defaults use keycode -1 (Fn/Option/Cmd), which no Linux key produces.
// Replace each with the default for the same action unless that default is already taken.
function migrateLinuxShortcuts(app, setPrefs, shortcutsModule, sendHelperRequest) {
  const user = app.prefs?.user;
  const stale = (combo) => combo.split("+").includes("-1");
  const strip = (map) => Object.fromEntries(Object.entries(map ?? {}).filter(([combo]) => !stale(combo)));
  const dropped = Object.entries(user?.shortcuts ?? {}).filter(([combo]) => stale(combo)).map(([, action]) => action);
  if (!dropped.length) return;
  const shortcuts = strip(user.shortcuts);
  const defaults = { ...shortcutsModule.Au, ...shortcutsModule.Qs({}) }; // Qs({}) = the open_meeting_recorder default
  for (const [combo, action] of Object.entries(defaults)) {
    if (dropped.includes(action) && !(combo in shortcuts) && !Object.values(shortcuts).includes(action)) shortcuts[combo] = action;
  }
  const update = { shortcuts };
  for (const key of ["stashedScratchpadShortcuts", "stashedPolishShortcuts", "stashedMeetingRecorderShortcuts"]) {
    if (Object.keys(user[key] ?? {}).some(stale)) update[key] = strip(user[key]);
  }
  setPrefs(update);
  if (app.helper?.ready) sendHelperRequest("UpdateShortcuts");
}

// Electron's screen.getCursorScreenPoint() on X11 only updates while the pointer is over one of the app's windows
// that accepts input. The floating bar ignores input until its hit-test poll sees the pointer on it, so that poll
// read a frozen position and the bar and its popups were never clickable. Read the real position from the helper.
function linuxCursorTracker() {
  const { app, screen } = require("electron"), { spawn } = require("child_process");
  if (process.env.XDG_SESSION_TYPE === "wayland" || !process.env.DISPLAY) return; // X11 only: Wayland hides the global pointer
  const original = screen.getCursorScreenPoint.bind(screen);
  let point = null, child = null, quitting = false;
  screen.getCursorScreenPoint = () => point ?? original();
  const toDip = (p) => {
    if (screen.screenToDipPoint) return screen.screenToDipPoint(p);
    const scale = screen.getPrimaryDisplay().scaleFactor || 1;
    return { x: Math.round(p.x / scale), y: Math.round(p.y / scale) };
  };
  const start = () => {
    let buffer = "";
    child = spawn(process.env.WISPR_LINUX_HELPER || "wispr-flow-linux-helper", ["--cursor"], { stdio: ["ignore", "pipe", "ignore"] });
    child.stdout.on("data", (chunk) => {
      const lines = (buffer + chunk).split("\n");
      buffer = lines.pop();
      const [x, y] = (lines.pop() ?? "").split(" ").map(Number);
      if (Number.isFinite(x) && Number.isFinite(y)) point = toDip({ x, y });
    });
    child.on("error", () => {}); // "exit" follows; without the tracker Electron's own (frozen) value is used
    child.on("exit", () => { point = null; if (!quitting) setTimeout(start, 2000); });
  };
  app.on("before-quit", () => { quitting = true; child?.kill(); });
  start();
}

// On a Wayland session the app runs under XWayland (the launcher passes --ozone-platform=x11): native Wayland gives
// apps no pointer position and no way to make part of a window click-through. Under XWayland a window that ignores
// input gets no pointer events at all, so the cursor poll would never see the pointer arrive. Instead of ignoring
// input, cut every overlay window (the ones that ask to ignore the mouse) down to its visible pixels: those take
// hover and clicks, everything transparent falls through to the window below.
function linuxAlphaShape() {
  const { BrowserWindow } = require("electron");
  if (process.env.XDG_SESSION_TYPE !== "wayland" || !process.env.DISPLAY) return;
  const CELL = 8, tracked = new Map();
  // visible pixels of a BGRA bitmap as rectangles on a CELL grid, in window (DIP) coordinates
  const rectsOf = (bitmap, width, height, scale) => {
    const cols = Math.ceil(width / CELL), rows = Math.ceil(height / CELL), open = new Map(), rects = [];
    const filled = (cx, cy) => {
      for (let y = cy * CELL; y < Math.min((cy + 1) * CELL, height); y += 2)
        for (let x = cx * CELL; x < Math.min((cx + 1) * CELL, width); x += 2) if (bitmap[(y * width + x) * 4 + 3] > 8) return true;
      return false;
    };
    for (let cy = 0; cy <= rows; cy++) {
      const runs = new Map();
      for (let cx = 0; cy < rows && cx < cols; cx++) {
        if (!filled(cx, cy)) continue;
        const start = cx;
        while (cx + 1 < cols && filled(cx + 1, cy)) cx++;
        runs.set(`${start}:${cx}`, [start, cx]);
      }
      for (const [key, rect] of open) if (!runs.has(key)) { rects.push(rect); open.delete(key); } // a run ended
      for (const [key, [a, b]] of runs) {
        if (open.has(key)) open.get(key).height += CELL;
        else open.set(key, { x: a * CELL, y: cy * CELL, width: (b - a + 1) * CELL, height: CELL });
      }
    }
    return rects.map((r) => ({ x: Math.floor(r.x / scale), y: Math.floor(r.y / scale), width: Math.ceil(r.width / scale), height: Math.ceil(r.height / scale) }));
  };
  const update = async (win) => {
    if (win.isDestroyed() || !win.isVisible()) return;
    try {
      const image = await win.webContents.capturePage(), { width, height } = image.getSize();
      if (!width || !height || win.isDestroyed()) return;
      const rects = rectsOf(image.toBitmap(), width, height, width / win.getContentBounds().width);
      const key = JSON.stringify(rects);
      if (key === tracked.get(win).last) return;
      tracked.get(win).last = key;
      win.setShape(rects.length ? rects : [{ x: 0, y: 0, width: 1, height: 1 }]); // nothing visible: nothing to click
    } catch {} // the page is loading or gone; the next tick retries
  };
  BrowserWindow.prototype.setIgnoreMouseEvents = function () {
    if (tracked.has(this)) return;
    tracked.set(this, { last: "", timer: setInterval(() => update(this), 150) });
    this.once("closed", () => { clearInterval(tracked.get(this).timer); tracked.delete(this); });
  };
}

// Name the webpack module alias bound to module `id` (e.g. `ri` for `ri=n(69583)`), nearest one before `offset`.
function moduleAlias(src, offset, id) {
  const hits = [...src.slice(0, offset).matchAll(new RegExp(`[,;\\s]([a-zA-Z$]+)=n\\(${id}\\)`, "g"))];
  if (!hits.length) throw new Error(`no alias for webpack module ${id} before offset ${offset}`);
  return hits.at(-1)[1];
}
const MODULE = { prefsStore: 69583, prefsSetter: 86279, helper: 26181, shortcuts: 28889 };

// First available sound settings app (shell snippet, run with `sh -c`).
const OPEN_SOUND_SETTINGS =
  'for c in "gnome-control-center sound" "cinnamon-settings sound" "systemsettings kcm_pulseaudio" pavucontrol; do ' +
  'command -v "${c%% *}" >/dev/null 2>&1 && exec $c; done';

// Default shortcut tables are `H8 ? windows : mac`; Linux must take the Windows branch (its keycodes are Windows VK codes,
// which the helper sends). `tD ? mac : win` ternaries (keycodes, app lists) already fall to the Windows side on Linux.
// One regex covers every `x.H8?a:b` identifier ternary, i.e. the default/PTT/polish/meeting tables.
const shortcutDefaults = (count, linux) => ({
  name: "use the Windows default shortcuts on Linux",
  count,
  re: /([a-zA-Z$]+\.H8)(\?[a-zA-Z$]+:[a-zA-Z$]+[,;)])/,
  to: (_, h, rest) => `(${h}||${linux})${rest}`,
});

const patches = [
  {
    name: "skip macOS Applications-folder gate",
    re: /(k=async\(\)=>\{if\()([a-zA-Z$]+\.H8)(\)return!1;const e=[a-zA-Z$]+\.app\.getAppPath\(\))/,
    to: (_, a, h, b) => `${a}${h}||${LINUX}${b}`,
  },
  {
    name: "use the Linux helper binary",
    re: /`\$\{[a-zA-Z$]+\.ZI\}\\\\Release\\\\Wispr Flow Helper\.exe`/,
    to: () => `(process.env.WISPR_LINUX_HELPER||"wispr-flow-linux-helper")`,
  },
  {
    name: "pass the session environment (DISPLAY, WAYLAND_DISPLAY, ...) to the helper",
    re: /(stdio:\["pipe","pipe","pipe","pipe"\],env:)([a-zA-Z$]+\(\))/,
    to: (_, a, env) => `${a}{...process.env,...${env}}`,
  },
  {
    name: "accept wispr-flow:// sign-in deep links from argv (Windows-only upstream)",
    count: 2,
    re: /([a-zA-Z$]+\.H8)(\)\{const e=[a-zA-Z$]+\((?:process\.argv|t)\.find\(e=>e\.startsWith\("wispr-flow:"\))/,
    to: (_, h, rest) => `${h}||${LINUX}${rest}`,
  },
  {
    name: "native window frame for the main window on Linux (renderer draws controls only on macOS/Windows)",
    re: /(trafficLightPosition:\(0,[a-zA-Z$]+\.xC\)\("hub",[^;]{0,120}?\):Object\.assign\(t,\{frame:)!1(,autoHideMenuBar:!0\}\))/,
    to: (_, a, b) => `${a}${LINUX}${b}`,
  },
  {
    name: "native window frame for the secondary window on Linux",
    re: /(trafficLightPosition:\{x:12,y:16\}\}\):Object\.assign\(t,\{titleBarStyle:)"hidden"(,autoHideMenuBar:!0\}\))/,
    to: (_, a, b) => `${a}${LINUX}?"default":"hidden"${b}`,
  },
  {
    name: "create the main window focusable on Linux (window managers skip the frame otherwise)",
    re: /(focusable:)!1(\};[a-zA-Z$]+\.tD\?Object\.assign\(t,\{frame:!1,titleBarStyle:"hidden",trafficLightPosition:\(0,[a-zA-Z$]+\.xC\)\("hub")/,
    to: (_, a, b) => `${a}${LINUX}${b}`,
  },
  shortcutDefaults(8, LINUX),
  {
    name: "Linux startup: track the real cursor position, migrate saved shortcuts that use macOS-only keycode -1",
    re: /(\(0,[a-zA-Z$]+\.e6\)\(\)),(?=[a-zA-Z$]+\.[a-zA-Z$]+\.startCheckingForUpdates\(\))/,
    to: (_, call, offset, src) => {
      const alias = (id) => moduleAlias(src, offset, id);
      const args = `${alias(MODULE.prefsStore)}.RA,${alias(MODULE.prefsSetter)}.xB,${alias(MODULE.shortcuts)},${alias(MODULE.helper)}.sendHelperRequest`;
      return `${call},${LINUX}&&((${linuxCursorTracker})(),(${linuxAlphaShape})(),(${migrateLinuxShortcuts})(${args})),`;
    },
  },
  {
    name: "launch at login: first-run auto-enable writes the XDG autostart entry on Linux",
    re: /([a-zA-Z$]+\.app\.setLoginItemSettings\(\{openAtLogin:)!0(\}\))/,
    to: (_, a, b) => `${LINUX}?(${linuxAutostart})(!0):${a}!0${b}`,
  },
  {
    name: "launch at login: the settings toggle adds/removes the XDG autostart entry on Linux",
    re: /([a-zA-Z$]+\.app\.setLoginItemSettings\(\{openAtLogin:)e(\}\))/,
    to: (_, a, b) => `${LINUX}?(${linuxAutostart})(e):${a}e${b}`,
  },
  {
    name: "launch at login: the toggle shows the real autostart state",
    re: /(uf=e=>\{)(dr\.H8&&)/,
    to: (_, a, b, offset, src) => `${a}${LINUX}&&(${syncLinuxAutostartPref})(e,${moduleAlias(src, offset, MODULE.prefsSetter)}.xB,${linuxAutostart});${b}`,
  },
  {
    name: "launched at login: start hidden on Linux when started by the autostart entry (as on Windows)",
    re: /([a-zA-Z$]+\.H8)(&&[a-zA-Z$]+\.RA\.prefs\?\.user\?\.openAtLogin&&[a-zA-Z$]+\.RA\.prefs\.user\.onboardingCompleted\?\{show:!1)/,
    to: (_, h, rest) => `(${h}||${LINUX}&&process.argv.includes("--autostart"))${rest}`,
  },
  {
    name: "recognize Linux terminals (TERMINAL_APPS)",
    re: /("hyper","kitty","wezterm")(\])/,
    to: (_, a, b) => `${a},...(${LINUX}?${JSON.stringify(LINUX_TERMINALS)}:[])${b}`,
  },
  {
    name: "recognize Linux email clients",
    re: /("rambox","wavebox","notion mail")(\])/,
    to: (_, a, b) => `${a},...(${LINUX}?${JSON.stringify(LINUX_EMAIL)}:[])${b}`,
  },
  {
    name: "recognize Linux messaging apps",
    re: /("nachrichten","微信","weixin")(\])/,
    to: (_, a, b) => `${a},...(${LINUX}?${JSON.stringify(LINUX_MESSAGING)}:[])${b}`,
  },
  {
    name: "recognize Linux document/notes apps",
    re: /("anteckningar","заметки")(\])/,
    to: (_, a, b) => `${a},...(${LINUX}?${JSON.stringify(LINUX_DOCUMENTS)}:[])${b}`,
  },
  {
    name: "open the sound settings of the Linux desktop",
    re: /("darwin"===e\?\{type:"external",url:`x-apple\.systempreferences:com\.apple\.preference\.sound\?\$\{t\}`\}):null\)/,
    to: (_, mac) => `${mac}:${LINUX}?{type:"process",command:"sh",args:["-c",${JSON.stringify(OPEN_SOUND_SETTINGS)}]}:null)`,
  },
  {
    name: "tray icon click opens the app on Linux",
    re: /([a-zA-Z$]+\.H8)(&&[a-zA-Z$]+\.on\("click",\(\)=>[a-zA-Z$]+\("left_click_open"\)\))/,
    to: (_, h, rest) => `(${h}||${LINUX})${rest}`,
  },
  // Meeting auto-detect: the helper reports microphone holders from PipeWire/PulseAudio, as Windows does.
  // Only the master gate and the mic-holder monitor are opened; tab-strip/speaker features stay off (they need UI automation).
  {
    name: "meeting auto-detect: allow detection on Linux",
    re: /(=\(\)=>\()("darwin"===process\.platform\|\|"win32"===process\.platform)(\)&&!\(0,[a-zA-Z$]+\.kJ\)\(\)&&"unsupported"!==)/,
    to: (_, a, gate, b) => `${a}${gate}||${LINUX}${b}`,
  },
  {
    name: "meeting auto-detect: subscribe/unsubscribe the microphone holders monitor on Linux",
    count: 2,
    re: /"darwin"!==process\.platform&&"win32"!==process\.platform(?=\|\|(?:null===[a-zA-Z$]+\|\|![a-zA-Z$]+\.isEnabled\(\)\)return;const [a-zA-Z$]+=\+\+|\(0,[a-zA-Z$]+\.setMicrophoneHoldersMonitor\)))/,
    to: (gate) => `${gate}&&!(${LINUX})`,
  },
  // Slack on the microphone: Wispr asks the helper whether it is a huddle (GetSlackHuddleState). On Linux the helper
  // answers "huddle" for Slack's pop-out huddle window and "unknown" otherwise, which Wispr treats as a call, as before.
  {
    name: "slack huddle: ask the helper on Linux",
    re: /("darwin"!==process\.platform&&"win32"!==process\.platform)(\)return [a-zA-Z$]+;const [a-zA-Z$]+=await [a-zA-Z$]+\("GetSlackHuddleState")/,
    to: (_, gate, rest) => `${gate}&&!(${LINUX})${rest}`,
  },
  {
    name: "slack huddle: do not bypass the huddle read as an unsupported platform on Linux",
    re: /("win32"!==[a-zA-Z$]+\.platform)(\?"platform_unsupported":[a-zA-Z$]+\.recordingActiveOrStopping)/,
    to: (_, gate, rest) => `${gate}&&!(${LINUX})${rest}`,
  },
  // Wispr skips system audio on Linux ("no system loopback path"), so a meeting recording fails with "Could not capture
  // system audio" and has no other participants. Electron does loop back the default output's PulseAudio/PipeWire monitor.
  {
    name: "meeting recorder: capture system audio on Linux like on Windows",
    re: /if\("linux"===process\.platform\)return [a-zA-Z$]+\(\)\.info\("\[MeetingDisplayMedia\] Skipping handler install on Linux[^"]*"\),void\([a-zA-Z$]+=!0\);([^;]{0,120}\{if\()("win32"===process\.platform)(\)return void [a-zA-Z$]+\(\{audio:"loopback"\}\))/,
    to: (_, a, gate, b) => `${a}${gate}||${LINUX}${b}`,
  },
  // Linux has no setIgnoreMouseEvents({forward}), so the floating bar is clickable only through the cursor poll
  // (ignoreMouseEventsWhenNotInAlphaRegion); poll faster than the default 400ms so the bar reacts when the pointer arrives.
  {
    name: "poll the floating bar hit-test every 100ms on Linux",
    re: /(\([a-zA-Z$]+=\(0,[a-zA-Z$]+\.Bi\)\(e)(\)\)\)\},[a-zA-Z$]+=\(\)=>\{[a-zA-Z$]+\?\.stop\(\))/,
    to: (_, a, b) => `${a},0,${LINUX}?100:400${b}`,
  },
];

// Renderer bundles carry their own copy of the shortcut tables (platform flags come from the preload).
// Expected number of `H8?a:b` default-shortcut ternaries per bundle.
const rendererShortcutDefaults = { hub: 9, status: 5, contextMenu: 3, scratchpad: 2, meeting_recorder: 2 };

function applyPatches(src, list = patches) {
  for (const p of list) {
    const re = new RegExp(p.re.source, "g");
    const hits = src.match(re) || [];
    const want = p.count ?? 1;
    if (hits.length !== want) {
      throw new Error(`patch "${p.name}": expected ${want} match(es), found ${hits.length} (Wispr Flow changed upstream; please open an issue)`);
    }
    src = src.replace(re, p.to);
    console.log(`ok: ${p.name}`);
  }
  return src;
}

// Chromium's default audio processing (echo cancellation, noise suppression) silences the Linux loopback stream
// completely; the hub window captures the meeting's system audio, so ask for the raw stream there.
const rendererExtra = {
  hub: [{
    name: "meeting recorder: raw system audio stream on Linux",
    count: 2,
    re: /getDisplayMedia\(\{audio:!0,video:!1\}\)/,
    to: () => `getDisplayMedia({audio:${LINUX_RENDERER}?{echoCancellation:!1,noiseSuppression:!1,autoGainControl:!1}:!0,video:!1})`,
  }],
};

function applyRendererPatches(src, bundle) {
  const list = [shortcutDefaults(rendererShortcutDefaults[bundle], LINUX_RENDERER), ...(rendererExtra[bundle] ?? [])];
  return applyPatches(src, list.map((p) => ({ ...p, name: `${p.name} (${bundle} renderer)` })));
}

module.exports = { applyPatches, applyRendererPatches, linuxAlphaShape, rendererBundles: Object.keys(rendererShortcutDefaults) };

if (require.main === module) {
  const file = process.argv[2];
  fs.writeFileSync(file, applyPatches(fs.readFileSync(file, "utf8")));
}
