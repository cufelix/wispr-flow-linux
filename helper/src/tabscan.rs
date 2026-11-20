//! GetMeetingTabScan: Wispr asks which meeting tabs are open, to confirm that the user really joined
//! the call it is recording. Without that confirmation it never trusts "the browser released the
//! microphone" and keeps recording after the call ends.
//! Answer: the URL of the visible page of every non-private browser window, if it is a meeting page.
//! Other pages are never reported.
// ponytail: confirm-only. A window shows accessibility only for its selected tab, so a call in a
// background tab is not seen; the answer is therefore always `degraded`, which lets Wispr read it as
// "present" or "don't know", never as "the call is gone". Tab-strip titles would cover background tabs.

use crate::{a11y, mic};
use serde_json::{json, Value};

const MEETING_HOSTS: [&str; 5] = ["meet.google.com", "teams.microsoft.com", "teams.live.com", "teams.cloud.microsoft", "zoom.us"];

/// Host of an https URL, if it is one of the meeting services (or a subdomain, e.g. acme.zoom.us).
fn meeting_host(url: &str) -> Option<&'static str> {
    let host = url.strip_prefix("https://")?.split(['/', '?', '#']).next()?.split(':').next()?;
    MEETING_HOSTS.into_iter().find(|h| host == *h || host.ends_with(&format!(".{h}")))
}

pub fn scan() -> Value {
    let tabs: Vec<Value> = a11y::browser_documents()
        .into_iter()
        .filter(|(_, url)| meeting_host(url).is_some())
        .map(|((bus, _), url)| {
            let comm = a11y::bus_pid(&bus).and_then(|p| std::fs::read_to_string(format!("/proc/{p}/comm")).ok()).unwrap_or_default();
            json!({ "browser": mic::wispr_app_id(comm.trim()), "url": url })
        })
        .collect();
    json!({ "MeetingTabScan": { "payload": { "status": "ok", "scannedAgoMs": 0, "degraded": true, "tabs": tabs, "uncoveredBrowserBundleIds": [] } } })
}

#[cfg(test)]
mod tests {
    use super::meeting_host;

    #[test]
    fn only_meeting_pages_are_reported() {
        assert_eq!(meeting_host("https://meet.google.com/abc-defg-hij?authuser=0"), Some("meet.google.com"));
        assert_eq!(meeting_host("https://acme.zoom.us/j/123"), Some("zoom.us"));
        assert_eq!(meeting_host("https://mail.google.com/mail/u/0/"), None);
        assert_eq!(meeting_host("https://evilzoom.us/j/1"), None);
        assert_eq!(meeting_host("https://meet.google.com.evil.example/x"), None);
        assert_eq!(meeting_host("http://meet.google.com/abc"), None);
    }
}
