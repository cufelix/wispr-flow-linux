// Turns the official macOS Wispr Flow bundle into a Linux Electron app directory.
// Run with: ELECTRON_RUN_AS_NODE=1 <electron> prepare.js <Wispr Flow.app/Contents/Resources> <electron/resources>
// Uses Electron's own Node, so no separate Node/npm install is needed.
const fs = require("original-fs"); // plain fs without Electron's transparent .asar handling
const path = require("path");
const { applyPatches } = require("./patch.js");

const [srcRes, outRes] = process.argv.slice(2);
if (!srcRes || !outRes) throw new Error("usage: prepare.js <mac Resources dir> <electron resources dir>");

// --- asar: [u32 4][u32 headerSize][u32 ...][u32 jsonLen][json] then file data at 8 + headerSize
function extractAsar(archive, dest) {
  const buf = fs.readFileSync(archive);
  const headerSize = buf.readUInt32LE(4);
  const jsonLen = buf.readUInt32LE(12);
  const header = JSON.parse(buf.toString("utf8", 16, 16 + jsonLen));
  const base = 8 + headerSize;
  const walk = (node, rel) => {
    for (const [name, entry] of Object.entries(node.files || {})) {
      const r = path.join(rel, name);
      if (r.split(path.sep).includes("..")) throw new Error(`unsafe path in asar: ${r}`);
      const out = path.join(dest, r);
      if (entry.files) {
        fs.mkdirSync(out, { recursive: true });
        walk(entry, r);
      } else if (entry.link) {
        fs.symlinkSync(entry.link, out);
      } else if (entry.unpacked) {
        fs.copyFileSync(path.join(`${archive}.unpacked`, r), out);
      } else {
        const start = base + Number(entry.offset);
        fs.writeFileSync(out, buf.subarray(start, start + entry.size), { mode: entry.executable ? 0o755 : 0o644 });
      }
    }
  };
  fs.mkdirSync(dest, { recursive: true });
  walk(header, "");
}

// --- icns: sequence of [4-byte type][u32be length incl. header][data]; big icons are embedded PNGs
function largestIcnsPng(file) {
  const buf = fs.readFileSync(file);
  let best = null;
  for (let off = 8; off + 8 <= buf.length; ) {
    const len = buf.readUInt32BE(off + 4);
    if (len < 8) break;
    const data = buf.subarray(off + 8, off + len);
    if (data.subarray(0, 4).toString("latin1") === "\x89PNG" && (!best || data.length > best.length)) best = data;
    off += len;
  }
  return best;
}

const app = path.join(outRes, "app");
fs.rmSync(app, { recursive: true, force: true });
extractAsar(path.join(srcRes, "app.asar"), app);
console.log("extracted app.asar");

const mainJs = path.join(app, ".webpack", "main", "index.js");
fs.writeFileSync(mainJs, applyPatches(fs.readFileSync(mainJs, "utf8")));

// files the app reads from process.resourcesPath
for (const item of ["assets", "fasttext", "migrations", "ax-inspect-lib.mjs", "ax-inspect.mjs", "ax-inspect-server.mjs"]) {
  const from = path.join(srcRes, item);
  if (fs.existsSync(from)) fs.cpSync(from, path.join(outRes, item), { recursive: true, filter: (f) => !f.includes(":com.apple.cs.") });
}
fs.rmSync(path.join(outRes, "default_app.asar"), { force: true });

const icon = largestIcnsPng(path.join(srcRes, "electron.icns"));
if (icon) fs.writeFileSync(path.join(outRes, "wispr-flow.png"), icon);
console.log("prepared resources");
