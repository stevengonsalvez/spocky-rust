use paseo_audio_delivery_pilot::{DeliveryPilot, DeliveryState, Outcome};

#[test]
fn delivery_transitions_preserve_state_across_upgrade_failure_and_rollback() {
    let mut pilot = DeliveryPilot::new(true, true);

    assert_eq!(pilot.install("1.0.0", "sha256:state-a"), Outcome::Supported);
    assert_eq!(
        pilot.state(),
        &DeliveryState::Installed {
            version: "1.0.0".into()
        }
    );
    assert_eq!(
        pilot.upgrade("1.1.0", false),
        Outcome::Failed("update_failed".into())
    );
    assert_eq!(
        pilot.state(),
        &DeliveryState::UpdateFailed {
            active_version: "1.0.0".into(),
            target_version: "1.1.0".into(),
        }
    );
    assert_eq!(pilot.retained_state_digest(), Some("sha256:state-a"));
    assert_eq!(pilot.rollback(), Outcome::Supported);
    assert_eq!(
        pilot.state(),
        &DeliveryState::Installed {
            version: "1.0.0".into()
        }
    );
    assert_eq!(pilot.upgrade("1.1.0", true), Outcome::Supported);
    assert_eq!(pilot.uninstall(true), Outcome::Supported);
    assert_eq!(pilot.state(), &DeliveryState::Absent);
    assert_eq!(pilot.retained_state_digest(), Some("sha256:state-a"));
    assert_eq!(pilot.events().len(), 5);
}

#[test]
fn delivery_reports_unsupported_updates_and_destructive_uninstall() {
    let mut pilot = DeliveryPilot::new(false, false);

    pilot.install("1.0.0", "sha256:state-a");
    assert_eq!(
        pilot.upgrade("1.1.0", true),
        Outcome::Unsupported("delivery.upgrade".into())
    );
    assert_eq!(
        pilot.rollback(),
        Outcome::Unsupported("delivery.rollback".into())
    );
    assert_eq!(pilot.uninstall(false), Outcome::Supported);
    assert_eq!(pilot.retained_state_digest(), None);
}
