//! What the interface needs to know about each provider: how to ask for its credentials.
//! Everything else goes through the provider-agnostic [`kg_provider::Provider`].

use std::collections::BTreeMap;

use kg_provider::{ProviderAccess, Registry};
use secrecy::SecretString;

pub struct Field {
    /// Key in the provider's JSON config.
    pub key: &'static str,
    pub label: &'static str,
    pub secret: bool,
    pub default: &'static str,
}

pub struct ProviderUi {
    pub kind: &'static str,
    pub name: &'static str,
    pub fields: &'static [Field],
    pub register: fn(Registry) -> Registry,
}

pub const PROVIDERS: &[ProviderUi] = &[ProviderUi {
    kind: kg_provider_scaleway::KIND,
    name: "Scaleway",
    fields: &[
        Field { key: "access_key", label: "Access key", secret: false, default: "" },
        Field { key: "secret_key", label: "Secret key", secret: true, default: "" },
        Field { key: "project_id", label: "Project ID", secret: false, default: "" },
        Field { key: "zone", label: "Zone", secret: false, default: "fr-par-2" },
    ],
    register: |registry| registry.with(kg_provider_scaleway::Factory),
}];

pub fn find(kind: &str) -> Option<&'static ProviderUi> {
    PROVIDERS.iter().find(|p| p.kind == kind)
}

pub fn registry() -> Registry {
    PROVIDERS.iter().fold(Registry::default(), |registry, p| (p.register)(registry))
}

impl ProviderUi {
    pub fn access(&self, values: &BTreeMap<&'static str, String>) -> ProviderAccess {
        let config: serde_json::Map<_, _> = self
            .fields
            .iter()
            .map(|f| (f.key.to_string(), serde_json::Value::String(values.get(f.key).cloned().unwrap_or_default().trim().into())))
            .collect();
        ProviderAccess { kind: self.kind.into(), config: SecretString::from(serde_json::Value::Object(config).to_string()) }
    }

    pub fn is_complete(&self, values: &BTreeMap<&'static str, String>) -> bool {
        self.fields.iter().all(|f| values.get(f.key).is_some_and(|v| !v.trim().is_empty()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_provider_form_builds_an_access_its_factory_accepts() {
        for provider in PROVIDERS {
            let values = provider.fields.iter().map(|f| (f.key, format!(" value-{} ", f.key))).collect();
            assert!(provider.is_complete(&values));
            let access = provider.access(&values);
            assert!(registry().connect(&access).is_ok(), "{} rejects its own form", provider.name);
        }
    }

    #[test]
    fn missing_field_is_incomplete() {
        let scaleway = find("scaleway").unwrap();
        let values = BTreeMap::from([("access_key", "SCW".to_string())]);
        assert!(!scaleway.is_complete(&values));
    }
}
