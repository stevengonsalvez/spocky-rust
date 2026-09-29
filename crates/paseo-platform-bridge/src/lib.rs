use std::collections::{BTreeSet, HashSet};
use std::error::Error;
use std::fmt::{Display, Formatter};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Presence<T> {
    Missing,
    Null,
    Value(T),
}

#[derive(Debug, Clone, PartialEq)]
pub struct AbiRequest {
    pub request_id: String,
    pub method: String,
    pub params: Presence<Value>,
}

impl AbiRequest {
    /// Decodes one deterministic bridge request.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed JSON or missing string envelope fields.
    pub fn from_json(bytes: &[u8]) -> Result<Self, AbiJsonError> {
        let value: Value = serde_json::from_slice(bytes).map_err(AbiJsonError::Json)?;
        let object = value
            .as_object()
            .ok_or_else(|| AbiJsonError::Contract("request must be an object".into()))?;
        Ok(Self {
            request_id: required_string(object, "requestId")?,
            method: required_string(object, "method")?,
            params: presence(object, "params"),
        })
    }

    /// Encodes the bridge request with stable envelope field ordering.
    ///
    /// # Errors
    ///
    /// Returns an error when JSON serialization fails.
    pub fn to_json(&self) -> Result<Vec<u8>, serde_json::Error> {
        let mut object = Map::new();
        object.insert("requestId".into(), Value::String(self.request_id.clone()));
        object.insert("method".into(), Value::String(self.method.clone()));
        insert_presence(&mut object, "params", &self.params);
        serde_json::to_vec(&object)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AbiResponse {
    pub request_id: String,
    pub result: Presence<Value>,
    pub error: Presence<BridgeError>,
}

impl AbiResponse {
    /// Encodes the bridge response with stable envelope field ordering.
    ///
    /// # Errors
    ///
    /// Returns an error when JSON serialization fails.
    pub fn to_json(&self) -> Result<Vec<u8>, serde_json::Error> {
        let mut object = Map::new();
        object.insert("requestId".into(), Value::String(self.request_id.clone()));
        insert_presence(&mut object, "result", &self.result);
        match &self.error {
            Presence::Missing => {}
            Presence::Null => {
                object.insert("error".into(), Value::Null);
            }
            Presence::Value(error) => {
                object.insert("error".into(), serde_json::to_value(error)?);
            }
        }
        serde_json::to_vec(&object)
    }
}

fn insert_presence(object: &mut Map<String, Value>, name: &str, value: &Presence<Value>) {
    match value {
        Presence::Missing => {}
        Presence::Null => {
            object.insert(name.into(), Value::Null);
        }
        Presence::Value(value) => {
            object.insert(name.into(), value.clone());
        }
    }
}

fn presence(object: &Map<String, Value>, name: &str) -> Presence<Value> {
    match object.get(name) {
        None => Presence::Missing,
        Some(Value::Null) => Presence::Null,
        Some(value) => Presence::Value(value.clone()),
    }
}

fn required_string(object: &Map<String, Value>, name: &str) -> Result<String, AbiJsonError> {
    object
        .get(name)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| AbiJsonError::Contract(format!("{name} must be a string")))
}

#[derive(Debug)]
pub enum AbiJsonError {
    Json(serde_json::Error),
    Contract(String),
}

impl Display for AbiJsonError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Json(error) => write!(formatter, "invalid bridge JSON: {error}"),
            Self::Contract(message) => write!(formatter, "invalid bridge request: {message}"),
        }
    }
}

