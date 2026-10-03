//! Bounded HTTP adapter for the Hub authentication pilot.

use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::net::TcpStream;

use serde::{Deserialize, Serialize};
use spocky_contracts::text::js_length;

use crate::{
    AccountEmailMessage, AccountId, AuthorityError, BrowserAccountStatus, DurableHubStore,
    HubError, HubPilot, InvitationRole, OrganizationId, PasswordChange, RecoveryToken,
    SessionToken, iso_timestamp, render_password_reset_email, render_verification_email,
};

const SESSION_COOKIE: &str = "paseo_session";
const MAX_REQUEST_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RegistrationMode {
    InviteOnly,
    OpenVerified,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AccountEmail {
    pub recipient: String,
    pub url: String,
    pub callback_url: String,
    pub token: RecoveryToken,
    pub message: AccountEmailMessage,
}

#[derive(Debug, Default, Eq, PartialEq)]
pub struct AccountEmails {
    pub verifications: Vec<AccountEmail>,
    pub password_resets: Vec<AccountEmail>,
}

#[derive(Debug, Eq, PartialEq)]
pub struct HttpRequest {
    pub method: String,
    pub path: String,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
}

#[derive(Debug, Eq, PartialEq)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
}

pub struct HubHttpService<S: DurableHubStore> {
    hub: HubPilot<S>,
    registration: RegistrationMode,
    base_url: String,
    account_emails: AccountEmails,
}

impl<S: DurableHubStore> HubHttpService<S> {
    #[must_use]
    pub fn new(hub: HubPilot<S>) -> Self {
        Self {
            hub,
            registration: RegistrationMode::InviteOnly,
            base_url: "https://hub.example.test".into(),
            account_emails: AccountEmails::default(),
        }
    }

    #[must_use]
    pub fn with_registration(
        hub: HubPilot<S>,
        registration: RegistrationMode,
        base_url: &str,
    ) -> Self {
        Self {
            hub,
            registration,
            base_url: base_url.trim_end_matches('/').to_owned(),
            account_emails: AccountEmails::default(),
        }
    }

    #[must_use]
    pub const fn account_emails(&self) -> &AccountEmails {
        &self.account_emails
    }

    pub fn handle(&mut self, request: &HttpRequest) -> HttpResponse {
        let path = request_path(&request.path);
        match (request.method.as_str(), path) {
            ("GET", "/api/auth/paseo/state") => self.state(request),
            ("GET", "/api/auth/get-session") => self.get_session(request),
            ("GET", "/api/auth/verify-email") => self.verify_email(request),
            ("POST", "/api/auth/sign-up/email") => self.sign_up(request),
            ("POST", "/api/auth/sign-in/email") => self.sign_in(request),
            ("POST", "/api/auth/request-password-reset") => self.request_password_reset(request),
            ("POST", "/api/auth/reset-password") => self.reset_password(request),
            ("POST", "/api/auth/paseo/change-password") => self.change_password(request),
            ("POST", "/api/auth/paseo/complete-app-setup") => self.complete_setup(request),
            ("POST", "/api/auth/paseo/create-invitation") => self.create_invitation(request),
            ("POST", "/api/auth/paseo/cancel-invitation") => self.cancel_invitation(request),
            ("POST", "/api/auth/paseo/accept-invitation") => self.accept_invitation(request),
            ("POST", "/api/auth/paseo/select-organization") => self.select_organization(request),
            ("GET", path) if path.starts_with("/api/auth/reset-password/") => {
                Self::password_reset_callback(request)
            }
            _ => json_response(404, &ErrorBody { error: "not_found" }),
        }
    }

    fn get_session(&self, request: &HttpRequest) -> HttpResponse {
        let account = session_token(request)
            .as_ref()
            .and_then(|token| self.hub.account_for_session(token));
        let Some(account) = account else {
            return raw_json_response(200, b"null".to_vec());
        };
        json_response(
            200,
            &SessionBody {
                user: SessionUser {
                    email: account.as_str(),
                },
            },
        )
    }

    fn state(&self, request: &HttpRequest) -> HttpResponse {
        let token = session_token(request);
        json_response(200, &self.hub.browser_account_state(token.as_ref()))
    }

