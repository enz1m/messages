use std::collections::BTreeMap;
use std::sync::Arc;

use crate::{Adapter, AdapterError, ConnectionConfig};

#[derive(Clone, Debug)]
pub struct ProtocolDescriptor {
    pub id: &'static str,
    pub name: &'static str,
    pub icon: &'static str,
    pub description: &'static str,
    pub default_endpoint: &'static str,
    pub setup_hint: &'static str,
}

type Factory = fn(ConnectionConfig, Option<String>) -> Result<Arc<dyn Adapter>, AdapterError>;

pub struct Registry {
    entries: BTreeMap<&'static str, (ProtocolDescriptor, Factory)>,
}

impl Registry {
    pub fn empty() -> Self {
        Self {
            entries: BTreeMap::new(),
        }
    }

    pub fn register(
        &mut self,
        descriptor: ProtocolDescriptor,
        factory: Factory,
    ) -> Result<(), AdapterError> {
        if self.entries.contains_key(descriptor.id) {
            return Err(AdapterError::Configuration(format!(
                "Adapter '{}' is already registered.",
                descriptor.id
            )));
        }
        self.entries.insert(descriptor.id, (descriptor, factory));
        Ok(())
    }

    pub fn protocols(&self) -> impl Iterator<Item = &ProtocolDescriptor> {
        self.entries.values().map(|(descriptor, _)| descriptor)
    }

    pub fn descriptor(&self, id: &str) -> Option<&ProtocolDescriptor> {
        self.entries.get(id).map(|(descriptor, _)| descriptor)
    }

    pub fn connect(
        &self,
        config: ConnectionConfig,
        token: Option<String>,
    ) -> Result<Arc<dyn Adapter>, AdapterError> {
        let (_, factory) = self.entries.get(config.protocol.as_str()).ok_or_else(|| {
            AdapterError::Configuration("This protocol adapter is not installed.".into())
        })?;
        factory(config, token)
    }
}

impl Default for Registry {
    fn default() -> Self {
        let mut registry = Self::empty();
        let builtins: [(ProtocolDescriptor, Factory); 3] = [
            (
                ProtocolDescriptor {
                    id: "signal",
                    name: "Signal",
                    icon: "shield-check",
                    description: "Private conversations, together.",
                    default_endpoint: "http://127.0.0.1:8080",
                    setup_hint: "Link a signal-cli device, then connect to its local HTTP daemon.",
                },
                |config, token| Ok(Arc::new(crate::signal::Signal::new(config, token)?)),
            ),
            (
                ProtocolDescriptor {
                    id: "matrix",
                    name: "Matrix",
                    icon: "hash",
                    description: "Element and the open Matrix network.",
                    default_endpoint: "https://matrix.org",
                    setup_hint: "Connect with a homeserver URL, user ID and access token. Encrypted rooms require the encryption bridge.",
                },
                |config, token| {
                    if config.native_bridge {
                        Ok(Arc::new(crate::bridge::Bridge::new(config, token)?))
                    } else {
                        Ok(Arc::new(crate::matrix::Matrix::new(config, token)?))
                    }
                },
            ),
            (
                ProtocolDescriptor {
                    id: "whatsapp",
                    name: "WhatsApp",
                    icon: "phone",
                    description: "Stay close to friends and family.",
                    default_endpoint: "http://127.0.0.1:8821",
                    setup_hint: "Run messages-whatsapp, use its access-token file, then scan the pairing code in Messages.",
                },
                |config, token| Ok(Arc::new(crate::bridge::Bridge::new(config, token)?)),
            ),
        ];
        for (descriptor, factory) in builtins {
            registry
                .register(descriptor, factory)
                .expect("built-in protocol IDs are unique");
        }
        registry
    }
}
