use std::collections::{HashMap, hash_map::Entry};

use anyhow::Result;
use serde::Serialize;

use crate::{BillingRecord, BillingRecordValue, conversation::BillingId};

#[derive(Debug, Serialize)]
pub struct BillingRecords {
    service: String,
    scope: String,
    records: Vec<BillingRecord>,
}

#[derive(Debug, Default)]
pub struct BillingCollector {
    /// Records organized by billing id.
    /// The outer HashMap uses BillingId as the key.
    /// The inner HashMap uses (service, scope, name) as the key and stores the BillingRecordValue.
    records: HashMap<BillingId, HashMap<(String, String, String), BillingRecordValue>>,
}

impl BillingCollector {
    pub fn record(
        &mut self,
        id: &BillingId,
        service: &str,
        scope: &str,
        record: BillingRecord,
    ) -> Result<()> {
        let inner_key = (service.to_string(), scope.to_string(), record.name.clone());

        match self.records.entry(id.clone()) {
            Entry::Occupied(mut occupied_entry) => {
                let records_map = occupied_entry.get_mut();
                match records_map.entry(inner_key) {
                    Entry::Occupied(mut occupied_inner) => {
                        occupied_inner.get_mut().aggregate_with(&record.value)?;
                    }
                    Entry::Vacant(vacant_inner) => {
                        vacant_inner.insert(record.value);
                    }
                }
            }
            Entry::Vacant(vacant_entry) => {
                let mut records_map = HashMap::new();
                records_map.insert(inner_key, record.value);
                vacant_entry.insert(records_map);
            }
        }
        Ok(())
    }

    pub fn collect(&mut self, id: &BillingId) -> Vec<BillingRecords> {
        if let Some(records_map) = self.records.remove(id) {
            // Group records by service and scope
            let mut grouped: HashMap<(String, String), Vec<BillingRecord>> = HashMap::new();

            for ((service, scope, name), value) in records_map {
                grouped
                    .entry((service.clone(), scope.clone()))
                    .or_default()
                    .push(BillingRecord { name, value });
            }

            // Convert to Vec<ScopedBillingRecords>
            grouped
                .into_iter()
                .map(|((service, scope), records)| BillingRecords {
                    service,
                    scope,
                    records,
                })
                .collect()
        } else {
            Vec::new()
        }
    }
}
