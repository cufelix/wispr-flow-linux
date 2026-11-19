//! `wispr-flow-linux-helper --cursor`: prints the pointer position ("x y", physical pixels) whenever it moves.
//! Electron's `screen.getCursorScreenPoint()` on X11 only updates while the pointer is over one of its
//! windows that accepts input, and Wispr's floating bar ignores input until the pointer is on it, so the
//! bar and its popups could never become clickable. The patched app reads this stream instead.

use std::io::Write;
use std::time::Duration;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::ConnectionExt;

const POLL: Duration = Duration::from_millis(30); // ponytail: polling QueryPointer; XInput2 raw motion if CPU ever matters

pub fn run() -> anyhow::Result<()> {
    let (conn, screen) = x11rb::connect(None)?;
    let root = conn.setup().roots[screen].root;
    let parent = std::os::unix::process::parent_id();
    let (mut last, mut out) = (None, std::io::stdout().lock());
    // stop with Electron: its pid changes to init's when it dies, and a closed pipe fails the write
    while std::os::unix::process::parent_id() == parent {
        let p = conn.query_pointer(root)?.reply()?;
        let now = (p.root_x, p.root_y);
        if last != Some(now) {
            writeln!(out, "{} {}", now.0, now.1)?;
            out.flush()?;
            last = Some(now);
        }
        std::thread::sleep(POLL);
    }
    Ok(())
}
