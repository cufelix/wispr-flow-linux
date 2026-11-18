// Patches the official Wispr Flow main bundle so it runs on Linux.
// Run with: ELECTRON_RUN_AS_NODE=1 electron patch.js <path/to/.webpack/main/index.js>
// Every patch must match exactly once, so an upstream change fails loudly instead of silently.
const fs = require("fs");

const LINUX = '"linux"===process.platform';
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
];

function applyPatches(src) {
for (const p of patches) {
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

module.exports = { applyPatches };

if (require.main === module) {
  const file = process.argv[2];
  fs.writeFileSync(file, applyPatches(fs.readFileSync(file, "utf8")));
}
