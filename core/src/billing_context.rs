use std::sync::{Arc, Mutex};

use anyhow::Result;

use crate::{BillingRecord, billing_collector::BillingCollector, conversation::BillingId};

#[derive(Debug, Clone)]
pub struct BillingContext {
    billing_id: BillingId,
    service: String,
    collector: Arc<Mutex<BillingCollector>>,
}

impl BillingContext {
    pub fn new(
        billing_id: BillingId,
        service: impl Into<String>,
        collector: Arc<Mutex<BillingCollector>>,
    ) -> Self {
        Self {
            billing_id,
            service: service.into(),
            collector,
        }
    }

    pub fn with_service(self, service: impl Into<String>) -> Self {
        Self {
            service: service.into(),
            ..self
        }
    }

    pub fn record(
        &self,
        scope: impl Into<Option<String>>,
        records: Vec<BillingRecord>,
    ) -> Result<()> {
        self.collector.lock().expect("Lock poisoned").record(
            &self.billing_id,
            &self.service,
            scope.into(),
            records,
        )
    }
}
