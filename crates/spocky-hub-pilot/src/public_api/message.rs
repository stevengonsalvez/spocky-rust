//! Minimal request and response model for the public API boundary.
//!
//! Header values are strings of Latin-1 characters, like the fetch `Headers` the Hub reads.

use reqwest::Url;

/// Case-insensitive header list. Equal names are combined with `, ` when read.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Headers {
    entries: Vec<(String, String)>,
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
#[derive(Clone, Debug)]
pub struct ApiRequest {
    pub method: String,
    pub url: Url,
    pub headers: Headers,
    pub body: Vec<u8>,
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
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApiResponse {
    pub status: u16,
    pub headers: Headers,
    pub body: Vec<u8>,
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
    use super::Headers;

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
