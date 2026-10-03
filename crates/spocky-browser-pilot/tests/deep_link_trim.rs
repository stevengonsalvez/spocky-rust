//! `parseAgentDeepLink` (`agent-deep-link.ts`) trims each decoded segment with
//! `String.prototype.trim`. Node v22.20.0 printed, for these links, the target
//! below (`null` for the second): U+FEFF and U+3000 are trimmed, U+0085 and
//! U+200B are kept, and a segment that trims to nothing rejects the link.

use spocky_browser_pilot::{BrowserPilot, DeepLinkTarget, PilotConfig};

fn target(link: &str) -> Option<DeepLinkTarget> {
    let mut pilot = BrowserPilot::new(PilotConfig::all_supported());
    pilot.receive_deep_link("host-1", link).ok()?;
    pilot.host_ready("host-1")
}

#[test]
fn segments_are_trimmed_like_string_trim() {
    let trimmed = target("paseo://h/%EF%BB%BFserver/agent/%E3%80%80agent%E3%80%80").expect("link");
    assert_eq!(
        (trimmed.server_id.as_str(), trimmed.agent_id.as_str()),
        ("server", "agent")
    );
    assert!(target("paseo://h/%EF%BB%BF/agent/x").is_none());
    let kept = target("paseo://h/s/agent/%C2%85agent").expect("link");
    assert_eq!(kept.agent_id, "\u{85}agent");
    let zero_width = target("paseo://h/s/agent/%E2%80%8Bz").expect("link");
    assert_eq!(zero_width.agent_id, "\u{200b}z");
}
