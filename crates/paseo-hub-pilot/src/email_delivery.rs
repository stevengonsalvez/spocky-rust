//! Bounded Resend delivery matching the pinned Hub email boundary.

use std::collections::BTreeMap;
use std::time::Duration;

use reqwest::blocking::Client;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use serde::Serialize;

use crate::{HubError, InvitationEmailMessage};

const RESEND_EMAILS_URL: &str = "https://api.resend.com/emails";
const DELIVERY_TIMEOUT: Duration = Duration::from_secs(10);
const IDEMPOTENCY_KEY: HeaderName = HeaderName::from_static("idempotency-key");

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResendConfig {
    pub api_key: String,
    pub from: String,
}

impl ResendConfig {
    pub fn from_environment(
        environment: &BTreeMap<String, String>,
    ) -> Result<Option<Self>, HubError> {
        let Some(raw_key) = environment.get("RESEND_API_KEY") else {
            return Ok(None);
        };
        let api_key = raw_key.trim();
        if api_key.is_empty() {
            return Ok(None);
        }
        if !api_key.starts_with("re_") {
            return Err(HubError::EmailDeliveryConfig);
        }
        let from = environment
            .get("RESEND_FROM")
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .ok_or(HubError::EmailDeliveryConfig)?;
        Ok(Some(Self {
            api_key: api_key.to_owned(),
            from: from.to_owned(),
        }))
    }
}

pub struct ResendEmailDelivery {
    client: Client,
    config: ResendConfig,
    endpoint: String,
}

impl ResendEmailDelivery {
    pub fn new(config: ResendConfig) -> Result<Self, HubError> {
        Self::with_endpoint(config, RESEND_EMAILS_URL.to_owned())
    }

    pub fn with_endpoint(config: ResendConfig, endpoint: String) -> Result<Self, HubError> {
        let client = Client::builder()
            .timeout(DELIVERY_TIMEOUT)
            .build()
            .map_err(|_| HubError::EmailDeliveryConfig)?;
        Ok(Self {
            client,
            config,
            endpoint,
        })
    }

    pub fn send(&self, message: &InvitationEmailMessage) -> Result<(), HubError> {
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", self.config.api_key))
                .map_err(|_| HubError::EmailDeliveryConfig)?,
        );
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            IDEMPOTENCY_KEY,
            HeaderValue::from_str(&message.idempotency_key)
                .map_err(|_| HubError::EmailDeliveryConfig)?,
        );
        let response = self
            .client
            .post(&self.endpoint)
            .headers(headers)
            .json(&ResendRequest {
                from: &self.config.from,
                to: [&message.to],
                subject: &message.subject,
                text: &message.text,
                html: &message.html,
            })
            .send()
            .map_err(|_| HubError::EmailDeliveryTransport)?;
        if !response.status().is_success() {
            return Err(HubError::EmailDeliveryRejected(response.status().as_u16()));
        }
        Ok(())
    }
}

#[derive(Serialize)]
struct ResendRequest<'a> {
    from: &'a str,
    to: [&'a str; 1],
    subject: &'a str,
    text: &'a str,
    html: &'a str,
}
