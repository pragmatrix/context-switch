use std::collections::{HashMap, hash_map::Entry};

use anyhow::Result;
use serde::Serialize;

use crate::{BillingRecord, BillingRecordValue, conversation::BillingId};

#[derive(Debug, Serialize)]
pub struct BillingRecords {
    service: String,
    scope: Option<String>,
    records: Vec<BillingRecord>,
}

/// Type definition for the inner HashMap key in BillingCollector
/// Contains (service, scope, name)
type BillingRecordKey = (String, Option<String>, String);

#[derive(Debug, Default)]
pub struct BillingCollector {
    /// The inner HashMap uses (service, scope, name) as the key and stores the BillingRecordValue.
    /// The scope is optional.
    records: HashMap<BillingId, HashMap<BillingRecordKey, BillingRecordValue>>,
}

impl BillingCollector {
    pub fn record(
        &mut self,
        id: &BillingId,
        service: &str,
        scope: Option<String>,
        records: Vec<BillingRecord>,
    ) -> Result<()> {
        // Get or create the records map for this billing ID
        let records_map = match self.records.entry(id.clone()) {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => {
                let map = HashMap::new();
                entry.insert(map)
            }
        };

        let scope = scope.map(|s| s.to_string());

        // Process each record
        for record in records {
            let inner_key = (service.to_string(), scope.clone(), record.name.clone());

            match records_map.entry(inner_key) {
                Entry::Occupied(mut occupied_inner) => {
                    occupied_inner.get_mut().aggregate_with(&record.value)?;
                }
                Entry::Vacant(vacant_inner) => {
                    vacant_inner.insert(record.value);
                }
            }
        }

        Ok(())
    }

    pub fn collect(&mut self, id: &BillingId) -> Vec<BillingRecords> {
        if let Some(records_map) = self.records.remove(id) {
            // Group records by service and scope
            let mut grouped: HashMap<(String, Option<String>), Vec<BillingRecord>> = HashMap::new();

            for ((service, scope, name), value) in records_map {
                grouped
                    .entry((service.clone(), scope))
                    .or_default()
                    .push(BillingRecord { name, value });
            }

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
