use std::collections::HashMap;

use crate::{endpoint::Endpoint, endpoints};

#[derive(Debug)]
struct Registry {
    endpoints: HashMap<&'static str, Box<dyn Endpoint>>,
}

impl Default for Registry {
    fn default() -> Self {
        Self {
            endpoints: [(
                "azure-transcribe",
                Box::new(endpoints::AzureTranscribe) as Box<dyn Endpoint>,
            )]
            .into(),
        }
    }
}
