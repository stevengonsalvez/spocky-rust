use spocky_audio_delivery_pilot::{AndroidDeviceAdapter, NativeCapability};

#[test]
#[ignore = "requires the isolated ainb-api35 AVD on emulator-5580"]
fn android_adapter_executes_real_device_commands_and_reports_missing_app_routes() {
    let adapter = AndroidDeviceAdapter::new(
        "/Users/stevengonsalvez/Library/Android/sdk/platform-tools/adb",
        "emulator-5580",
    );

    let identity = adapter.identity().expect("Android identity is queried");
    assert_eq!(identity.android_release, "15");
    assert_eq!(identity.model, "sdk_gphone64_x86_64");

    let haptics = adapter
        .invoke(NativeCapability::Haptics)
        .expect("haptic command launches");
    assert!(haptics.succeeded);
    assert_eq!(haptics.exit_code, Some(0));

    let notification = adapter
        .invoke(NativeCapability::Notifications)
        .expect("notification command launches");
    assert!(notification.succeeded);
    assert!(
        notification
            .stdout
            .contains("Notification(channel=shell_cmd")
    );

    let deep_link = adapter
        .invoke(NativeCapability::DeepLink)
        .expect("deep-link command launches");
    assert!(!deep_link.succeeded);
    assert!(deep_link.output().contains("unable to resolve Intent"));
}