impl Error for AbiJsonError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostKind {
    Browser,
    Electron,
    Ios,
    Android,
    Macos,
    Windows,
    Linux,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostCapability {
    KeyboardInput,
    TouchInput,
    FileDialog,
    GuestWebview,
    ManagedDaemon,
    AutoUpdate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostDescriptor {
    pub host: HostKind,
    pub capabilities: BTreeSet<HostCapability>,
}

impl HostDescriptor {
    #[must_use]
    pub fn new(host: HostKind, capabilities: impl IntoIterator<Item = HostCapability>) -> Self {
        Self {
            host,
            capabilities: capabilities.into_iter().collect(),
        }
    }

    /// Requires explicitly advertised host capabilities.
    ///
    /// # Errors
    ///
    /// Returns every unsupported capability without selecting a fallback.
    pub fn require(
        &self,
        required: impl IntoIterator<Item = HostCapability>,
    ) -> Result<NegotiatedCapabilities<HostCapability>, CapabilityError<HostCapability>> {
        negotiate(self.host, &self.capabilities, required)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdapterCapability {
    AudioCapture,
    AudioPlayback,
    SpeechToText,
    Haptics,
    SecureStorage,
    BackgroundService,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeAdapterDescriptor {
    pub host: HostKind,
    pub capabilities: BTreeSet<AdapterCapability>,
}

impl NativeAdapterDescriptor {
    #[must_use]
    pub fn new(host: HostKind, capabilities: impl IntoIterator<Item = AdapterCapability>) -> Self {
        Self {
            host,
            capabilities: capabilities.into_iter().collect(),
        }
    }

    /// Requires explicitly advertised adapter capabilities.
    ///
    /// # Errors
    ///
    /// Returns every unsupported capability without selecting a fallback.
    pub fn require(
        &self,
        required: impl IntoIterator<Item = AdapterCapability>,
    ) -> Result<NegotiatedCapabilities<AdapterCapability>, CapabilityError<AdapterCapability>> {
        negotiate(self.host, &self.capabilities, required)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PackageFormat {
    Dmg,
    Msi,
    Deb,
    Rpm,
    AppImage,
    Apk,
    Ipa,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackagingDescriptor {
    pub host: HostKind,
    pub formats: BTreeSet<PackageFormat>,
}

impl PackagingDescriptor {
    /// Requires an explicitly advertised package format.
    ///
    /// # Errors
    ///
    /// Returns an unsupported capability error instead of substituting a format.
    pub fn require(
        &self,
        required: PackageFormat,
    ) -> Result<NegotiatedCapabilities<PackageFormat>, CapabilityError<PackageFormat>> {
        negotiate(self.host, &self.formats, [required])
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateDescriptor {
    pub host: HostKind,
    pub signed: bool,
    pub rollback: bool,
}

impl UpdateDescriptor {
    /// Requires rollback support.
    ///
    /// # Errors
    ///
    /// Returns a visible bridge error when rollback is absent.
    pub fn require_rollback(&self) -> Result<(), BridgeError> {
        if self.rollback {
            Ok(())
        } else {
            Err(BridgeError::Unsupported {
                host: self.host,
                capability: "update.rollback".into(),
            })
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum BridgeError {
    Unsupported { host: HostKind, capability: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityError<C> {
    pub host: HostKind,
    pub unsupported: Vec<C>,
}

impl<C: std::fmt::Debug> Display for CapabilityError<C> {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "host {:?} lacks capabilities {:?}",
            self.host, self.unsupported
        )
    }
}

impl<C: std::fmt::Debug> Error for CapabilityError<C> {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NegotiatedCapabilities<C> {
    pub host: HostKind,
    pub capabilities: BTreeSet<C>,
}

fn negotiate<C: Copy + Ord + std::hash::Hash + Eq>(
    host: HostKind,
    available: &BTreeSet<C>,
    required: impl IntoIterator<Item = C>,
) -> Result<NegotiatedCapabilities<C>, CapabilityError<C>> {
    let required: BTreeSet<_> = required.into_iter().collect();
    let available_hash: HashSet<_> = available.iter().copied().collect();
    let unsupported: Vec<_> = required
        .iter()
        .copied()
        .filter(|capability| !available_hash.contains(capability))
        .collect();
    if unsupported.is_empty() {
        Ok(NegotiatedCapabilities {
            host,
            capabilities: required,
        })
    } else {
        Err(CapabilityError { host, unsupported })
    }
}
