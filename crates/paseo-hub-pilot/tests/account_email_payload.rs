use paseo_hub_pilot::{render_password_reset_email, render_verification_email};

#[test]
fn account_email_payloads_match_pinned_subject_text_html_and_key() {
    let verification = render_verification_email(
        "verified@example.test",
        "https://hub.example.test/api/auth/verify-email?token=a&callbackURL=b&next=\"quoted\"",
        "verification-token",
    );
    assert_eq!(verification.to, "verified@example.test");
    assert_eq!(verification.subject, "Verify your Paseo Hub email");
    assert_eq!(
        verification.text,
        "Verify your email address to finish creating your Paseo Hub account:\n\nhttps://hub.example.test/api/auth/verify-email?token=a&callbackURL=b&next=\"quoted\"\n\nThis link expires in one hour."
    );
    assert_eq!(
        verification.html,
        "<p>Verify your email address to finish creating your Paseo Hub account.</p><p><a href=\"https://hub.example.test/api/auth/verify-email?token=a&amp;callbackURL=b&amp;next=&quot;quoted&quot;\">Verify email</a></p><p>This link expires in one hour.</p>"
    );
    assert_eq!(
        verification.idempotency_key,
        "paseo-verification-46f6e828be35b9e2482ea7fc7a6a8f43f95a131098470486be3d137d408c8811"
    );

    let reset = render_password_reset_email(
        "verified@example.test",
        "https://hub.example.test/api/auth/reset-password/reset-token?callbackURL=x&next='quoted'",
        "reset-token",
    );
    assert_eq!(reset.to, "verified@example.test");
    assert_eq!(reset.subject, "Reset your Paseo Hub password");
    assert_eq!(
        reset.text,
        "Set a new password for your Paseo Hub account:\n\nhttps://hub.example.test/api/auth/reset-password/reset-token?callbackURL=x&next='quoted'\n\nThis link expires in one hour. If you did not request this, you can ignore this email."
    );
    assert_eq!(
        reset.html,
        "<p>Set a new password for your Paseo Hub account.</p><p><a href=\"https://hub.example.test/api/auth/reset-password/reset-token?callbackURL=x&amp;next=&#39;quoted&#39;\">Reset password</a></p><p>This link expires in one hour. If you did not request this, you can ignore this email.</p>"
    );
    assert_eq!(
        reset.idempotency_key,
        "paseo-password-reset-7c18b43a1d8227cddb332e67971e790ce35ac2303f4fccfb2a565622f2fe1cec"
    );
}
