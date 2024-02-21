use serde::Serialize;
use std::net::IpAddr;

#[derive(Serialize)]
struct ConfigItem {
    key: &'static str,
    value: String,
}

#[derive(Serialize)]
pub(crate) struct Source {
    source_type: &'static str,
    config: Vec<ConfigItem>,
    logical_source_name: String,
    physical_source_name: String,
}

impl Source {
    pub(crate) fn tcp_source(
        logical_source_name: String,
        physical_source_name: String,
        ip_addr: IpAddr,
        port: usize,
        flush_interval: std::time::Duration,
    ) -> Source {
        Source {
            source_type: "TCP_SOURCE",
            logical_source_name,
            physical_source_name,
            config: vec![
                ConfigItem {
                    key: "socketHost",
                    value: ip_addr.to_string(),
                },
                ConfigItem {
                    key: "socketPort",
                    value: port.to_string(),
                },
                ConfigItem {
                    key: "socketDomain",
                    value: "AF_INET".to_string(),
                },
                ConfigItem {
                    key: "socketType",
                    value: "SOCK_STREAM".to_string(),
                },
                ConfigItem {
                    key: "flushIntervalMS",
                    value: flush_interval.as_millis().to_string(),
                },
                ConfigItem {
                    key: "inputFormat",
                    value: "CSV".to_string(),
                },
                ConfigItem {
                    key: "decideMessageSize",
                    value: "TUPLE_SEPARATOR".to_string(),
                },
            ],
        }
    }
}
