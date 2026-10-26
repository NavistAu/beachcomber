use crate::plugin::sys;
use crate::provider::{
    FailbackConfig, FieldSchema, FieldType, InvalidationStrategy, Provider, ProviderMetadata,
    Source, SourceMetadata, SourceResult,
};

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(test)]
mod network_tests;

pub struct NetworkProvider;

impl Provider for NetworkProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "network".into(),
            sources: vec![interfaces_source_metadata()],
        }
    }

    fn sources(&self) -> Vec<Box<dyn Source>> {
        vec![Box::new(NetworkInterfaces)]
    }
}

fn interfaces_source_metadata() -> SourceMetadata {
    SourceMetadata {
        name: "interfaces".into(),
        fields: vec![
            FieldSchema {
                name: "interface".into(),
                field_type: FieldType::String,
            },
            FieldSchema {
                name: "ip".into(),
                field_type: FieldType::String,
            },
            FieldSchema {
                name: "vpn_active".into(),
                field_type: FieldType::Bool,
            },
            FieldSchema {
                name: "vpn_name".into(),
                field_type: FieldType::String,
            },
            FieldSchema {
                name: "ssid".into(),
                field_type: FieldType::String,
            },
            FieldSchema {
                name: "online".into(),
                field_type: FieldType::Bool,
            },
            FieldSchema {
                name: "ipv6".into(),
                field_type: FieldType::String,
            },
        ],
        invalidation: InvalidationStrategy::Poll {
            interval_secs: 10,
            count: 6,
        },
        failback: FailbackConfig {
            reattempts: 3,
            interval_secs: 30,
        },
        decay: None,
        budget: crate::scheduler::lifecycle::DEFAULT_BUDGET,
        timeout: crate::scheduler::lifecycle::DEFAULT_TIMEOUT,
        nearest: false,
    }
}

struct NetworkInterfaces;

impl Source for NetworkInterfaces {
    fn metadata(&self) -> &SourceMetadata {
        use std::sync::OnceLock;
        static M: OnceLock<SourceMetadata> = OnceLock::new();
        M.get_or_init(interfaces_source_metadata)
    }

    /// `sys`'s scan and SSID; the preferred interface on Linux is still the
    /// one `ip route` names, as it has been.
    fn execute(&self, _path: Option<&str>) -> Result<SourceResult, String> {
        #[cfg(target_os = "linux")]
        let preferred = linux::is_preferred_interface;
        #[cfg(target_os = "macos")]
        let preferred = sys::preferred_interface();
        let facts = sys::network_of(sys::scan_interfaces(&preferred), sys::wifi_ssid());
        Ok(SourceResult::from_json_object(&facts))
    }
}
