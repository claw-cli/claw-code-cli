use std::cmp::Reverse;
use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;

use rand::Rng;
use tokio::sync::RwLock;
use tracing::warn;

use super::process::UnifiedExecProcess;
use super::{MAX_PROCESSES, WARNING_PROCESSES};

struct ProcessEntry {
    process: Arc<UnifiedExecProcess>,
    last_used: std::time::Instant,
}

#[derive(Clone, Copy)]
struct ProcessPruneMeta {
    process_id: i32,
    last_used: std::time::Instant,
    exited: bool,
}

const PROTECTED_RECENT_PROCESSES: usize = 8;

pub struct ProcessStore {
    processes: RwLock<HashMap<i32, ProcessEntry>>,
    reserved_process_ids: RwLock<HashSet<i32>>,
}

impl ProcessStore {
    pub fn new() -> Self {
        ProcessStore {
            processes: RwLock::new(HashMap::new()),
            reserved_process_ids: RwLock::new(HashSet::new()),
        }
    }

    pub async fn allocate(&self, process: Arc<UnifiedExecProcess>) -> i32 {
        let Some(id) = self.reserve_process_id().await else {
            process.terminate();
            return 0;
        };
        self.insert_reserved(id, process).await;
        id
    }

    pub async fn reserve_process_id(&self) -> Option<i32> {
        let mut reserved = self.reserved_process_ids.write().await;
        let mut map = self.processes.write().await;

        if map.len() >= MAX_PROCESSES {
            self.prune_process_if_needed(&mut map);
        }

        if map.len() >= MAX_PROCESSES {
            warn!("max unified exec processes ({MAX_PROCESSES}) reached; cannot allocate process");
            return None;
        }

        if map.len() >= WARNING_PROCESSES {
            warn!(
                "unified exec processes at {}/{} (warning threshold)",
                map.len(),
                MAX_PROCESSES
            );
        }

        let id = loop {
            let candidate = rand::rng().random_range(1_000..100_000);
            if !map.contains_key(&candidate) && !reserved.contains(&candidate) {
                break candidate;
            }
        };
        reserved.insert(id);
        Some(id)
    }

    pub async fn insert_reserved(&self, id: i32, process: Arc<UnifiedExecProcess>) {
        self.reserved_process_ids.write().await.remove(&id);
        let mut map = self.processes.write().await;
        map.insert(
            id,
            ProcessEntry {
                process,
                last_used: std::time::Instant::now(),
            },
        );
    }

    pub async fn release_reserved(&self, id: i32) {
        self.reserved_process_ids.write().await.remove(&id);
    }

    pub async fn get(&self, id: i32) -> Option<Arc<UnifiedExecProcess>> {
        let mut map = self.processes.write().await;
        map.get_mut(&id).map(|entry| {
            entry.last_used = std::time::Instant::now();
            Arc::clone(&entry.process)
        })
    }

    pub async fn remove(&self, id: i32) {
        self.reserved_process_ids.write().await.remove(&id);
        let mut map = self.processes.write().await;
        if let Some(entry) = map.remove(&id) {
            entry.process.terminate();
        }
    }

    pub async fn terminate_all(&self) {
        self.reserved_process_ids.write().await.clear();
        let processes = {
            let mut map = self.processes.write().await;
            map.drain()
                .map(|(_id, entry)| entry.process)
                .collect::<Vec<_>>()
        };

        for process in processes {
            process.terminate();
        }
    }

    pub async fn len(&self) -> usize {
        self.processes.read().await.len()
    }

    pub async fn is_empty(&self) -> bool {
        self.processes.read().await.is_empty()
    }

    pub async fn prune_exited(&self) {
        let mut map = self.processes.write().await;
        self.prune_locked(&mut map);
    }

    fn prune_locked(&self, map: &mut HashMap<i32, ProcessEntry>) {
        let to_remove: Vec<i32> = map
            .iter()
            .filter(|(_, e)| !e.process.is_running())
            .map(|(id, _)| *id)
            .collect();
        for id in to_remove {
            if let Some(entry) = map.remove(&id) {
                entry.process.terminate();
            }
        }
    }

    fn prune_process_if_needed(&self, map: &mut HashMap<i32, ProcessEntry>) {
        if map.len() < MAX_PROCESSES {
            return;
        }
        let meta = map
            .iter()
            .map(|(process_id, entry)| ProcessPruneMeta {
                process_id: *process_id,
                last_used: entry.last_used,
                exited: !entry.process.is_running(),
            })
            .collect::<Vec<_>>();
        if let Some(process_id) = process_id_to_prune_from_meta(&meta)
            && let Some(entry) = map.remove(&process_id)
        {
            entry.process.terminate();
        }
    }
}

fn process_id_to_prune_from_meta(meta: &[ProcessPruneMeta]) -> Option<i32> {
    if meta.is_empty() {
        return None;
    }

    let mut by_recency = meta.to_vec();
    by_recency.sort_by_key(|entry| Reverse(entry.last_used));
    let protected = by_recency
        .iter()
        .take(PROTECTED_RECENT_PROCESSES)
        .map(|entry| entry.process_id)
        .collect::<HashSet<_>>();

    let mut lru = meta.to_vec();
    lru.sort_by_key(|entry| entry.last_used);

    if let Some(entry) = lru
        .iter()
        .find(|entry| !protected.contains(&entry.process_id) && entry.exited)
    {
        return Some(entry.process_id);
    }

    lru.into_iter()
        .find(|entry| !protected.contains(&entry.process_id))
        .map(|entry| entry.process_id)
}

