use std::thread;

use paseo_hub_pilot::{HubError, InvitationRole, PostgresInvitationStore};

#[test]
fn relational_schema_preserves_pinned_invitation_constraints() {
    let schema = PostgresInvitationStore::DISPOSABLE_SCHEMA_SQL;
    assert!(schema.contains("CREATE TABLE IF NOT EXISTS invitation"));
    assert!(schema.contains("invitations_pending_organization_email_unique"));
    assert!(schema.contains("lower(email)"));
    assert!(schema.contains("WHERE status = 'pending'"));
    assert!(schema.contains("role IN ('admin', 'member')"));
    assert!(schema.contains("status IN ('pending', 'accepted', 'rejected', 'canceled')"));
    assert!(schema.contains("UNIQUE (organization_id, user_id)"));
}

#[test]
fn postgres_invitation_races_preserve_one_credential_and_membership() {
    let Ok(url) = std::env::var("PASEO_TEST_POSTGRES_URL") else {
        eprintln!("PASEO_TEST_POSTGRES_URL absent; disposable PostgreSQL capture owns this test");
        return;
    };
    PostgresInvitationStore::bootstrap_disposable_schema(&url)
        .expect("bootstrap invitation schema");
    let suffix = std::process::id();
    let organization = format!("invitation-organization-{suffix}");
    let manager = format!("invitation-manager-{suffix}");
    let invitee = format!("invitation-invitee-{suffix}");
    let invitee_email = format!("invitee-{suffix}@example.test");
    let mut store = PostgresInvitationStore::open(&url).expect("open invitation store");
    store
        .seed_identity(
            &organization,
            &manager,
            &format!("manager-{suffix}@example.test"),
            &invitee,
            &invitee_email,
        )
        .expect("seed identities");

    let create = || {
        let url = url.clone();
        let organization = organization.clone();
        let manager = manager.clone();
        let invitee_email = invitee_email.clone();
        thread::spawn(move || {
            PostgresInvitationStore::open(&url)
                .expect("open concurrent creator")
                .create(
                    &organization,
                    &manager,
                    &invitee_email,
                    InvitationRole::Member,
                )
                .expect("create invitation")
        })
    };
    let first = create();
    let second = create();
    let first_id = first.join().expect("join first creator");
    let second_id = second.join().expect("join second creator");
    assert_eq!(first_id, second_id);
    assert_eq!(
        store.pending_count(&organization, &invitee_email).unwrap(),
        1
    );

    let accept = || {
        let url = url.clone();
        let invitation = first_id.clone();
        let invitee = invitee.clone();
        let invitee_email = invitee_email.clone();
        thread::spawn(move || {
            PostgresInvitationStore::open(&url)
                .expect("open concurrent accepter")
                .accept(&invitation, &invitee, &invitee_email)
        })
    };
    let first_accept = accept();
    let second_accept = accept();
    let outcomes = [
        first_accept.join().expect("join first accepter"),
        second_accept.join().expect("join second accepter"),
    ];
    assert_eq!(outcomes.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        outcomes
            .iter()
            .filter(|result| matches!(result, Err(HubError::InvitationUnavailable)))
            .count(),
        1
    );
    assert_eq!(store.membership_count(&organization, &invitee).unwrap(), 1);
    assert_eq!(
        store.pending_count(&organization, &invitee_email).unwrap(),
        0
    );
}
