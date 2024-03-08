use std::net::IpAddr;

use indoc::indoc;
use once_cell::unsync::Lazy;
use ouroboros::self_referencing;
use rust_embed::{EmbeddedFile, RustEmbed};
use serde::Serialize;
use tinytemplate::TinyTemplate;

use crate::nes::{
    Source, WorkerQueryProcessingConfigurationBuilder, WorkerQueryProcessingConfigurationInternal,
};

thread_local! {
pub static TEMPLATES: Lazy<Templates> = Lazy::new(Templates::create);
}

const WORKER_CONFIG_TEMPLATE: &str = "worker_config";
const DOCKER_UNIT_TEMPLATE: &str = "dockerunit";
const NETWORK_CONFIGURATION_TEMPLATE: &str = "networkconfiguration";
const DOCKER_DAEMON_CONFIG_TEMPLATE: &str = "dockerdaemon";
const TEMPLATE_FILES: [&str; 4] = [
    WORKER_CONFIG_TEMPLATE,
    DOCKER_UNIT_TEMPLATE,
    NETWORK_CONFIGURATION_TEMPLATE,
    DOCKER_DAEMON_CONFIG_TEMPLATE,
];

#[derive(RustEmbed)]
#[folder = "resources/"]
struct TemplateAssets;

#[self_referencing]
pub struct Templates {
    pub(crate) files: Vec<(&'static str, EmbeddedFile)>,
    #[borrows(files)]
    #[covariant]
    pub(crate) tt: TinyTemplate<'this>,
}

impl Templates {
    fn create() -> Self {
        TemplatesBuilder {
            files: TEMPLATE_FILES
                .into_iter()
                .map(|name| {
                    (
                        name,
                        TemplateAssets::get(&format!("{}.template", name)).unwrap(),
                    )
                })
                .collect(),
            tt_builder: |files| {
                let mut tt = TinyTemplate::new();
                for (name, file) in files {
                    let template_str = std::str::from_utf8(file.data.as_ref()).unwrap();
                    tt.add_template(name, template_str).unwrap();
                }
                tt
            },
        }
        .build()
    }

    pub(crate) fn worker_config(wc: &WorkerConfiguration) -> String {
        TEMPLATES
            .try_with(|t| t.borrow_tt().render(WORKER_CONFIG_TEMPLATE, &wc).unwrap())
            .unwrap()
    }
    pub(crate) fn docker_unit(wc: &WorkerConfiguration) -> String {
        TEMPLATES
            .try_with(|t| t.borrow_tt().render(DOCKER_UNIT_TEMPLATE, &wc).unwrap())
            .unwrap()
    }

    pub(crate) fn docker_daemon(wc: &WorkerConfiguration) -> String {
        TEMPLATES
            .try_with(|t| {
                t.borrow_tt()
                    .render(DOCKER_DAEMON_CONFIG_TEMPLATE, &wc)
                    .unwrap()
            })
            .unwrap()
    }
    pub(crate) fn network_config(wc: &WorkerConfiguration) -> String {
        TEMPLATES
            .try_with(|t| {
                t.borrow_tt()
                    .render(NETWORK_CONFIGURATION_TEMPLATE, &wc)
                    .unwrap()
            })
            .unwrap()
    }
}

#[derive(Serialize)]
pub(crate) struct WorkerConfiguration {
    pub(crate) ip_addr: IpAddr,
    pub(crate) host_ip_addr: IpAddr,
    pub(crate) worker_id: usize,
    pub(crate) parent_id: usize,
    pub(crate) sources: Vec<Source>,
    pub(crate) log_level: &'static str,
    pub(crate) query_processing: WorkerQueryProcessingConfigurationInternal,
}

#[test]
fn physical_sources() {
    let wc = WorkerConfiguration {
        ip_addr: IpAddr::from([10, 0, 0, 1]),
        host_ip_addr: IpAddr::from([10, 0, 0, 2]),
        worker_id: 0,
        parent_id: 0,
        sources: vec![],
        log_level: "LOG_INFO",
        query_processing: WorkerQueryProcessingConfigurationBuilder::default()
            .buffer_size(8192)
            .total_number_of_buffers(4096)
            .number_of_source_buffers(32)
            .number_of_buffers_per_thread(128)
            .number_of_worker_threads(8)
            .build()
            .unwrap()
            .into(),
    };

    assert_eq!(
        &Templates::worker_config(&wc),
        indoc! {r#"
                logLevel: LOG_INFO
                localWorkerIp: 10.0.0.1
                coordinatorIp: 10.0.0.2
                numberOfSlots: 2147483647
                numWorkerThreads: 8
                bufferSizeInBytes: 8192
                numberOfBuffersPerWorker: 128
                queryCompiler:
                  queryCompilerNautilusBackendConfig: MLIR_COMPILER_BACKEND
                workerId: 0
                parentId: 0
                dataPort: 8432
                rpcPort: 8433
                coordinatorPort: 8434
                "#}
    );

    let wc = WorkerConfiguration {
        ip_addr: IpAddr::from([10, 0, 0, 1]),
        host_ip_addr: IpAddr::from([10, 0, 0, 2]),
        worker_id: 0,
        parent_id: 0,
        log_level: "LOG_DEBUG",
        sources: vec![],
        query_processing: WorkerQueryProcessingConfigurationInternal::default(),
    };
    assert_eq!(
        &Templates::worker_config(&wc),
        indoc! {r#"
                logLevel: LOG_DEBUG
                localWorkerIp: 10.0.0.1
                coordinatorIp: 10.0.0.2
                numberOfSlots: 2147483647
                queryCompiler:
                  queryCompilerNautilusBackendConfig: MLIR_COMPILER_BACKEND
                workerId: 0
                parentId: 0
                dataPort: 8432
                rpcPort: 8433
                coordinatorPort: 8434
                physicalSources:
                 - type: TCP_SOURCE
                   logicalSourceName: logical
                   physicalSourceName: physical
                   configuration:
                     socketHost: 10.0.0.1
                     socketPort: 8080
                     socketDomain: AF_INET
                     socketType: SOCK_STREAM
                     flushIntervalMS: 100
                     inputFormat: CSV
                     decideMessageSize: TUPLE_SEPARATOR
                "#}
    );
}