    fn sign_up(&mut self, request: &HttpRequest) -> HttpResponse {
        if request
            .headers
            .get("content-type")
            .is_none_or(|content_type| {
                !content_type.split(';').next().is_some_and(|value| {
                    value
                        .trim_matches(is_ows)
                        .eq_ignore_ascii_case("application/json")
                })
            })
        {
            return json_response(
                400,
                &ErrorBody {
                    error: "invalid_signup",
                },
            );
        }
        let Ok(input) = serde_json::from_slice::<SignUpBody>(&request.body) else {
            return json_response(
                400,
                &ErrorBody {
                    error: "invalid_signup",
                },
            );
        };
        if self.registration == RegistrationMode::OpenVerified {
            return self.open_sign_up(input);
        }
        let Some(invitation) = input.invitation else {
            return json_response(
                403,
                &ErrorBody {
                    error: "registration_closed",
                },
            );
        };
        match self.hub.register_invited_account(
            &AccountId::from(input.email.as_str()),
            &input.name,
            &input.password,
            &invitation,
        ) {
            Ok(()) => json_response(
                200,
                &StateBody {
                    status: BrowserAccountStatus::AppSetupRequired,
                },
            ),
            Err(HubError::InvitationUnavailable | HubError::IdempotencyConflict) => json_response(
                403,
                &ErrorBody {
                    error: "registration_closed",
                },
            ),
            Err(HubError::InvalidInvitationInput) => json_response(
                400,
                &ErrorBody {
                    error: "invalid_signup",
                },
            ),
            Err(_) => json_response(
                500,
                &ErrorBody {
                    error: "internal_error",
                },
            ),
        }
    }

    fn open_sign_up(&mut self, input: SignUpBody) -> HttpResponse {
        let callback_url = input
            .callback_url
            .unwrap_or_else(|| format!("{}/?auth=email-verification", self.base_url));
        match self.hub.register_unverified_account(
            &AccountId::from(input.email.as_str()),
            &input.name,
            &input.password,
        ) {
            Ok(token) => {
                let url = format!(
                    "{}/api/auth/verify-email?token={}&callbackURL={}",
                    self.base_url,
                    token.as_str(),
                    percent_encode(&callback_url)
                );
                self.account_emails.verifications.push(AccountEmail {
                    message: render_verification_email(&input.email, &url, token.as_str()),
                    recipient: input.email,
                    url,
                    callback_url,
                    token,
                });
                json_response(200, &EmptyBody {})
            }
            Err(HubError::InvalidRecoveryInput) => json_response(
                400,
                &ErrorBody {
                    error: "invalid_signup",
                },
            ),
            Err(HubError::IdempotencyConflict) => json_response(
                422,
                &CodeBody {
                    code: "USER_ALREADY_EXISTS",
                },
            ),
            Err(_) => json_response(
                500,
                &ErrorBody {
                    error: "internal_error",
                },
            ),
        }
    }

    fn sign_in(&mut self, request: &HttpRequest) -> HttpResponse {
        let Ok(input) = serde_json::from_slice::<SignInBody>(&request.body) else {
            return json_response(
                400,
                &ErrorBody {
                    error: "invalid_body",
                },
            );
        };
        match self
            .hub
            .sign_in(&AccountId::from(input.email.as_str()), &input.password)
        {
            Ok(token) => {
                let mut response = json_response(
                    200,
                    &StateBody {
                        status: self.hub.browser_account_status(Some(&token)),
                    },
                );
                response.headers.insert(
                    "set-cookie".into(),
                    format!(
                        "{SESSION_COOKIE}={}; Path=/; HttpOnly; SameSite=Lax",
                        token.as_str()
                    ),
                );
                response
            }
            Err(HubError::InvalidCredentials) => json_response(
                401,
                &ErrorBody {
                    error: "invalid_credentials",
                },
            ),
            Err(HubError::EmailNotVerified) => json_response(
                403,
                &MessageCodeBody {
                    message: "Email not verified",
                    code: "EMAIL_NOT_VERIFIED",
                },
            ),
            Err(_) => json_response(
                500,
                &ErrorBody {
                    error: "internal_error",
                },
            ),
        }
    }

    fn verify_email(&mut self, request: &HttpRequest) -> HttpResponse {
        let query = query_parameters(&request.path);
        let (Some(token), Some(callback_url)) = (query.get("token"), query.get("callbackURL"))
        else {
            return json_response(
                400,
                &CodeBody {
                    code: "INVALID_TOKEN",
                },
            );
        };
        let recovery = RecoveryToken::from(token.as_str());
        let Ok(account) = self.hub.verify_account(&recovery) else {
            return json_response(
                400,
                &CodeBody {
                    code: "INVALID_TOKEN",
                },
            );
        };
        let Ok(session) = self.hub.sign_in_after_verification(&account) else {
            return json_response(
                500,
                &ErrorBody {
                    error: "internal_error",
                },
            );
        };
        redirect_with_session(callback_url, &session)
    }

