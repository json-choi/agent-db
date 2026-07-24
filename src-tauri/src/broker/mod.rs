//! Local Broker runtime shared by the Desktop app and the `dopedb` CLI.

mod discovery;
mod dispatch;
mod peer;
mod server;
#[allow(
    dead_code,
    reason = "session issuance and revocation are consumed by the upcoming PTY Terminal manager; authentication is already active in the broker"
)]
mod session;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::services::ApplicationServices;

pub(crate) use session::BrokerSessionRegistry;

#[derive(Debug, Clone, Default)]
pub(crate) struct BrokerRuntimeStatus {
    pub(crate) running: bool,
    pub(crate) endpoint: Option<String>,
    pub(crate) runtime_file: Option<PathBuf>,
    pub(crate) last_error_kind: Option<&'static str>,
}

struct BrokerRuntimeInner {
    runtime_id: Uuid,
    sessions: BrokerSessionRegistry,
    shutdown: CancellationToken,
    status: Mutex<BrokerRuntimeStatus>,
    spawned: AtomicBool,
    stopped: AtomicBool,
    stopped_notify: Notify,
}

#[derive(Clone)]
pub(crate) struct BrokerRuntime {
    inner: Arc<BrokerRuntimeInner>,
}

impl BrokerRuntime {
    pub(crate) fn new(runtime_id: Uuid) -> Self {
        Self {
            inner: Arc::new(BrokerRuntimeInner {
                runtime_id,
                sessions: BrokerSessionRegistry::new(runtime_id),
                shutdown: CancellationToken::new(),
                status: Mutex::new(BrokerRuntimeStatus::default()),
                spawned: AtomicBool::new(false),
                stopped: AtomicBool::new(false),
                stopped_notify: Notify::new(),
            }),
        }
    }

    pub(crate) fn runtime_id(&self) -> Uuid {
        self.inner.runtime_id
    }

    pub(crate) fn sessions(&self) -> &BrokerSessionRegistry {
        &self.inner.sessions
    }

    pub(crate) fn prepare_start(&self) -> bool {
        self.inner
            .spawned
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }

    pub(crate) fn mark_running(&self, endpoint: String, runtime_file: PathBuf) {
        let mut status = self.inner.status.lock().unwrap();
        status.running = true;
        status.endpoint = Some(endpoint);
        status.runtime_file = Some(runtime_file);
        status.last_error_kind = None;
    }

    pub(crate) fn finish(&self, error: Option<&crate::AppError>) {
        {
            let mut status = self.inner.status.lock().unwrap();
            status.running = false;
            status.endpoint = None;
            status.last_error_kind = error.map(crate::AppError::kind);
        }
        self.inner.stopped.store(true, Ordering::SeqCst);
        self.inner.stopped_notify.notify_waiters();
    }

    pub(crate) fn shutdown_token(&self) -> &CancellationToken {
        &self.inner.shutdown
    }

    pub(crate) fn shutdown(&self) {
        self.inner.sessions.revoke_all();
        self.inner.shutdown.cancel();
    }

    pub(crate) async fn shutdown_and_wait(&self, timeout: Duration) {
        self.shutdown();
        if !self.inner.spawned.load(Ordering::SeqCst) || self.inner.stopped.load(Ordering::SeqCst) {
            return;
        }
        let _ = tokio::time::timeout(timeout, self.inner.stopped_notify.notified()).await;
    }
}

pub(crate) fn start(
    runtime: BrokerRuntime,
    services: ApplicationServices,
    app_handle: tauri::AppHandle,
) {
    if !runtime.prepare_start() {
        return;
    }
    tauri::async_runtime::spawn(async move {
        if let Err(error) = server::serve(runtime.clone(), services, app_handle).await {
            tracing::error!(error_kind = error.kind(), "local broker stopped");
        }
    });
}
