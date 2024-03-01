use derive_builder::Builder;
use serde::Serialize;

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

#[derive(Debug, Clone)]
pub(crate) enum Format {
    NES(u8),
    CSV,
}

fn default_host_ip() -> String {
    "10.0.0.1".to_string()
}

#[derive(Debug, Builder)]
#[builder(setter(strip_option))]
pub(crate) struct TCPSourceConfig {
    logical_source_name: String,
    #[builder(default = "None")]
    physical_source_name: Option<String>,
    #[builder(default = "default_host_ip()")]
    socket_host: String,
    socket_port: u16,
    #[builder(default = "std::time::Duration::from_millis(100)")]
    flush_interval: std::time::Duration,
    #[builder(default = "Format::CSV")]
    format: Format,
}

impl Into<Source> for TCPSourceConfig {
    fn into(self) -> Source {
        let mut config = vec![
            ConfigItem {
                key: "socketHost",
                value: self.socket_host.to_string(),
            },
            ConfigItem {
                key: "socketPort",
                value: self.socket_port.to_string(),
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
                value: self.flush_interval.as_millis().to_string(),
            },
        ];

        match self.format {
            Format::CSV => config.append(&mut vec![
                ConfigItem {
                    key: "inputFormat",
                    value: "CSV".to_string(),
                },
                ConfigItem {
                    key: "decideMessageSize",
                    value: "TUPLE_SEPARATOR".to_string(),
                },
            ]),
            Format::NES(buffer_size_size) => config.append(&mut vec![
                ConfigItem {
                    key: "inputFormat",
                    value: "NES".to_string(),
                },
                ConfigItem {
                    key: "decideMessageSize",
                    value: "BUFFER_SIZE_FROM_SOCKET".to_string(),
                },
                ConfigItem {
                    key: "bytesUsedForSocketBufferSizeTransfer",
                    value: buffer_size_size.to_string(),
                },
            ]),
        };

        Source {
            source_type: "TCP_SOURCE",
            physical_source_name: self
                .physical_source_name
                .unwrap_or_else(|| format!("{}_phy", &self.logical_source_name)),
            logical_source_name: self.logical_source_name,
            config,
        }
    }
}
