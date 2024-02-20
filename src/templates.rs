use once_cell::unsync::Lazy;
use ouroboros::self_referencing;
use rust_embed::{EmbeddedFile, RustEmbed};
use serde::Serialize;
use tinytemplate::TinyTemplate;

thread_local! {
pub static TEMPLATES: Lazy<Templates> = Lazy::new(Templates::create);
}

const WORKER_CONFIG_TEMPLATE: &str = "worker_config";
const TEMPLATE_FILES: [&str; 1] = [WORKER_CONFIG_TEMPLATE];

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

    pub(crate) fn worker_config(&self, wc: &WorkerConfiguration) -> String {
        self.borrow_tt()
            .render(WORKER_CONFIG_TEMPLATE, &wc)
            .unwrap()
    }
}

#[derive(Serialize)]
pub(crate) struct WorkerConfiguration {
    ip_addr: String,
    host_ip_addr: String,
    worker_id: usize,
    parent_id: usize,
}
