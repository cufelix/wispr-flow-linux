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
    name: "migrate saved shortcuts that use macOS-only keycode -1 (once the prefs are loaded)",
    re: /(\(0,[a-zA-Z$]+\.e6\)\(\)),(?=[a-zA-Z$]+\.[a-zA-Z$]+\.startCheckingForUpdates\(\))/,
    to: (_, call, offset, src) => {
      const alias = (id) => moduleAlias(src, offset, id);
      const args = `${alias(MODULE.prefsStore)}.RA,${alias(MODULE.prefsSetter)}.xB,${alias(MODULE.shortcuts)},${alias(MODULE.helper)}.sendHelperRequest`;
      return `${call},${LINUX}&&(${migrateLinuxShortcuts})(${args}),`;
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

function applyRendererPatches(src, bundle) {
  return applyPatches(src, [shortcutDefaults(rendererShortcutDefaults[bundle], LINUX_RENDERER)].map((p) => ({ ...p, name: `${p.name} (${bundle} renderer)` })));
}

module.exports = { applyPatches, applyRendererPatches, rendererBundles: Object.keys(rendererShortcutDefaults) };

if (require.main === module) {
  const file = process.argv[2];
  fs.writeFileSync(file, applyPatches(fs.readFileSync(file, "utf8")));
}
