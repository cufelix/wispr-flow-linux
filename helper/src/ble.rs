//! Wispr's BLE "MicRing" over BlueZ (system D-Bus). The helper is a plain bridge, as on macOS:
//! ring notifications go to Electron, which decodes the audio (IMA ADPCM, 244-byte packets) and
//! the control signals (1 PTT start, 2 PTT stop, 3 hands-free, 4 hands-free stop, 5 Enter) itself.

use crate::ipc;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use zbus::blocking::{Connection, MessageIterator};
use zbus::zvariant::{OwnedObjectPath, OwnedValue};

const SERVICE: &str = "19b10000-e8f2-537e-4f6c-d104768a1214"; // ring GATT service (chars ...0001, ...0002)
const AUDIO_PACKET: usize = 244;
const BLUEZ: &str = "org.bluez";
const DEVICE: &str = "org.bluez.Device1";
const CHAR: &str = "org.bluez.GattCharacteristic1";
const PROPS: &str = "org.freedesktop.DBus.Properties";

type Objects = HashMap<OwnedObjectPath, HashMap<String, HashMap<String, OwnedValue>>>;

/// Paired ring (BLE address, our `deviceId`), and whether the connect loop runs.
static PAIRED: Mutex<Option<String>> = Mutex::new(None);
static LOOP_STARTED: Mutex<bool> = Mutex::new(false);

fn log(msg: &str) {
    eprintln!("[linux-helper] BLE: {msg}");
}

/// Ring notification -> helper message for Electron (pure, tested).
fn notification(bytes: &[u8]) -> Option<(&'static str, serde_json::Value)> {
    match bytes.len() {
        0 => None,
        AUDIO_PACKET => Some(("BLEAudioData", json!({ "payload": { "data": base64(bytes) } }))),
        _ => Some(("BLEControl", json!({ "payload": { "signal": bytes[0] } }))),
    }
}

fn base64(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for c in bytes.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            s.push(if i <= c.len() {
                A[(n >> (18 - 6 * i) & 63) as usize] as char
            } else {
                '='
            });
        }
    }
    s
}

fn objects(bus: &Connection) -> zbus::Result<Objects> {
    bus.call_method(
        Some(BLUEZ),
        "/",
        Some("org.freedesktop.DBus.ObjectManager"),
        "GetManagedObjects",
        &(),
    )?
    .body()
    .deserialize()
}

fn str_prop(ifaces: &HashMap<String, HashMap<String, OwnedValue>>, iface: &str, name: &str) -> Option<String> {
    String::try_from(ifaces.get(iface)?.get(name)?.try_clone().ok()?).ok()
}

fn is_ring(ifaces: &HashMap<String, HashMap<String, OwnedValue>>) -> bool {
    let uuids = ifaces
        .get(DEVICE)
        .and_then(|d| d.get("UUIDs"))
        .and_then(|v| Vec::<String>::try_from(v.try_clone().ok()?).ok());
    uuids.is_some_and(|u| u.iter().any(|u| u.eq_ignore_ascii_case(SERVICE))) // never by advertised name
}

fn adapter(objs: &Objects) -> Option<OwnedObjectPath> {
    objs.iter()
        .find(|(_, i)| i.contains_key("org.bluez.Adapter1"))
        .map(|(p, _)| p.clone())
}

fn device_by_address(objs: &Objects, address: &str) -> Option<(OwnedObjectPath, String)> {
    objs.iter()
        .find(|(_, i)| str_prop(i, DEVICE, "Address").is_some_and(|a| a.eq_ignore_ascii_case(address)))
        .map(|(p, i)| (p.clone(), str_prop(i, DEVICE, "Name").unwrap_or_else(|| "MicRing".into())))
}

fn call(bus: &Connection, path: &str, iface: &str, method: &str) -> zbus::Result<()> {
    bus.call_method(Some(BLUEZ), path, Some(iface), method, &()).map(|_| ())
}

/// BleStartPairing: scan for a ring, report BlePairResult, then keep it connected.
pub fn start_pairing(scan_secs: u64) {
    std::thread::spawn(move || {
        let found = scan(Duration::from_secs(scan_secs.clamp(5, 120))).unwrap_or_else(|e| {
            log(&format!("scan failed: {e}"));
            None
        });
        match found {
            Some((address, name)) => {
                ipc::request(
                    "BlePairResult",
                    json!({ "payload": { "success": true, "deviceId": address, "deviceName": name } }),
                );
                set_paired(Some(address));
            }
            None => ipc::request("BlePairResult", json!({ "payload": { "success": false } })),
        }
    });
}

