use sha2::{Digest, Sha256};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AccountEmailMessage {
    pub to: String,
    pub subject: &'static str,
    pub text: String,
    pub html: String,
    pub idempotency_key: String,
}

#[must_use]
pub fn render_verification_email(to: &str, url: &str, token: &str) -> AccountEmailMessage {
    AccountEmailMessage {
        to: to.to_owned(),
        subject: "Verify your Spocky Hub email",
        text: format!(
            "Verify your email address to finish creating your Spocky Hub account:\n\n{url}\n\nThis link expires in one hour."
        ),
        html: format!(
            "<p>Verify your email address to finish creating your Spocky Hub account.</p><p><a href=\"{}\">Verify email</a></p><p>This link expires in one hour.</p>",
            escape_html(url)
        ),
        idempotency_key: email_key("verification", token),
    }
}

#[must_use]
pub fn render_password_reset_email(to: &str, url: &str, token: &str) -> AccountEmailMessage {
    AccountEmailMessage {
        to: to.to_owned(),
        subject: "Reset your Spocky Hub password",
        text: format!(
            "Set a new password for your Spocky Hub account:\n\n{url}\n\nThis link expires in one hour. If you did not request this, you can ignore this email."
        ),
        html: format!(
            "<p>Set a new password for your Spocky Hub account.</p><p><a href=\"{}\">Reset password</a></p><p>This link expires in one hour. If you did not request this, you can ignore this email.</p>",
            escape_html(url)
        ),
        idempotency_key: email_key("password-reset", token),
    }
}

fn email_key(kind: &str, token: &str) -> String {
    format!("paseo-{kind}-{:x}", Sha256::digest(token.as_bytes()))
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
