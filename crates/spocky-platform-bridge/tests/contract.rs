use std::collections::BTreeSet;

use serde_json::json;
use spocky_platform_bridge::{
    AbiRequest, AbiResponse, AdapterCapability, BridgeError, CapabilityError, HostCapability,
    HostDescriptor, HostKind, NativeAdapterDescriptor, PackageFormat, PackagingDescriptor,
    Presence, UpdateDescriptor,
};

#[test]
fn json_abi_preserves_missing_null_and_object_order() {
    let missing = AbiRequest::from_json(br#"{"requestId":"r1","method":"workspace.open"}"#)
        .expect("missing params parse");
    let null =
        AbiRequest::from_json(br#"{"requestId":"r1","method":"workspace.open","params":null}"#)
            .expect("null params parse");
    let ordered = AbiRequest::from_json(
        br#"{"requestId":"r1","method":"workspace.open","params":{"z":1,"a":2}}"#,
    )
    .expect("ordered params parse");

    assert_eq!(missing.params, Presence::Missing);
    assert_eq!(null.params, Presence::Null);
    assert_eq!(ordered.params, Presence::Value(json!({"z": 1, "a": 2})));
    assert_eq!(
        ordered.to_json().expect("request serializes"),
        br#"{"requestId":"r1","method":"workspace.open","params":{"z":1,"a":2}}"#
    );

    let response = AbiResponse {
        request_id: "r1".into(),
        result: Presence::Null,
        error: Presence::Missing,
    };
    assert_eq!(
        response.to_json().expect("response serializes"),
        br#"{"requestId":"r1","result":null}"#
    );
}

#[test]
fn browser_and_electron_negotiate_declared_capabilities_only() {
    let browser = HostDescriptor::new(
        HostKind::Browser,
        [HostCapability::KeyboardInput, HostCapability::TouchInput],
    );
    let electron = HostDescriptor::new(
        HostKind::Electron,
        [
            HostCapability::KeyboardInput,
            HostCapability::FileDialog,
            HostCapability::GuestWebview,
            HostCapability::ManagedDaemon,
            HostCapability::AutoUpdate,
        ],
    );

    assert_eq!(
        browser.require([HostCapability::FileDialog]),
        Err(CapabilityError {
            host: HostKind::Browser,
            unsupported: vec![HostCapability::FileDialog],
        })
    );
    assert_eq!(
        electron
            .require([HostCapability::FileDialog, HostCapability::GuestWebview])
            .expect("declared Electron capabilities negotiate")
            .capabilities,
        BTreeSet::from([HostCapability::FileDialog, HostCapability::GuestWebview])
    );
}

#[test]
fn ios_and_android_native_descriptors_are_exact() {
    let ios = NativeAdapterDescriptor::new(
        HostKind::Ios,
        [
            AdapterCapability::AudioCapture,
            AdapterCapability::AudioPlayback,
            AdapterCapability::Haptics,
            AdapterCapability::SecureStorage,
        ],
    );
    let android = NativeAdapterDescriptor::new(
        HostKind::Android,
        [
            AdapterCapability::AudioCapture,
            AdapterCapability::AudioPlayback,
            AdapterCapability::Haptics,
            AdapterCapability::BackgroundService,
        ],
    );

    assert_eq!(
        ios.capabilities,
        BTreeSet::from([
            AdapterCapability::AudioCapture,
            AdapterCapability::AudioPlayback,
            AdapterCapability::Haptics,
            AdapterCapability::SecureStorage,
        ])
    );
    assert_eq!(
        android.capabilities,
        BTreeSet::from([
            AdapterCapability::AudioCapture,
            AdapterCapability::AudioPlayback,
            AdapterCapability::Haptics,
            AdapterCapability::BackgroundService,
        ])
    );
    assert_eq!(
        ios.require([AdapterCapability::SpeechToText]),
        Err(CapabilityError {
            host: HostKind::Ios,
            unsupported: vec![AdapterCapability::SpeechToText],
        })
    );
}

#[test]
fn packaging_and_updates_are_explicit_capabilities() {
    let packaging = PackagingDescriptor {
        host: HostKind::Linux,
        formats: BTreeSet::from([PackageFormat::Deb, PackageFormat::AppImage]),
    };
    let updates = UpdateDescriptor {
        host: HostKind::Linux,
        signed: true,
        rollback: false,
    };

    assert_eq!(
        packaging.require(PackageFormat::Rpm),
        Err(CapabilityError {
            host: HostKind::Linux,
            unsupported: vec![PackageFormat::Rpm],
        })
    );
    assert_eq!(
        updates.require_rollback(),
        Err(BridgeError::Unsupported {
            host: HostKind::Linux,
            capability: "update.rollback".into(),
        })
    );
}
