//! Live instance registry.
//!
//! Owned by the daemon, shared (Arc<Mutex<...>>) between IPC and MCP servers.
//! Resolves target strings (UUID / instance name / track name) to UUIDs.

use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use superduper_dsp_protocol::{InstanceStatus, InstanceSummary, ParamInfo};
use uuid::Uuid;

pub type SharedRegistry = Arc<Mutex<Registry>>;

pub struct Registry {
    pub instances: HashMap<Uuid, InstanceRecord>,
}

pub struct InstanceRecord {
    pub id: Uuid,
    pub name: Option<String>,
    pub track_name: Option<String>,
    pub current_effect: Option<String>,
    pub status: InstanceStatus,
    pub params: Vec<ParamInfo>,
    pub last_heartbeat: Instant,
    // TODO M2: sender half of an mpsc to push commands to this instance's IPC writer
    // pub command_tx: tokio::sync::mpsc::UnboundedSender<DaemonToPlugin>,
}

impl Registry {
    pub fn new_shared() -> SharedRegistry {
        Arc::new(Mutex::new(Self {
            instances: HashMap::new(),
        }))
    }

    pub fn add(&mut self, record: InstanceRecord) {
        self.instances.insert(record.id, record);
    }

    pub fn remove(&mut self, id: &Uuid) {
        self.instances.remove(id);
    }

    /// Resolve a target string to an instance UUID.
    ///
    /// Priority:
    /// 1. Exact UUID parse
    /// 2. Exact instance name match (case-insensitive)
    /// 3. Exact track name match (case-insensitive)
    /// 4. Substring match on track name
    pub fn resolve(&self, target: &str) -> Option<Uuid> {
        if let Ok(uuid) = Uuid::parse_str(target) {
            if self.instances.contains_key(&uuid) {
                return Some(uuid);
            }
        }
        let target_lc = target.to_lowercase();
        // Pass 1: exact instance name
        for rec in self.instances.values() {
            if let Some(name) = &rec.name {
                if name.to_lowercase() == target_lc {
                    return Some(rec.id);
                }
            }
        }
        // Pass 2: exact track name
        for rec in self.instances.values() {
            if let Some(tn) = &rec.track_name {
                if tn.to_lowercase() == target_lc {
                    return Some(rec.id);
                }
            }
        }
        // Pass 3: substring on track name
        for rec in self.instances.values() {
            if let Some(tn) = &rec.track_name {
                if tn.to_lowercase().contains(&target_lc) {
                    return Some(rec.id);
                }
            }
        }
        None
    }

    pub fn summaries(&self) -> Vec<InstanceSummary> {
        self.instances
            .values()
            .map(|r| InstanceSummary {
                id: r.id,
                name: r.name.clone(),
                track_name: r.track_name.clone(),
                current_effect: r.current_effect.clone(),
                status: r.status,
            })
            .collect()
    }
}