impl Default for ProcessStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::unified_exec::process::UnifiedExecProcess;
    use pretty_assertions::assert_eq;
    use std::path::Path;
    use std::time::Duration;

    fn spawn_echo() -> UnifiedExecProcess {
        let (proc, _rx) = UnifiedExecProcess::spawn(
            1,
            "echo test",
            Path::new("."),
            /*shell*/ None,
            /*login*/ false,
            /*tty*/ false,
        )
        .expect("spawn should succeed");
        proc
    }

    #[tokio::test]
    async fn store_allocate_and_get() {
        let store = ProcessStore::new();
        let proc = spawn_echo();
        let id = store.allocate(Arc::new(proc)).await;
        assert!(store.get(id).await.is_some());
        assert!(store.get(9999).await.is_none());
    }

    #[tokio::test]
    async fn store_reserved_insert_preserves_process_id() {
        let store = ProcessStore::new();
        let id = store.reserve_process_id().await.expect("reserve id");
        let (proc, _rx) = UnifiedExecProcess::spawn(
            id,
            "echo test",
            Path::new("."),
            /*shell*/ None,
            /*login*/ false,
            /*tty*/ false,
        )
        .expect("spawn should succeed");

        store.insert_reserved(id, Arc::new(proc)).await;
        let proc = store.get(id).await.expect("stored process");

        assert_eq!(proc.process_id(), id);
    }

    #[tokio::test]
    async fn store_remove_terminates() {
        let store = ProcessStore::new();
        let proc = spawn_echo();
        let id = store.allocate(Arc::new(proc)).await;
        store.remove(id).await;
        assert!(store.get(id).await.is_none());
    }

    #[tokio::test]
    async fn store_terminate_all_drains_processes_and_reservations() {
        let store = ProcessStore::new();
        let reserved_id = store.reserve_process_id().await.expect("reserve id");
        let proc = Arc::new(spawn_echo());
        let id = store.allocate(Arc::clone(&proc)).await;

        store.terminate_all().await;

        assert_eq!(store.len().await, 0);
        assert!(store.get(id).await.is_none());
        assert!(!proc.is_running());
        assert!(store.reserve_process_id().await.is_some());
        store.release_reserved(reserved_id).await;
    }

    #[tokio::test]
    async fn store_len() {
        let store = ProcessStore::new();
        assert_eq!(store.len().await, 0);

        let proc = spawn_echo();
        store.allocate(Arc::new(proc)).await;
        assert_eq!(store.len().await, 1);

        let proc = spawn_echo();
        store.allocate(Arc::new(proc)).await;
        assert_eq!(store.len().await, 2);
    }

    #[tokio::test]
    async fn store_default_is_empty() {
        let store = ProcessStore::default();
        assert_eq!(store.len().await, 0);
    }

    #[tokio::test]
    async fn store_concurrent_allocate_and_get() {
        let store = Arc::new(ProcessStore::new());
        let mut handles = Vec::new();

        for _i in 0..10 {
            let s = Arc::clone(&store);
            handles.push(tokio::spawn(async move {
                let proc = spawn_echo();
                let id = s.allocate(Arc::new(proc)).await;
                assert!(s.get(id).await.is_some());
                id
            }));
        }

        let ids: Vec<i32> = futures::future::join_all(handles)
            .await
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();

        assert_eq!(ids.len(), 10);
        // All IDs should be unique
        let mut unique = ids.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), 10);
        assert_eq!(store.len().await, 10);
    }

    #[tokio::test]
    async fn store_prune_exited_removes_nothing_for_running() {
        // This test is best-effort since processes might exit quickly
        let store = ProcessStore::new();
        let proc = spawn_echo();
        let _id = store.allocate(Arc::new(proc)).await;

        // Don't prune immediately — allow process to potentially still be running
        let count_before = store.len().await;
        store.prune_exited().await;
        let count_after = store.len().await;

        // Process may have exited, but at minimum len should not increase
        assert_eq!(count_before, count_after);
        assert!(count_before >= 1);
    }

    #[test]
    fn process_prune_prefers_exited_lru_outside_protected_recent_set() {
        let now = std::time::Instant::now();
        let meta = (0..MAX_PROCESSES)
            .map(|index| ProcessPruneMeta {
                process_id: index as i32,
                last_used: now + Duration::from_millis(index as u64),
                exited: index == 10 || index == 20,
            })
            .collect::<Vec<_>>();

        assert_eq!(process_id_to_prune_from_meta(&meta), Some(10));
    }

    #[test]
    fn process_prune_protects_recent_exited_processes() {
        let now = std::time::Instant::now();
        let meta = (0..MAX_PROCESSES)
            .map(|index| ProcessPruneMeta {
                process_id: index as i32,
                last_used: now + Duration::from_millis(index as u64),
                exited: index == MAX_PROCESSES - 1,
            })
            .collect::<Vec<_>>();

        assert_eq!(process_id_to_prune_from_meta(&meta), Some(0));
    }
}
