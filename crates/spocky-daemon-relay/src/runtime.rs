//! `relay-runtime.ts`: starts and stops the relay transport as the enabled setting changes.

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RelayRuntimeConfig {
    pub enabled: bool,
    pub endpoint: String,
    pub public_endpoint: String,
    pub use_tls: bool,
    pub public_use_tls: bool,
}

/// A running transport, as `RelayTransportController`.
pub trait TransportController {
    /// `stop()`; an error is logged by the runtime as `Failed to stop relay transport`.
    ///
    /// # Errors
    ///
    /// Returns the message of a rejected stop.
    fn stop(self: Box<Self>) -> Result<(), String>;
}

/// Starts a transport for a configuration, or fails as `startTransport` throws.
pub trait TransportStarter {
    /// # Errors
    ///
    /// Returns the message of the thrown error.
    fn start(
        &mut self,
        config: &RelayRuntimeConfig,
    ) -> Result<Box<dyn TransportController>, String>;
}

/// What the runtime did that the caller must report.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RuntimeEffect {
    /// `logger.warn({ err }, "Failed to stop relay transport")`.
    StopFailed(String),
}

impl RuntimeEffect {
    /// The log message of the record.
    #[must_use]
    pub const fn message(&self) -> &'static str {
        match self {
            Self::StopFailed(_) => "Failed to stop relay transport",
        }
    }

    /// The `err` field of the record: the error's message.
    #[must_use]
    pub fn error(&self) -> &str {
        match self {
            Self::StopFailed(message) => message,
        }
    }
}

pub struct RelayRuntime<S: TransportStarter> {
    config: RelayRuntimeConfig,
    starter: S,
    transport: Option<Box<dyn TransportController>>,
}

impl<S: TransportStarter> RelayRuntime<S> {
    /// `createRelayRuntime`: starts at once when the configuration is enabled.
    ///
    /// # Errors
    ///
    /// Fails like `startTransport` when the initial start throws.
    pub fn new(config: RelayRuntimeConfig, starter: S) -> Result<Self, String> {
        let mut runtime = Self {
            config,
            starter,
            transport: None,
        };
        if runtime.config.enabled {
            runtime.start()?;
        }
        Ok(runtime)
    }

    #[must_use]
    pub const fn config(&self) -> &RelayRuntimeConfig {
        &self.config
    }

    fn start(&mut self) -> Result<(), String> {
        if self.transport.is_some() {
            return Ok(());
        }
        self.transport = Some(self.starter.start(&self.config)?);
        Ok(())
    }

    /// `setEnabled`.
    ///
    /// # Errors
    ///
    /// Fails when enabling throws; the setting then stays disabled.
    pub fn set_enabled(&mut self, enabled: bool) -> Result<Vec<RuntimeEffect>, String> {
        if self.config.enabled == enabled {
            return Ok(Vec::new());
        }
        if enabled {
            self.start()?;
            self.config.enabled = true;
            return Ok(Vec::new());
        }
        self.config.enabled = false;
        let current = self.transport.take();
        Ok(current
            .and_then(|transport| transport.stop().err())
            .map(RuntimeEffect::StopFailed)
            .into_iter()
            .collect())
    }

    /// `stop()`.
    ///
    /// # Errors
    ///
    /// Returns the message of a rejected transport stop.
    pub fn stop(&mut self) -> Result<(), String> {
        match self.transport.take() {
            Some(transport) => transport.stop(),
            None => Ok(()),
        }
    }
}
