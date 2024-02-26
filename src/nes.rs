use derive_builder::Builder;
use serde::Serialize;
use std::net::IpAddr;

#[derive(Serialize)]
struct ConfigItem {
    key: &'static str,
    value: String,
}

#[derive(Serialize, Default)]
pub(crate) struct WorkerQueryProcessingConfigurationInternal {
    config: Vec<ConfigItem>,
}

#[derive(Builder)]
#[builder(setter(strip_option))]
pub(crate) struct WorkerQueryProcessingConfiguration {
    number_of_worker_threads: Option<usize>,
    total_number_of_buffers: Option<usize>,
    number_of_source_buffers: Option<usize>,
    number_of_buffers_per_thread: Option<usize>,
    buffer_size: Option<usize>,
}

impl Into<WorkerQueryProcessingConfigurationInternal> for WorkerQueryProcessingConfiguration {
    fn into(self) -> WorkerQueryProcessingConfigurationInternal {
        let mut config_items = vec![];

        if let Some(number_of_worker_threads) = self.number_of_worker_threads {
            config_items.push(ConfigItem {
                key: "numWorkerThreads",
                value: number_of_worker_threads.to_string(),
            });
        }

        if let Some(buffer_size) = self.buffer_size {
            config_items.push(ConfigItem {
                key: "bufferSizeInBytes",
                value: buffer_size.to_string(),
            });
        }

        if let Some(num_buf) = self.number_of_buffers_per_thread {
            config_items.push(ConfigItem {
                key: "numberOfBuffersPerWorker",
                value: num_buf.to_string(),
            });
        }

        if let Some(num_buf) = self.total_number_of_buffers {
            config_items.push(ConfigItem {
                key: "numberOfBuffersInGlobalBufferManager",
                value: num_buf.to_string(),
            });
        }
        if let Some(num_buf) = self.number_of_source_buffers {
            config_items.push(ConfigItem {
                key: "numberOfBuffersInSourceLocalBufferPool",
                value: num_buf.to_string(),
            });
        }

        WorkerQueryProcessingConfigurationInternal {
            config: config_items,
        }
    }
}

#[derive(Serialize)]
pub(crate) struct Source {
    source_type: &'static str,
    logical_source_name: String,
    physical_source_name: String,
    config: Vec<ConfigItem>,
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