fn scan(timeout: Duration) -> zbus::Result<Option<(String, String)>> {
    let bus = Connection::system()?;
    let Some(hci) = adapter(&objects(&bus)?) else {
        log("no Bluetooth adapter");
        return Ok(None);
    };
    let filter: HashMap<&str, zbus::zvariant::Value> = HashMap::from([("Transport", "le".into())]);
    let _ = bus.call_method(
        Some(BLUEZ),
        hci.as_str(),
        Some("org.bluez.Adapter1"),
        "SetDiscoveryFilter",
        &(filter,),
    );
    call(&bus, hci.as_str(), "org.bluez.Adapter1", "StartDiscovery")?;
    let deadline = Instant::now() + timeout;
    let mut rings = Vec::new();
    while rings.is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(500));
        rings = objects(&bus)?
            .values()
            .filter(|i| is_ring(i))
            .map(|i| {
                (
                    str_prop(i, DEVICE, "Address").unwrap_or_default(),
                    str_prop(i, DEVICE, "Name").unwrap_or_else(|| "MicRing".into()),
                )
            })
            .collect();
    }
    let _ = call(&bus, hci.as_str(), "org.bluez.Adapter1", "StopDiscovery");
    // No Pair/Trusted: the GATT notifications need neither (CoreBluetooth on macOS does not bond either),
    // so the system Bluetooth trust list stays untouched.
    if rings.len() > 1 {
        log(&format!(
            "{} rings in range, not guessing; pair with only one nearby",
            rings.len()
        ));
        return Ok(None);
    }
    Ok(rings.pop())
}

/// BleSetPairedDevice: `Some(id)` keeps that ring connected, `None` forgets it.
pub fn set_paired(id: Option<String>) {
    let id = id.filter(|s| !s.is_empty());
    let old = std::mem::replace(&mut *PAIRED.lock().unwrap(), id.clone());
    if let Some(old) = old.filter(|o| Some(o) != id.as_ref())
        && let Ok(bus) = Connection::system()
        && let Some((path, _)) = objects(&bus).ok().and_then(|o| device_by_address(&o, &old))
    {
        let _ = call(&bus, path.as_str(), DEVICE, "Disconnect"); // the connect loop sees Connected=false
    }
    let mut started = LOOP_STARTED.lock().unwrap();
    if id.is_some() && !*started {
        *started = true;
        std::thread::spawn(connect_loop);
    }
}

fn connect_loop() {
    loop {
        let paired = PAIRED.lock().unwrap().clone();
        if let Some(address) = paired
            && let Err(e) = session(&address)
        {
            log(&format!("{address}: {e}"));
        }
        std::thread::sleep(Duration::from_secs(3)); // ponytail: fixed reconnect poll, BlueZ InterfacesAdded watch if it matters
    }
}

/// One connection: connect, subscribe to the ring characteristics, forward until it drops.
fn session(address: &str) -> zbus::Result<()> {
    let bus = Connection::system()?;
    let Some((dev, name)) = device_by_address(&objects(&bus)?, address) else {
        return Ok(());
    }; // not seen yet
    call(&bus, dev.as_str(), DEVICE, "Connect")?;
    let deadline = Instant::now() + Duration::from_secs(10);
    let chars = loop {
        let objs = objects(&bus)?;
        let chars: Vec<String> = objs
            .iter()
            .filter(|(p, i)| p.as_str().starts_with(dev.as_str()) && i.contains_key(CHAR))
            .filter(|(_, i)| {
                str_prop(i, CHAR, "UUID").is_some_and(|u| u.starts_with("19b1000") && !u.eq_ignore_ascii_case(SERVICE))
            })
            .map(|(p, _)| p.to_string())
            .collect();
        if !chars.is_empty() || Instant::now() > deadline {
            break chars;
        }
        std::thread::sleep(Duration::from_millis(300)); // GATT services resolve after Connect
    };
    if chars.is_empty() {
        return Err(zbus::Error::Failure("ring service not found".into()));
    }
    let rule = format!(
        "type='signal',sender='{BLUEZ}',interface='{PROPS}',member='PropertiesChanged',path_namespace='{}'",
        dev.as_str()
    );
    let events = MessageIterator::for_match_rule(zbus::MatchRule::try_from(rule.as_str())?, &bus, None)?;
    for c in &chars {
        call(&bus, c, CHAR, "StartNotify")?;
    }
    log(&format!("connected to {name}"));
    ipc::request(
        "BLEConnectionState",
        json!({ "payload": { "connected": true, "deviceName": name } }),
    );
    for msg in events {
        let msg = msg?;
        let Ok((iface, changed, _)) = msg.body().deserialize::<(String, HashMap<String, OwnedValue>, Vec<String>)>() else {
            continue;
        };
        if iface == DEVICE && changed.get("Connected").and_then(|v| bool::try_from(v).ok()) == Some(false) {
            break;
        }
        if iface == CHAR {
            let bytes = changed
                .get("Value")
                .and_then(|v| Vec::<u8>::try_from(v.try_clone().ok()?).ok())
                .unwrap_or_default();
            if let Some((kind, body)) = notification(&bytes) {
                ipc::request(kind, body);
            }
        }
        if PAIRED.lock().unwrap().as_deref() != Some(address) {
            let _ = call(&bus, dev.as_str(), DEVICE, "Disconnect");
            break;
        }
    }
    log(&format!("disconnected from {name}"));
    ipc::request(
        "BLEConnectionState",
        json!({ "payload": { "connected": false, "deviceName": name } }),
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_rfc4648() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn routes_audio_and_control_by_length() {
        let (kind, body) = notification(&[0u8; AUDIO_PACKET]).unwrap();
        assert_eq!(kind, "BLEAudioData");
        assert_eq!(body["payload"]["data"].as_str().unwrap().len(), 328);
        let (kind, body) = notification(&[2]).unwrap();
        assert_eq!((kind, body["payload"]["signal"].as_u64()), ("BLEControl", Some(2)));
        assert!(notification(&[]).is_none());
    }
}
