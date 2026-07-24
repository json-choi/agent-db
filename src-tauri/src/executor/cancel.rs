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

use std::collections::hash_map::Entry;
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
struct CancelSlot {
    sender: watch::Sender<bool>,
    handles: usize,
}

static REGISTRY: LazyLock<Mutex<HashMap<Uuid, CancelSlot>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// A live cancellation slot. Unregisters itself on drop (query finished/aborted).
pub struct CancelHandle {
    id: Uuid,
    rx: watch::Receiver<bool>,
}

impl CancelHandle {
    /// Exact operation/query identity that owns this cancellation slot.
    pub const fn id(&self) -> Uuid {
        self.id
    }

    /// Return the currently stored signal without waiting.
    pub fn is_cancelled(&self) -> bool {
        *self.rx.borrow()
    }

    /// Resolves once the query is cancelled. `watch` stores the flag, so a cancel
    /// that races ahead of this await is never lost.
    pub async fn cancelled(&self) {
        let mut rx = self.rx.clone();
        let _ = rx.wait_for(|v| *v).await;
    }
}

impl Drop for CancelHandle {
    fn drop(&mut self) {
        let mut registry = REGISTRY.lock().unwrap();
        let Entry::Occupied(mut entry) = registry.entry(self.id) else {
            return;
        };
        if entry.get().handles > 1 {
            entry.get_mut().handles -= 1;
        } else {
            entry.remove();
        }
    }
}

/// Register or join the cancellation slot for `id`. Concurrent claim attempts for
/// one immutable operation share the same signal, and the slot remains registered
/// until every handle has dropped.
pub fn register(id: Uuid) -> CancelHandle {
    let mut registry = REGISTRY.lock().unwrap();
    let rx = match registry.entry(id) {
        Entry::Occupied(mut entry) => {
            entry.get_mut().handles += 1;
            entry.get().sender.subscribe()
        }
        Entry::Vacant(entry) => {
            let (sender, receiver) = watch::channel(false);
            entry.insert(CancelSlot { sender, handles: 1 });
            receiver
        }
    };
    CancelHandle { id, rx }
}

/// Signal cancellation for `id`. Returns `true` iff a query was registered under it.
pub fn cancel(id: Uuid) -> bool {
    match REGISTRY.lock().unwrap().get(&id) {
        Some(slot) => {
            let _ = slot.sender.send(true);
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
    guard_registered(handle.as_ref(), wall, fut).await
}

/// Run under a cancellation slot that was registered before an async preparation
/// boundary. This keeps a cancel signal sent immediately after an operation claim
/// from being lost before target execution begins.
pub async fn guard_registered<T, F>(
    handle: Option<&CancelHandle>,
    wall: Duration,
    fut: F,
) -> AppResult<T>
where
    F: std::future::Future<Output = AppResult<T>>,
{
    tokio::select! {
        biased;
        _ = async {
            match handle {
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
        assert!(!handle.is_cancelled());
        assert!(cancel(id), "registered → cancellable");
        assert!(handle.is_cancelled());
        drop(handle);
        assert!(!cancel(id), "dropped → auto-unregistered");
    }

    #[test]
    fn duplicate_handles_share_one_signal_and_do_not_unregister_each_other() {
        let id = Uuid::new_v4();
        let first = register(id);
        let second = register(id);

        drop(second);
        assert!(
            cancel(id),
            "the surviving execution handle must remain cancellable"
        );
        assert!(first.is_cancelled());

        drop(first);
        assert!(!cancel(id), "the final handle removes the shared slot");
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
