//! Bounded HTTP adapter for the Hub authentication pilot.

use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::net::TcpStream;

use serde::{Deserialize, Serialize};

use crate::{
    AccountId, AuthorityError, BrowserAccountStatus, DurableHubStore, HubError, HubPilot,
    InvitationRole, OrganizationId, PasswordChange, SessionToken, iso_timestamp,
};

const SESSION_COOKIE: &str = "paseo_session";
const MAX_REQUEST_BYTES: usize = 64 * 1024;

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
}

impl<S: DurableHubStore> HubHttpService<S> {
    #[must_use]
    pub const fn new(hub: HubPilot<S>) -> Self {
        Self { hub }
    }

    pub fn handle(&mut self, request: &HttpRequest) -> HttpResponse {
        match (request.method.as_str(), request.path.as_str()) {
            ("GET", "/api/auth/paseo/state") => self.state(request),
            ("POST", "/api/auth/sign-up/email") => self.sign_up(request),
            ("POST", "/api/auth/sign-in/email") => self.sign_in(request),
            ("POST", "/api/auth/paseo/change-password") => self.change_password(request),
            ("POST", "/api/auth/paseo/complete-app-setup") => self.complete_setup(request),
            ("POST", "/api/auth/paseo/create-invitation") => self.create_invitation(request),
            ("POST", "/api/auth/paseo/cancel-invitation") => self.cancel_invitation(request),
            ("POST", "/api/auth/paseo/accept-invitation") => self.accept_invitation(request),
            ("POST", "/api/auth/paseo/select-organization") => self.select_organization(request),
            _ => json_response(404, &ErrorBody { error: "not_found" }),
        }
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
                !content_type
                    .split(';')
                    .next()
                    .is_some_and(|value| value.trim().eq_ignore_ascii_case("application/json"))
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
        if input.name.trim().is_empty() {
            return json_response(
                400,
                &ErrorBody {
                    error: "invalid_signup",
                },
            );
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
struct ErrorBody {
    error: &'static str,
}

fn session_token(request: &HttpRequest) -> Option<SessionToken> {
    request.headers.get("cookie").and_then(|cookies| {
        cookies.split(';').find_map(|cookie| {
            let (name, value) = cookie.trim().split_once('=')?;
            (name == SESSION_COOKIE).then(|| SessionToken::from(value))
        })
    })
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
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
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
