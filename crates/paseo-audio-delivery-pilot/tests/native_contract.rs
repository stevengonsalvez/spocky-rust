use paseo_audio_delivery_pilot::{NativeCapability, NativePilot, Outcome, Platform};

#[test]
fn native_adapters_cover_mobile_contracts_with_ordered_evidence() {
    let mut pilot = NativePilot::new(
        Platform::Ios,
        [
            NativeCapability::Push,
            NativeCapability::Camera,
            NativeCapability::FilePicker,
            NativeCapability::Haptics,
            NativeCapability::Notifications,
            NativeCapability::Background,
            NativeCapability::DeepLink,
        ],
    );

    for capability in NativeCapability::ALL {
        assert_eq!(pilot.invoke(capability), Outcome::Supported);
    }

    assert_eq!(pilot.events().len(), NativeCapability::ALL.len());
    assert_eq!(pilot.events()[0].operation, "push");
    assert_eq!(pilot.events()[6].operation, "deep_link");
}

#[test]
fn web_and_reduced_mobile_builds_report_unsupported_adapters() {
    let mut web = NativePilot::new(Platform::Web, []);
    let mut fdroid = NativePilot::new(
        Platform::Android,
        [
            NativeCapability::FilePicker,
            NativeCapability::Haptics,
            NativeCapability::Background,
            NativeCapability::DeepLink,
        ],
    );

    assert_eq!(
        web.invoke(NativeCapability::Push),
        Outcome::Unsupported("native.push@web".into())
    );
    assert_eq!(
        fdroid.invoke(NativeCapability::Camera),
        Outcome::Unsupported("native.camera@android".into())
    );
    assert_eq!(
        fdroid.invoke(NativeCapability::Notifications),
        Outcome::Unsupported("native.notifications@android".into())
    );
}
