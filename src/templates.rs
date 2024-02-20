use once_cell::unsync::Lazy;
use ouroboros::self_referencing;
use rust_embed::{EmbeddedFile, RustEmbed};
use serde::Serialize;
use std::net::{IpAddr, Ipv4Addr};
use tinytemplate::TinyTemplate;

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
    pub(crate) ip_addr: Ipv4Addr,
    pub(crate) host_ip_addr: Ipv4Addr,
    pub(crate) worker_id: usize,
    pub(crate) parent_id: usize,
}
