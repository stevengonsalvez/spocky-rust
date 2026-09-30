use std::sync::{Arc, Barrier};
use std::thread;

use paseo_hub_pilot::{
    AccountId, Bootstrap, DurableHubStore, HubPilot, OrganizationId, PasswordChange, PostgresStore,
    StoreSemantics,
};
use postgres::{Client, NoTls};

#[test]
fn postgres_restart_and_concurrent_transactions_are_real() {
    let Ok(url) = std::env::var("PASEO_TEST_POSTGRES_URL") else {
        eprintln!("PASEO_TEST_POSTGRES_URL absent; disposable PostgreSQL capture owns this test");
        return;
    };
    let state_key = format!("runtime-{}", std::process::id());
    let mut hub = HubPilot::open(PostgresStore::open(&url, &state_key).expect("open PostgreSQL"))
        .expect("open Hub");
    assert_eq!(
        PostgresStore::SEMANTICS,
        StoreSemantics::PostgreSqlTransactionalSnapshot
    );
    hub.bootstrap(Bootstrap {
        instance_secret: "postgres-runtime-secret-at-least-32-characters".into(),
        owner: AccountId::from("owner@example.test"),
        organization: OrganizationId::from("organization-1"),
        temporary_password: "temporary-password".into(),
    })
    .expect("bootstrap in PostgreSQL");
    hub.replace_password(&PasswordChange {
        account: AccountId::from("owner@example.test"),
        current_password: "temporary-password".into(),
        new_password: "replacement-password".into(),
    })
    .expect("persist password replacement");
    drop(hub);

    let restarted =
        HubPilot::open(PostgresStore::open(&url, &state_key).expect("reopen PostgreSQL"))
            .expect("restart Hub");
    assert!(
        restarted
            .authorize(
                &AccountId::from("owner@example.test"),
                &OrganizationId::from("organization-1")
            )
            .is_ok()
    );

    let concurrent_key = format!("concurrent-{}", std::process::id());
    let barrier = Arc::new(Barrier::new(3));
    let mut writers = Vec::new();
    for value in [b"writer-a".to_vec(), b"writer-b".to_vec()] {
        let url = url.clone();
        let key = concurrent_key.clone();
        let barrier = Arc::clone(&barrier);
        writers.push(thread::spawn(move || {
            let store = PostgresStore::open(&url, key).expect("open concurrent store");
            barrier.wait();
            store.save(&value).expect("transactional save");
        }));
    }
    barrier.wait();
    for writer in writers {
        writer.join().expect("writer thread");
    }
    let stored = PostgresStore::open(&url, &concurrent_key)
        .expect("reopen concurrent store")
        .load()
        .expect("load concurrent state")
        .expect("state exists");
    assert!(stored == b"writer-a" || stored == b"writer-b");

    let mut client = Client::connect(&url, NoTls).expect("inspect PostgreSQL revision");
    let revision: i64 = client
        .query_one(
            "SELECT revision FROM paseo_hub_pilot_state WHERE state_key = $1",
            &[&concurrent_key],
        )
        .expect("query revision")
        .get(0);
    assert_eq!(revision, 2);
}
