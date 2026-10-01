//! Session admission: who a `hello` is allowed to be.
//!
//! Source at Paseo `5de45e2`: `session-admission-auth.ts`. This is the
//! security-sensitive decision of the daemon; every branch of the baseline is
//! kept in the baseline's order.

use spocky_contracts::ws::{DaemonPermission, HelloAuth};

use crate::local_credential::matches_local_credential;

/// Why a hello is refused (`AdmissionFailure`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmissionFailure {
    PasswordRequired,
    IncorrectPassword,
}

/// `SessionAdmission` without the hub execution agents, which only a hub
/// transport grants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionAdmission {
    pub principal_id: String,
    pub permissions: Vec<DaemonPermission>,
}

impl SessionAdmission {
    /// `{ principalId: "owner", permissions: OWNER_PERMISSIONS }`.
    #[must_use]
    pub fn owner() -> Self {
        Self {
            principal_id: "owner".to_owned(),
            permissions: DaemonPermission::ALL.to_vec(),
        }
    }
}

/// `transport` of the resolved connection: only a relay may omit credentials.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmissionTransport {
    Direct,
    Relay,
}

/// Checks a plaintext password against the daemon's stored hash
/// (`bcryptjs.compare`).
pub trait PasswordVerifier: Send + Sync {
    fn verify(&self, password: &str, password_hash: &str) -> bool;
}

/// `resolveSessionAdmission`.
///
/// # Errors
///
/// [`AdmissionFailure::PasswordRequired`] for a direct hello with no
/// credential, [`AdmissionFailure::IncorrectPassword`] for a credential that
/// does not match.
///
/// With no password hash configured everyone is the owner. With one, a hello
/// needs either the per-run local credential or the password; a relay hello
/// without credentials is admitted for old mobile builds (`relayPasswordOptional`).
pub fn resolve_session_admission(
    credential: Option<&HelloAuth>,
    password_hash: Option<&str>,
    local_credential: Option<&str>,
    transport: AdmissionTransport,
    verifier: &dyn PasswordVerifier,
) -> Result<SessionAdmission, AdmissionFailure> {
    let Some(password_hash) = password_hash.filter(|hash| !hash.is_empty()) else {
        return Ok(SessionAdmission::owner());
    };
    let Some(credential) = credential else {
        if transport == AdmissionTransport::Relay {
            return Ok(SessionAdmission::owner());
        }
        return Err(AdmissionFailure::PasswordRequired);
    };
    match credential {
        HelloAuth::LocalCredential { token } => {
            let matches = local_credential
                .filter(|expected| !expected.is_empty())
                .is_some_and(|expected| matches_local_credential(expected, token.as_str()));
            if matches {
                Ok(SessionAdmission::owner())
            } else {
                Err(AdmissionFailure::IncorrectPassword)
            }
        }
        HelloAuth::Password { password } => {
            if verifier.verify(password.as_str(), password_hash) {
                Ok(SessionAdmission::owner())
            } else {
                Err(AdmissionFailure::IncorrectPassword)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spocky_contracts::text::JsText;

    /// Accepts exactly the password `secret`; records nothing else.
    struct Fixed;

    impl PasswordVerifier for Fixed {
        fn verify(&self, password: &str, password_hash: &str) -> bool {
            password == "secret" && password_hash == "hash"
        }
    }

    const LOCAL: &str = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQ";

    fn resolve(
        credential: Option<&HelloAuth>,
        hash: Option<&str>,
        local: Option<&str>,
        transport: AdmissionTransport,
    ) -> Result<SessionAdmission, AdmissionFailure> {
        resolve_session_admission(credential, hash, local, transport, &Fixed)
    }

    fn password(text: &str) -> HelloAuth {
        HelloAuth::Password {
            password: JsText::new(text),
        }
    }

    fn local(token: &str) -> HelloAuth {
        HelloAuth::LocalCredential {
            token: JsText::new(token),
        }
    }

    #[test]
    fn without_a_password_hash_everyone_is_the_owner_whatever_they_send() {
        for credential in [None, Some(password("x")), Some(local("x"))] {
            let admitted = resolve(credential.as_ref(), None, None, AdmissionTransport::Direct);
            assert_eq!(admitted, Ok(SessionAdmission::owner()));
        }
        let empty = resolve(None, Some(""), None, AdmissionTransport::Direct);
        assert_eq!(empty, Ok(SessionAdmission::owner()));
    }

    #[test]
    fn the_owner_has_every_permission() {
        let owner = SessionAdmission::owner();
        assert_eq!(owner.principal_id, "owner");
        assert_eq!(owner.permissions, DaemonPermission::ALL.to_vec());
    }

    #[test]
    fn a_direct_hello_without_credentials_needs_a_password() {
        let result = resolve(None, Some("hash"), Some(LOCAL), AdmissionTransport::Direct);
        assert_eq!(result, Err(AdmissionFailure::PasswordRequired));
    }

    #[test]
    fn a_relay_hello_without_credentials_is_admitted() {
        let result = resolve(None, Some("hash"), None, AdmissionTransport::Relay);
        assert_eq!(result, Ok(SessionAdmission::owner()));
    }

    #[test]
    fn a_relay_hello_with_a_wrong_credential_is_still_refused() {
        let result = resolve(
            Some(&password("nope")),
            Some("hash"),
            None,
            AdmissionTransport::Relay,
        );
        assert_eq!(result, Err(AdmissionFailure::IncorrectPassword));
    }

    #[test]
    fn the_local_credential_must_match_the_running_daemons_token() {
        let ok = resolve(
            Some(&local(LOCAL)),
            Some("hash"),
            Some(LOCAL),
            AdmissionTransport::Direct,
        );
        assert_eq!(ok, Ok(SessionAdmission::owner()));
        for (token, expected) in [
            (LOCAL, None),
            (LOCAL, Some("")),
            ("", Some(LOCAL)),
            ("wrong", Some(LOCAL)),
            (&LOCAL[..42], Some(LOCAL)),
        ] {
            let result = resolve(
                Some(&local(token)),
                Some("hash"),
                expected,
                AdmissionTransport::Direct,
            );
            assert_eq!(
                result,
                Err(AdmissionFailure::IncorrectPassword),
                "{token:?} {expected:?}"
            );
        }
    }

    #[test]
    fn a_local_credential_cannot_pass_as_a_password() {
        let result = resolve(
            Some(&password(LOCAL)),
            Some("hash"),
            Some(LOCAL),
            AdmissionTransport::Direct,
        );
        assert_eq!(result, Err(AdmissionFailure::IncorrectPassword));
    }

    #[test]
    fn the_password_is_checked_with_the_verifier_against_the_stored_hash() {
        let ok = resolve(
            Some(&password("secret")),
            Some("hash"),
            None,
            AdmissionTransport::Direct,
        );
        assert_eq!(ok, Ok(SessionAdmission::owner()));
        let wrong = resolve(
            Some(&password("Secret")),
            Some("hash"),
            None,
            AdmissionTransport::Direct,
        );
        assert_eq!(wrong, Err(AdmissionFailure::IncorrectPassword));
        let other_hash = resolve(
            Some(&password("secret")),
            Some("other"),
            None,
            AdmissionTransport::Direct,
        );
        assert_eq!(other_hash, Err(AdmissionFailure::IncorrectPassword));
    }
}