    fn request_password_reset(&mut self, request: &HttpRequest) -> HttpResponse {
        let Ok(input) = serde_json::from_slice::<PasswordResetRequestBody>(&request.body) else {
            return json_response(
                400,
                &CodeBody {
                    code: "INVALID_BODY",
                },
            );
        };
        match self
            .hub
            .request_password_reset(&AccountId::from(input.email.as_str()))
        {
            Ok(Some(token)) => {
                let url = format!(
                    "{}/api/auth/reset-password/{}?callbackURL={}",
                    self.base_url,
                    token.as_str(),
                    percent_encode(&input.redirect_to)
                );
                self.account_emails.password_resets.push(AccountEmail {
                    message: render_password_reset_email(&input.email, &url, token.as_str()),
                    recipient: input.email,
                    url,
                    callback_url: input.redirect_to,
                    token,
                });
                json_response(200, &EmptyBody {})
            }
            Ok(None) => json_response(200, &EmptyBody {}),
            Err(_) => json_response(
                500,
                &ErrorBody {
                    error: "internal_error",
                },
            ),
        }
    }

    fn password_reset_callback(request: &HttpRequest) -> HttpResponse {
        let path = request_path(&request.path);
        let Some(token) = path.strip_prefix("/api/auth/reset-password/") else {
            return json_response(
                400,
                &CodeBody {
                    code: "INVALID_TOKEN",
                },
            );
        };
        let Some(callback_url) = query_parameters(&request.path).get("callbackURL").cloned() else {
            return json_response(
                400,
                &CodeBody {
                    code: "INVALID_TOKEN",
                },
            );
        };
        redirect(&append_query(&callback_url, "token", token))
    }

    fn reset_password(&mut self, request: &HttpRequest) -> HttpResponse {
        let Ok(input) = serde_json::from_slice::<ResetPasswordBody>(&request.body) else {
            return json_response(
                400,
                &CodeBody {
                    code: "INVALID_BODY",
                },
            );
        };
        match self.hub.reset_password(
            &RecoveryToken::from(input.token.as_str()),
            &input.new_password,
        ) {
            Ok(()) => json_response(200, &EmptyBody {}),
            Err(HubError::InvalidRecoveryToken) => json_response(
                400,
                &CodeBody {
                    code: "INVALID_TOKEN",
                },
            ),
            Err(HubError::InvalidRecoveryInput) => json_response(
                400,
                &CodeBody {
                    code: "INVALID_PASSWORD",
                },
            ),
            Err(_) => json_response(
                500,
                &ErrorBody {
                    error: "internal_error",
                },
            ),
        }
    }

    fn change_password(&mut self, request: &HttpRequest) -> HttpResponse {
        let Some(token) = session_token(request) else {
            return json_response(
                401,
                &ErrorBody {
                    error: "unauthorized",
                },
            );
        };
        let Some(account) = self.hub.account_for_session(&token) else {
            return json_response(
                401,
                &ErrorBody {
                    error: "unauthorized",
                },
            );
        };
        let Ok(input) = serde_json::from_slice::<ChangePasswordBody>(&request.body) else {
            return json_response(
                400,
                &ErrorBody {
                    error: "invalid_body",
                },
            );
        };
        match self.hub.replace_password(&PasswordChange {
            account,
            current_password: input.current_password,
            new_password: input.new_password,
        }) {
            Ok(()) => json_response(
                200,
                &StateBody {
                    status: self.hub.browser_account_status(Some(&token)),
                },
            ),
            Err(HubError::InvalidCurrentPassword) => json_response(
                403,
                &ErrorBody {
                    error: "invalid_current_password",
                },
            ),
            Err(_) => json_response(
                500,
                &ErrorBody {
                    error: "internal_error",
                },
            ),
        }
    }

