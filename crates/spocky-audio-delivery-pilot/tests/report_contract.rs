use spocky_audio_delivery_pilot::run_pilot;

#[test]
fn evidence_report_is_deterministic_and_scoped() {
    let first = run_pilot().to_json_pretty().expect("report serializes");
    let second = run_pilot()
        .to_json_pretty()
        .expect("report serializes again");
    assert_eq!(first, second);

    let report: serde_json::Value = serde_json::from_slice(&first).expect("report parses");
    assert_eq!(report["schemaVersion"], 1);
    assert_eq!(report["baseline"], "paseo@5de45e2");
    assert_eq!(report["claim"], "contract_pilot_only");
    assert_eq!(report["traces"][0]["contractId"], "P2-AUDIO-01");
    assert_eq!(report["traces"][1]["contractId"], "P2-NATIVE-01");
    assert_eq!(report["traces"][2]["contractId"], "P2-DELIVERY-01");
    assert!(
        first
            .windows(b"unsupported".len())
            .any(|window| window == b"unsupported")
    );
    assert_eq!(
        report["limitations"],
        serde_json::json!([
            "no_full_parity_claim",
            "no_real_device_recording",
            "no_signed_package_artifact",
            "no_production_update_execution"
        ])
    );
}
