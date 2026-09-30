use std::thread;
use std::time::Duration;

use paseo_hub_pilot::{ApiKeyAuthorization, ApiKeyScope, OrganizationId, PostgresApiKeyStore};
use postgres::{Client, NoTls};
use uuid::Uuid;

#[test]
fn relational_schema_preserves_pinned_api_key_constraints() {
    let schema = PostgresApiKeyStore::DISPOSABLE_SCHEMA_SQL;
    assert!(schema.contains("CREATE TABLE IF NOT EXISTS organization_api_keys"));
    assert!(schema.contains("prefix TEXT NOT NULL UNIQUE"));
    assert!(schema.contains("created_at TIMESTAMPTZ NOT NULL DEFAULT now()"));
    assert!(schema.contains("last_used_at TIMESTAMPTZ"));
    assert!(schema.contains("revoked_at TIMESTAMPTZ"));
    assert!(schema.contains("cardinality(scopes) > 0"));
    assert!(schema.contains("issued_by_api_key_id UUID"));
}

#[test]
fn postgres_api_key_revocation_serializes_enrollment_issuance() {
    let Ok(url) = std::env::var("PASEO_TEST_POSTGRES_URL") else {
        eprintln!("PASEO_TEST_POSTGRES_URL absent; disposable PostgreSQL capture owns this test");
        return;
    };
    PostgresApiKeyStore::bootstrap_disposable_schema(&url).expect("bootstrap relational schema");
    let organization = OrganizationId::from("relational-organization");
    let user = format!("relational-user-{}", std::process::id());
    let mut store = PostgresApiKeyStore::open(&url).expect("open relational store");
    store
        .seed_identity(organization.as_str(), &user)
        .expect("seed identity");

    let revoked_first = store
        .create(
            organization.as_str(),
            &user,
            "revoked first",
            &[ApiKeyScope::DaemonsEnroll],
        )
        .expect("create revoked-first key");
    let mut blocker = Client::connect(&url, NoTls).expect("open blocking client");
    let mut row_lock = blocker.transaction().expect("begin blocking transaction");
    row_lock
        .query_one(
            "SELECT id::text FROM organization_api_keys WHERE id = $1::text::uuid FOR UPDATE",
            &[&revoked_first.summary.id],
        )
        .expect("lock revoked-first row");
    let revoke_url = url.clone();
    let revoke_organization = organization.as_str().to_owned();
    let revoke_id = revoked_first.summary.id.clone();
    let revoke = thread::spawn(move || {
        PostgresApiKeyStore::open(&revoke_url)
            .expect("open revoke store")
            .revoke(&revoke_organization, &revoke_id)
            .expect("revoke key")
    });
    wait_until_query_blocks(&url, "UPDATE organization_api_keys", 100);
    let issue_url = url.clone();
    let issue_organization = organization.as_str().to_owned();
    let issue_key = revoked_first.summary.id.clone();
    let rejected_id = Uuid::new_v4().to_string();
    let issue = thread::spawn(move || {
        PostgresApiKeyStore::open(&issue_url)
            .expect("open issue store")
            .issue_enrollment_token(
                &rejected_id,
                "rejected-verifier",
                &issue_organization,
                &issue_key,
                4_102_444_800,
            )
            .expect("issue after revoke")
    });
    row_lock.commit().expect("release revoked-first row");
    assert!(revoke.join().expect("join revoke"));
    assert!(!issue.join().expect("join rejected issue"));

    let issued_first = store
        .create(
            organization.as_str(),
            &user,
            "issued first",
            &[ApiKeyScope::DaemonsEnroll],
        )
        .expect("create issued-first key");
    let accepted_id = Uuid::new_v4().to_string();
    let accepted = store
        .issue_enrollment_token(
            &accepted_id,
            "accepted-verifier",
            organization.as_str(),
            &issued_first.summary.id,
            4_102_444_800,
        )
        .expect("issue before revoke");
    assert!(accepted);
    assert!(
        store
            .revoke(organization.as_str(), &issued_first.summary.id)
            .expect("revoke issued key")
    );
    assert!(
        store
            .enrollment_token_is_expired(&accepted_id)
            .expect("inspect invalidated token")
    );
    assert_eq!(
        store
            .authorize(&revoked_first.secret, ApiKeyScope::DaemonsEnroll)
            .expect("authorize revoked key"),
        ApiKeyAuthorization::Unauthorized
    );
}

fn wait_until_query_blocks(url: &str, fragment: &str, attempts: usize) {
    let mut client = Client::connect(url, NoTls).expect("open activity client");
    for _ in 0..attempts {
        let blocked: bool = client
            .query_one(
                "SELECT EXISTS (
                    SELECT 1 FROM pg_stat_activity
                    WHERE pid <> pg_backend_pid()
                      AND state = 'active'
                      AND wait_event_type = 'Lock'
                      AND query LIKE '%' || $1 || '%'
                )",
                &[&fragment],
            )
            .expect("inspect blocked query")
            .get(0);
        if blocked {
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("query did not block in time: {fragment}");
}