    fn complete_setup(&mut self, request: &HttpRequest) -> HttpResponse {
        let Some(token) = session_token(request) else {
            return json_response(
                401,
                &ErrorBody {
                    error: "unauthorized",
                },
            );
        };
        match self.hub.complete_app_setup(&token) {
            Ok(()) => json_response(
                200,
                &StateBody {
                    status: BrowserAccountStatus::Active,
                },
            ),
            Err(HubError::Authority(_)) => json_response(
                403,
                &ErrorBody {
                    error: "password_change_required",
                },
            ),
            Err(HubError::InvalidSession) => json_response(
                401,
                &ErrorBody {
                    error: "unauthorized",
                },
            ),
            Err(_) => json_response(
                500,
                &ErrorBody {
                    error: "internal_error",
                },
            ),
        }
    }

    fn select_organization(&mut self, request: &HttpRequest) -> HttpResponse {
        let Some(token) = session_token(request) else {
            return json_response(
                401,
                &ErrorBody {
                    error: "unauthorized",
                },
            );
        };
        let Ok(input) = serde_json::from_slice::<SelectOrganizationBody>(&request.body) else {
            return json_response(
                400,
                &ErrorBody {
                    error: "invalid_body",
                },
            );
        };
        let organization = OrganizationId::from(input.organization_id.as_str());
        match self.hub.select_organization(&token, &organization) {
            Ok(()) => json_response(
                200,
                &OrganizationBody {
                    organization_id: input.organization_id,
                },
            ),
            Err(HubError::InvalidSession) => json_response(
                401,
                &ErrorBody {
                    error: "unauthorized",
                },
            ),
            Err(HubError::Authority(AuthorityError::OrganizationUnavailable)) => json_response(
                404,
                &ErrorBody {
                    error: "organization_unavailable",
                },
            ),
            Err(_) => json_response(
                500,
                &ErrorBody {
                    error: "internal_error",
                },
            ),
        }
    }

    fn create_invitation(&mut self, request: &HttpRequest) -> HttpResponse {
        let Some(token) = session_token(request) else {
            return json_response(
                401,
                &ErrorBody {
                    error: "unauthorized",
                },
            );
        };
        let Some(account) = self.hub.account_for_session(&token) else {
            return json_response(
                401,
                &ErrorBody {
                    error: "unauthorized",
                },
            );
        };
        let Some(organization) = self.hub.active_organization_for_session(&token) else {
            return json_response(
                404,
                &ErrorBody {
                    error: "organization_unavailable",
                },
            );
        };
        let Ok(input) = serde_json::from_slice::<CreateInvitationBody>(&request.body) else {
            return json_response(
                400,
                &ErrorBody {
                    error: "invalid_body",
                },
            );
        };
        match self
            .hub
            .create_invitation(&account, &organization, &input.email, input.role)
        {
            Ok(invitation) => json_response(
                201,
                &ManagerInvitationBody {
                    id: invitation.id,
                    email: invitation.email,
                    role: invitation.role,
                    expires_at: iso_timestamp(invitation.expires_at_epoch_seconds),
                    link: invitation.link,
                },
            ),
            Err(HubError::Authority(_) | HubError::InvitationManagementRequired) => {
                json_response(403, &ErrorBody { error: "forbidden" })
            }
            Err(HubError::InvalidInvitationInput) => json_response(
                400,
                &ErrorBody {
                    error: "invalid_body",
                },
            ),
            Err(_) => json_response(
                500,
                &ErrorBody {
                    error: "internal_error",
                },
            ),
        }
    }

    fn cancel_invitation(&mut self, request: &HttpRequest) -> HttpResponse {
        let Some(token) = session_token(request) else {
            return json_response(
                401,
                &ErrorBody {
                    error: "unauthorized",
                },
            );
        };
        let Some(account) = self.hub.account_for_session(&token) else {
            return json_response(
                401,
                &ErrorBody {
                    error: "unauthorized",
                },
            );
        };
        let Some(organization) = self.hub.active_organization_for_session(&token) else {
            return json_response(
                404,
                &ErrorBody {
                    error: "organization_unavailable",
                },
            );
        };
        let Ok(input) = serde_json::from_slice::<InvitationIdBody>(&request.body) else {
            return json_response(
                400,
                &ErrorBody {
                    error: "invalid_body",
                },
            );
        };
        match self
            .hub
            .cancel_invitation(&account, &organization, &input.invitation_id)
        {
            Ok(()) => json_response(200, &CanceledBody { canceled: true }),
            Err(HubError::Authority(_) | HubError::InvitationManagementRequired) => {
                json_response(403, &ErrorBody { error: "forbidden" })
            }
            Err(HubError::InvitationUnavailable) => json_response(
                404,
                &ErrorBody {
                    error: "invitation_unavailable",
                },
            ),
            Err(_) => json_response(
                500,
                &ErrorBody {
                    error: "internal_error",
                },
            ),
        }
    }

