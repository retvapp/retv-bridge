//! Starting the private adb server again after it died (killed by another
//! program, crashed): an operation that finds no server
//! (`adb-server-unreachable`) asks for one restart and is retried once.
//! At most one restart per gap (30 s); after that the error stands, which
//! the site shows as "adb is down". Never once stopped (the app is quitting).

use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use retv_bridge_core::api::TransportError;
use tokio::time::Instant;

use crate::bridge_adb::BoxFuture;

/// Starts the server again (`start_adb_server` on 5039 in the app);
/// `true` when it is up.
pub type Restarter = Arc<dyn Fn() -> BoxFuture<'static, bool> + Send + Sync>;

/// The shortest time between two restarts.
pub const REVIVE_GAP: Duration = Duration::from_secs(30);

pub struct ServerRevive {
    restart: Restarter,
    gap: Duration,
    /// When the last restart began, and whether it worked.
    last: tokio::sync::Mutex<Option<(Instant, bool)>>,
    stopped: AtomicBool,
}

impl ServerRevive {
    pub fn new(restart: Restarter) -> ServerRevive {
        ServerRevive {
            restart,
            gap: REVIVE_GAP,
            last: tokio::sync::Mutex::new(None),
            stopped: AtomicBool::new(false),
        }
    }

    pub fn with_gap(mut self, gap: Duration) -> ServerRevive {
        self.gap = gap;
        self
    }

    /// No restart from now on: the app is quitting (or updating).
    pub fn stop(&self) {
        self.stopped.store(true, Ordering::SeqCst);
    }

    pub fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
    }

    /// Whether the server may be up again, so the failed call is worth one
    /// retry. Calls that fail together share one restart; within the gap the
    /// last restart's outcome stands.
    pub async fn revive(&self) -> bool {
        if self.is_stopped() {
            return false;
        }
        let mut last = self.last.lock().await;
        if let Some((at, ok)) = *last {
            if at.elapsed() < self.gap {
                return ok;
            }
        }
        if self.is_stopped() {
            return false;
        }
        let at = Instant::now();
        let ok = (self.restart)().await;
        *last = Some((at, ok));
        ok
    }
}

/// Runs `op`; on `AdbServerUnreachable`, revives the server and runs it once more.
pub(crate) async fn retry_once<T, F, Fut>(
    revive: Option<&ServerRevive>,
    op: F,
) -> Result<T, TransportError>
where
    F: Fn() -> Fut,
    Fut: Future<Output = Result<T, TransportError>>,
{
    retry_once_after(revive, op, || async {}).await
}

/// `retry_once`, with `after` run between a successful revive and the retry
/// (a TV command reconnects its TV first: a new server has no TVs).
pub(crate) async fn retry_once_after<T, F, Fut, A, AFut>(
    revive: Option<&ServerRevive>,
    op: F,
    after: A,
) -> Result<T, TransportError>
where
    F: Fn() -> Fut,
    Fut: Future<Output = Result<T, TransportError>>,
    A: FnOnce() -> AFut,
    AFut: Future<Output = ()>,
{
    match op().await {
        Err(TransportError::AdbServerUnreachable) => match revive {
            Some(r) if r.revive().await => {
                after().await;
                op().await
            }
            _ => Err(TransportError::AdbServerUnreachable),
        },
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    fn counting(ok: bool) -> (Restarter, Arc<AtomicUsize>) {
        let n = Arc::new(AtomicUsize::new(0));
        let c = n.clone();
        let r: Restarter = Arc::new(move || -> BoxFuture<'static, bool> {
            let c = c.clone();
            Box::pin(async move {
                c.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(50)).await;
                ok
            })
        });
        (r, n)
    }

    #[tokio::test]
    async fn calls_failing_together_share_one_restart() {
        let (r, n) = counting(true);
        let revive = ServerRevive::new(r);
        let (a, b, c) = tokio::join!(revive.revive(), revive.revive(), revive.revive());
        assert!(a && b && c);
        assert_eq!(n.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn one_restart_per_gap() {
        let (r, n) = counting(false);
        let revive = ServerRevive::new(r);
        assert!(!revive.revive().await);
        tokio::time::advance(REVIVE_GAP - Duration::from_secs(1)).await;
        assert!(!revive.revive().await);
        assert_eq!(n.load(Ordering::SeqCst), 1);
        tokio::time::advance(Duration::from_secs(2)).await;
        assert!(!revive.revive().await);
        assert_eq!(n.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn stopped_never_restarts() {
        let (r, n) = counting(true);
        let revive = ServerRevive::new(r);
        revive.stop();
        assert!(!revive.revive().await);
        assert_eq!(n.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn only_server_unreachable_is_retried() {
        let (r, n) = counting(true);
        let revive = ServerRevive::new(r);
        let tries = AtomicUsize::new(0);
        let got: Result<(), _> = retry_once(Some(&revive), || async {
            tries.fetch_add(1, Ordering::SeqCst);
            Err(TransportError::DeviceOffline)
        })
        .await;
        assert_eq!(got, Err(TransportError::DeviceOffline));
        assert_eq!(
            (tries.load(Ordering::SeqCst), n.load(Ordering::SeqCst)),
            (1, 0)
        );

        let got: Result<u32, _> = retry_once(Some(&revive), || async {
            if tries.fetch_add(1, Ordering::SeqCst) == 1 {
                Err(TransportError::AdbServerUnreachable)
            } else {
                Ok(7)
            }
        })
        .await;
        assert_eq!(got, Ok(7));
        assert_eq!(
            (tries.load(Ordering::SeqCst), n.load(Ordering::SeqCst)),
            (3, 1)
        );
    }
}
