//! Process-wide query cancellation + a wall-clock guard.
//!
//! The desktop read/write paths hold a pooled DB connection for the life of the
//! query. Without a guard a slow statement pins that connection and hangs the tab
//! forever. [`guard`] wraps the query future in a `tokio::select!` between the
//! query, a wall-clock timeout, and an on-demand cancel signal keyed by the
//! frontend's `query_id`. On cancel/timeout the query future (and the pooled
//! connection it borrows) is dropped mid-flight; sqlx does not return a
//! connection dropped mid-statement to the pool, it closes it — so no corrupted
//! connection leaks back.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use tokio::sync::watch;
use uuid::Uuid;

use crate::error::{AppError, AppResult};

/// Default wall-clock ceiling for a desktop read/write.
pub const QUERY_TIMEOUT: Duration = Duration::from_secs(300);

// ponytail: one global lock over a small map keyed by unique v4 UUIDs. Contention
// is a non-issue at desktop scale; shard only if that ever changes.
static REGISTRY: LazyLock<Mutex<HashMap<Uuid, watch::Sender<bool>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// A live cancellation slot. Unregisters itself on drop (query finished/aborted).
pub struct CancelHandle {
    id: Uuid,
    rx: watch::Receiver<bool>,
}

impl CancelHandle {
    /// Resolves once the query is cancelled. `watch` stores the flag, so a cancel
    /// that races ahead of this await is never lost.
    pub async fn cancelled(&self) {
        let mut rx = self.rx.clone();
        let _ = rx.wait_for(|v| *v).await;
    }
}

impl Drop for CancelHandle {
    // ponytail: unique ids mean the entry we remove is always our own; if a caller
    // ever reuses an id, last-writer-wins and the older handle drops the shared slot.
    fn drop(&mut self) {
        REGISTRY.lock().unwrap().remove(&self.id);
    }
}

/// Register a cancellation slot for `id`. Drop the returned handle to unregister.
pub fn register(id: Uuid) -> CancelHandle {
    let (tx, rx) = watch::channel(false);
    REGISTRY.lock().unwrap().insert(id, tx);
    CancelHandle { id, rx }
}

/// Signal cancellation for `id`. Returns `true` iff a query was registered under it.
pub fn cancel(id: Uuid) -> bool {
    match REGISTRY.lock().unwrap().get(&id) {
        Some(tx) => {
            let _ = tx.send(true);
            true
        }
        None => false,
    }
}

/// Run `fut` under a cancellation slot (if `query_id` is set) and a wall-clock
/// timeout. Cancel or timeout drops `fut` (and its in-flight connection) and
/// returns a clear error. The slot auto-unregisters when this returns.
pub async fn guard<T, F>(query_id: Option<Uuid>, wall: Duration, fut: F) -> AppResult<T>
where
    F: std::future::Future<Output = AppResult<T>>,
{
    let handle = query_id.map(register); // dropped at fn end → unregister
    tokio::select! {
        biased;
        _ = async {
            match &handle {
                Some(h) => h.cancelled().await,
                None => std::future::pending::<()>().await,
            }
        } => Err(AppError::Safety("query cancelled".into())),
        r = tokio::time::timeout(wall, fut) => match r {
            Ok(inner) => inner,
            Err(_) => Err(AppError::Safety(format!(
                "query timed out after {}s and was aborted",
                wall.as_secs()
            ))),
        }
    }
}

/// Cancel an in-flight query by its id. `false` if nothing was running under it.
#[tauri::command]
pub async fn cancel_query(query_id: uuid::Uuid) -> bool {
    cancel(query_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_cancel_unregister() {
        let id = Uuid::new_v4();
        assert!(!cancel(id), "nothing registered yet");
        let handle = register(id);
        assert!(cancel(id), "registered → cancellable");
        drop(handle);
        assert!(!cancel(id), "dropped → auto-unregistered");
    }

    #[tokio::test]
    async fn guard_aborts_a_running_future_on_cancel() {
        let id = Uuid::new_v4();
        let task = tokio::spawn(async move {
            guard(Some(id), Duration::from_secs(30), async {
                tokio::time::sleep(Duration::from_secs(30)).await;
                Ok::<(), AppError>(())
            })
            .await
        });

        // Wait until the guard has registered, then cancel it.
        for _ in 0..200 {
            if cancel(id) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }

        let res = tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .expect("guard should return promptly after cancel")
            .unwrap();
        assert!(matches!(res, Err(AppError::Safety(_))));
    }
}