    fn accept_invitation(&mut self, request: &HttpRequest) -> HttpResponse {
        let Some(token) = session_token(request) else {
            return json_response(
                401,
                &ErrorBody {
                    error: "unauthorized",
                },
            );
        };
        let Some(account) = self.hub.account_for_session(&token) else {
            return json_response(
                401,
                &ErrorBody {
                    error: "unauthorized",
                },
            );
        };
        let Ok(input) = serde_json::from_slice::<InvitationIdBody>(&request.body) else {
            return json_response(
                400,
                &ErrorBody {
                    error: "invalid_body",
                },
            );
        };
        match self.hub.accept_invitation(&account, &input.invitation_id) {
            Ok(organization) => {
                if self.hub.select_organization(&token, &organization).is_err() {
                    return json_response(
                        500,
                        &ErrorBody {
                            error: "internal_error",
                        },
                    );
                }
                json_response(
                    200,
                    &OrganizationBody {
                        organization_id: organization.as_str().to_owned(),
                    },
                )
            }
            Err(HubError::InvitationUnavailable) => json_response(
                404,
                &ErrorBody {
                    error: "invitation_unavailable",
                },
            ),
            Err(_) => json_response(
                500,
                &ErrorBody {
                    error: "internal_error",
                },
            ),
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SignInBody {
    email: String,
    password: String,
}

#[derive(Deserialize)]
struct SignUpBody {
    name: String,
    email: String,
    password: String,
    invitation: Option<String>,
    #[serde(rename = "callbackURL")]
    callback_url: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PasswordResetRequestBody {
    email: String,
    redirect_to: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResetPasswordBody {
    token: String,
    new_password: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChangePasswordBody {
    current_password: String,
    new_password: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SelectOrganizationBody {
    organization_id: String,
}

#[derive(Deserialize)]
struct CreateInvitationBody {
    email: String,
    role: InvitationRole,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct InvitationIdBody {
    invitation_id: String,
}

#[derive(Serialize)]
struct CanceledBody {
    canceled: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ManagerInvitationBody {
    id: String,
    email: String,
    role: InvitationRole,
    expires_at: String,
    link: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OrganizationBody {
    organization_id: String,
}

#[derive(Serialize)]
struct StateBody {
    status: BrowserAccountStatus,
}

#[derive(Serialize)]
struct EmptyBody {}

#[derive(Serialize)]
struct CodeBody {
    code: &'static str,
}

#[derive(Serialize)]
struct MessageCodeBody {
    message: &'static str,
    code: &'static str,
}

#[derive(Serialize)]
struct SessionBody<'a> {
    user: SessionUser<'a>,
}

#[derive(Serialize)]
struct SessionUser<'a> {
    email: &'a str,
}

#[derive(Serialize)]
struct ErrorBody {
    error: &'static str,
}

fn session_token(request: &HttpRequest) -> Option<SessionToken> {
    let cookies = parse_cookies(request.headers.get("cookie")?);
    cookies
        .get(SESSION_COOKIE)
        .or_else(|| cookies.get("better-auth.session_token"))
        .map(|value| SessionToken::from(value.as_str()))
}

/// Optional whitespace of RFC 7230: space and horizontal tab only, narrower than
/// `String.prototype.trim`.
fn is_ows(character: char) -> bool {
    matches!(character, ' ' | '\t')
}

/// better-auth's `parseCookies` (`cookies/cookie-utils.mjs`): `;` separated chunks, names and
/// values trimmed of OWS, a surrounding pair of double quotes removed, entries whose name is not an
/// RFC 7230 token or whose value is not RFC 6265 cookie octets (plus space and comma) dropped, and
/// the value percent-decoded when that is valid. A repeated name keeps its last value.
fn parse_cookies(header: &str) -> BTreeMap<String, String> {
    let mut cookies = BTreeMap::new();
    if js_length(header) < 2 {
        return cookies;
    }
    for chunk in header.split(';') {
        let Some((name, value)) = chunk.split_once('=') else {
            continue;
        };
        let name = name.trim_matches(is_ows);
        let value = value.trim_matches(is_ows);
        let value = value
            .strip_prefix('"')
            .and_then(|inner| inner.strip_suffix('"'))
            .filter(|_| value.len() >= 2)
            .unwrap_or(value);
        if cookie_name(name) && cookie_value(value) {
            cookies.insert(name.to_owned(), decode_uri_component(value));
        }
    }
    cookies
}

/// `[\x21\x23-\x27\x2A\x2B\x2D\x2E\x30-\x39\x41-\x5A\x5E\x5F\x60\x61-\x7A\x7C\x7E]+`.
fn cookie_name(name: &str) -> bool {
    !name.is_empty()
        && name.bytes().all(|byte| {
            matches!(
                byte,
                0x21 | 0x23..=0x27 | 0x2A | 0x2B | 0x2D | 0x2E | 0x30..=0x39 | 0x41..=0x5A
                    | 0x5E | 0x5F | 0x60 | 0x61..=0x7A | 0x7C | 0x7E
            )
        })
}

/// `[\x20\x21\x23-\x3A\x3C-\x5B\x5D-\x7E]*`.
fn cookie_value(value: &str) -> bool {
    value
        .bytes()
        .all(|byte| matches!(byte, 0x20 | 0x21 | 0x23..=0x3A | 0x3C..=0x5B | 0x5D..=0x7E))
}

/// `decodeURIComponent`, or the text as it is when the escapes are not valid UTF-8.
fn decode_uri_component(text: &str) -> String {
    if !text.contains('%') {
        return text.to_owned();
    }
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            // Two hexadecimal digits exactly; `from_str_radix` alone would also take `+1`.
            let digits = bytes
                .get(index + 1..index + 3)
                .filter(|pair| pair.iter().all(u8::is_ascii_hexdigit))
                .and_then(|pair| std::str::from_utf8(pair).ok())
                .and_then(|pair| u8::from_str_radix(pair, 16).ok());
            let Some(byte) = digits else {
                return text.to_owned();
            };
            decoded.push(byte);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).unwrap_or_else(|_| text.to_owned())
}

fn request_path(uri: &str) -> &str {
    let uri = uri
        .strip_prefix("http://")
        .or_else(|| uri.strip_prefix("https://"))
        .and_then(|remainder| remainder.find('/').map(|index| &remainder[index..]))
        .unwrap_or(uri);
    uri.split_once('?').map_or(uri, |(path, _)| path)
}

fn query_parameters(uri: &str) -> BTreeMap<String, String> {
    let Some((_, query)) = uri.split_once('?') else {
        return BTreeMap::new();
    };
    query
        .split('&')
        .filter_map(|field| {
            let (name, value) = field.split_once('=')?;
            Some((percent_decode(name), percent_decode(value)))
        })
        .collect()
}

fn percent_encode(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            use std::fmt::Write as _;
            write!(encoded, "%{byte:02X}").expect("write URL encoding");
        }
    }
    encoded
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let hex = &value[index + 1..index + 3];
            if let Ok(byte) = u8::from_str_radix(hex, 16) {
                decoded.push(byte);
                index += 3;
                continue;
            }
        }
        decoded.push(if bytes[index] == b'+' {
            b' '
        } else {
            bytes[index]
        });
        index += 1;
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

fn append_query(url: &str, name: &str, value: &str) -> String {
    let separator = if url.contains('?') { '&' } else { '?' };
    format!("{url}{separator}{name}={}", percent_encode(value))
}

fn redirect(location: &str) -> HttpResponse {
    HttpResponse {
        status: 302,
        headers: BTreeMap::from([
            ("location".into(), location.into()),
            ("content-length".into(), "0".into()),
        ]),
        body: Vec::new(),
    }
}

fn redirect_with_session(location: &str, session: &SessionToken) -> HttpResponse {
    let mut response = redirect(location);
    response.headers.insert(
        "set-cookie".into(),
        format!(
            "better-auth.session_token={}; Max-Age=604800; Path=/; HttpOnly; SameSite=Lax",
            session.as_str()
        ),
    );
    response
}

fn json_response(status: u16, body: &impl Serialize) -> HttpResponse {
    let body = serde_json::to_vec(body).expect("serializable Hub response");
    HttpResponse {
        status,
        headers: BTreeMap::from([
            ("content-type".into(), "application/json".into()),
            ("content-length".into(), body.len().to_string()),
        ]),
        body,
    }
}

fn raw_json_response(status: u16, body: Vec<u8>) -> HttpResponse {
    HttpResponse {
        status,
        headers: BTreeMap::from([
            ("content-type".into(), "application/json".into()),
            ("content-length".into(), body.len().to_string()),
        ]),
        body,
    }
}

pub fn serve_one<S: DurableHubStore>(
    stream: &mut TcpStream,
    service: &mut HubHttpService<S>,
) -> io::Result<()> {
    let request = read_request(stream)?;
    let response = service.handle(&request);
    write_response(stream, &response)
}

fn read_request(stream: &mut TcpStream) -> io::Result<HttpRequest> {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 4096];
    let header_end = loop {
        let read = stream.read(&mut buffer)?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "incomplete request",
            ));
        }
        bytes.extend_from_slice(&buffer[..read]);
        if bytes.len() > MAX_REQUEST_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "request too large",
            ));
        }
        if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break index + 4;
        }
    };
    let headers_text = std::str::from_utf8(&bytes[..header_end])
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let mut lines = headers_text.split("\r\n");
    let request_line = lines
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing request line"))?;
    let mut request_parts = request_line.split_whitespace();
    let method = request_parts.next().unwrap_or_default().to_owned();
    let path = request_parts.next().unwrap_or_default().to_owned();
    let mut headers = BTreeMap::new();
    for line in lines.filter(|line| !line.is_empty()) {
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(
                name.trim_matches(is_ows).to_ascii_lowercase(),
                value.trim_matches(is_ows).to_owned(),
            );
        }
    }
    let body_length = headers
        .get("content-length")
        .map_or(Ok(0), |value| value.parse::<usize>())
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    while bytes.len() < header_end + body_length {
        let read = stream.read(&mut buffer)?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "incomplete body",
            ));
        }
        bytes.extend_from_slice(&buffer[..read]);
        if bytes.len() > MAX_REQUEST_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "request too large",
            ));
        }
    }
    Ok(HttpRequest {
        method,
        path,
        headers,
        body: bytes[header_end..header_end + body_length].to_vec(),
    })
}

