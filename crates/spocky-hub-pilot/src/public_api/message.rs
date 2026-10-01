//! Minimal request and response model for the public API boundary.
//!
//! Header values are strings of Latin-1 characters, like the fetch `Headers` the Hub reads.

use std::fmt;

use reqwest::Url;

/// Case-insensitive header list. Equal names are combined with `, ` when read.
///
/// `Debug` prints credential-bearing headers as `<redacted>`.
#[derive(Clone, Default, Eq, PartialEq)]
pub struct Headers {
    entries: Vec<(String, String)>,
}

const REDACTED: &str = "<redacted>";

/// Headers whose values are credentials and never reach `Debug` output.
fn credential_header(name: &str) -> bool {
    matches!(
        name,
        "authorization" | "proxy-authorization" | "cookie" | "set-cookie"
    )
}

impl fmt::Debug for Headers {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_list()
            .entries(self.entries.iter().map(|(name, value)| {
                (
                    name.as_str(),
                    if credential_header(name) {
                        REDACTED
                    } else {
                        value.as_str()
                    },
                )
            }))
            .finish()
    }
}

fn http_whitespace(ch: char) -> bool {
    matches!(ch, '\t' | '\n' | '\r' | ' ')
}

impl Headers {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a value; leading and trailing HTTP whitespace is dropped, as fetch does.
    pub fn append(&mut self, name: &str, value: &str) {
        self.entries.push((
            name.to_ascii_lowercase(),
            value.trim_matches(http_whitespace).to_owned(),
        ));
    }

    /// Replaces every value of the header.
    pub fn set(&mut self, name: &str, value: &str) {
        let name = name.to_ascii_lowercase();
        self.entries.retain(|(existing, _)| *existing != name);
        self.entries
            .push((name, value.trim_matches(http_whitespace).to_owned()));
    }

    #[must_use]
    pub fn get(&self, name: &str) -> Option<String> {
        let name = name.to_ascii_lowercase();
        let values: Vec<&str> = self
            .entries
            .iter()
            .filter(|(existing, _)| *existing == name)
            .map(|(_, value)| value.as_str())
            .collect();
        (!values.is_empty()).then(|| values.join(", "))
    }

    /// Header names in sorted order with equal names combined, as iterating `Headers` yields them.
    #[must_use]
    pub fn sorted(&self) -> Vec<(String, String)> {
        let mut names: Vec<&String> = self.entries.iter().map(|(name, _)| name).collect();
        names.sort();
        names.dedup();
        names
            .into_iter()
            .filter_map(|name| self.get(name).map(|value| (name.clone(), value)))
            .collect()
    }
}

/// An incoming request. The URL is absolute, as `Request.url` always is.
///
/// `Debug` omits the body, which can carry credentials, and redacts credential headers.
#[derive(Clone)]
pub struct ApiRequest {
    pub method: String,
    pub url: Url,
    pub headers: Headers,
    pub body: Vec<u8>,
}

impl fmt::Debug for ApiRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ApiRequest")
            .field("method", &self.method)
            .field("url", &self.url.as_str())
            .field("headers", &self.headers)
            .field(
                "body",
                &format_args!("<{} bytes redacted>", self.body.len()),
            )
            .finish()
    }
}

/// The request URL was not an absolute URL.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidUrl;

impl ApiRequest {
    /// # Errors
    ///
    /// Returns [`InvalidUrl`] when `url` is not an absolute URL.
    pub fn new(
        method: &str,
        url: &str,
        headers: Headers,
        body: Vec<u8>,
    ) -> Result<Self, InvalidUrl> {
        Ok(Self {
            method: method.to_owned(),
            url: Url::parse(url).map_err(|_| InvalidUrl)?,
            headers,
            body,
        })
    }
}

/// An outgoing response. Header names are lower case.
///
/// `Debug` omits the body, which can carry a disclosed credential.
#[derive(Clone, Eq, PartialEq)]
pub struct ApiResponse {
    pub status: u16,
    pub headers: Headers,
    pub body: Vec<u8>,
}

impl fmt::Debug for ApiResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ApiResponse")
            .field("status", &self.status)
            .field("headers", &self.headers)
            .field(
                "body",
                &format_args!("<{} bytes redacted>", self.body.len()),
            )
            .finish()
    }
}

impl ApiResponse {
    /// `Response.json(...)` with extra headers: the content type defaults to `application/json`.
    #[must_use]
    pub fn json(status: u16, body: &str, headers: &[(&str, &str)]) -> Self {
        let mut response = Self {
            status,
            headers: Headers::new(),
            body: body.as_bytes().to_vec(),
        };
        response.headers.set("content-type", "application/json");
        for (name, value) in headers {
            response.headers.set(name, value);
        }
        response
    }

    #[must_use]
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::{ApiRequest, ApiResponse, Headers};

    #[test]
    fn debug_output_never_contains_credentials_or_bodies() {
        let mut headers = Headers::new();
        headers.append("Authorization", "Bearer paseo_pk_secret-token");
        headers.append("Cookie", "session=secret-cookie");
        headers.append("x-request-id", "visible-id");
        let request = ApiRequest::new(
            "POST",
            "https://hub.test/api/v1/manual-runs",
            headers,
            b"{\"credential\":\"secret-body\"}".to_vec(),
        )
        .expect("absolute URL");
        let response = ApiResponse::json(200, "{\"credential\":\"secret-response\"}", &[]);
        let printed = format!("{request:?} {response:?} {:?}", request.headers);
        for secret in [
            "secret-token",
            "secret-cookie",
            "secret-body",
            "secret-response",
        ] {
            assert!(!printed.contains(secret), "{secret} leaked in {printed}");
        }
        assert!(printed.contains("visible-id"));
        assert!(printed.contains("<redacted>"));
    }

    #[test]
    fn headers_combine_trim_and_sort() {
        let mut headers = Headers::new();
        headers.append("X-Request-ID", "\t first ");
        headers.append("x-request-id", "second");
        headers.append("Allow", "GET");
        assert_eq!(
            headers.get("X-REQUEST-ID").as_deref(),
            Some("first, second")
        );
        assert_eq!(
            headers.sorted(),
            vec![
                ("allow".to_owned(), "GET".to_owned()),
                ("x-request-id".to_owned(), "first, second".to_owned())
            ]
        );
    }
}
