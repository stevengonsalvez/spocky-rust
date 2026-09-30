use paseo_hub_pilot::PostgresSessionStore;

#[test]
fn relational_schema_preserves_pinned_session_contract() {
    let schema = PostgresSessionStore::DISPOSABLE_SCHEMA_SQL;
    assert!(schema.contains("CREATE TABLE IF NOT EXISTS session"));
    assert!(schema.contains("expires_at TIMESTAMPTZ NOT NULL"));
    assert!(schema.contains("token TEXT NOT NULL UNIQUE"));
    assert!(schema.contains("active_organization_id TEXT"));
    assert!(schema.contains("sessions_active_organization_id_idx"));
}

#[test]
fn postgres_active_organization_selection_is_membership_bound_and_fails_closed() {
    let Ok(url) = std::env::var("PASEO_TEST_POSTGRES_URL") else {
        eprintln!("PASEO_TEST_POSTGRES_URL absent; disposable PostgreSQL capture owns this test");
        return;
    };
    PostgresSessionStore::bootstrap_disposable_schema(&url).unwrap();
    let suffix = std::process::id();
    let user = format!("session-user-{suffix}");
    let first = format!("session-first-{suffix}");
    let second = format!("session-second-{suffix}");
    let session = format!("session-{suffix}");
    let mut store = PostgresSessionStore::open(&url).unwrap();
    store.seed(&user, &first, &second, &session).unwrap();

    assert_eq!(
        store.active_organization(&session, &user).unwrap(),
        Some(first.clone())
    );
    assert!(store.select_organization(&session, &user, &second).unwrap());
    assert_eq!(
        store.active_organization(&session, &user).unwrap(),
        Some(second.clone())
    );
    assert!(
        !store
            .select_organization(&session, &user, "foreign")
            .unwrap()
    );
    store.remove_membership(&user, &second).unwrap();
    assert_eq!(store.active_organization(&session, &user).unwrap(), None);
}