fn write_response(stream: &mut TcpStream, response: &HttpResponse) -> io::Result<()> {
    let reason = match response.status {
        200 => "OK",
        201 => "Created",
        302 => "Found",
        422 => "Unprocessable Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        _ => "Internal Server Error",
    };
    write!(stream, "HTTP/1.1 {} {reason}\r\n", response.status)?;
    for (name, value) in &response.headers {
        write!(stream, "{name}: {value}\r\n")?;
    }
    write!(stream, "connection: close\r\n\r\n")?;
    stream.write_all(&response.body)
}

#[cfg(test)]
mod tests {
    use super::{decode_uri_component, parse_cookies};

    #[test]
    fn cookies_follow_better_auth_parse_cookies() {
        let cookies = parse_cookies("a=1;b=2; c = \"3\" ;\td=%E2%82%AC;e=%zz;f=%+1;a=last");
        assert_eq!(cookies.get("a").map(String::as_str), Some("last"));
        assert_eq!(cookies.get("b").map(String::as_str), Some("2"));
        assert_eq!(cookies.get("c").map(String::as_str), Some("3"));
        assert_eq!(cookies.get("d").map(String::as_str), Some("\u{20ac}"));
        assert_eq!(cookies.get("e").map(String::as_str), Some("%zz"));
        assert_eq!(cookies.get("f").map(String::as_str), Some("%+1"));
    }

    #[test]
    fn cookies_with_a_bad_name_or_value_are_dropped() {
        for header in [
            "x",
            "=1",
            "a b=1",
            "a=1\u{b}",
            "a=\"x\"y\"",
            "a=b\\c",
            "a=\u{e9}",
            "\u{a0}a=1",
            "a=\u{a0}1",
        ] {
            assert!(parse_cookies(header).is_empty(), "{header:?}");
        }
        // Only SP and HTAB are trimmed, so a vertical tab keeps the name invalid.
        assert!(parse_cookies("\u{b}a=1").is_empty());
        assert_eq!(parse_cookies(" a=1").len(), 1);
    }

    #[test]
    fn invalid_utf8_escapes_stay_as_written() {
        assert_eq!(decode_uri_component("%E2%82"), "%E2%82");
        assert_eq!(decode_uri_component("a%20b"), "a b");
    }
}
