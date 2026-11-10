//! Opt-in request scopes for diagnosing model writes. Only fixed stage names, counts and
//! monotonic durations are written to stderr; configuration values and errors are never logged.
//! The Console opens these scopes for edit requests. Background work has no scope and stays quiet.

use std::{
    future::Future,
    io::Write,
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        Arc, Mutex, OnceLock,
    },
    time::{Duration, Instant},
};

use serde::Serialize;

const MAX_OPERATIONS_PER_MINUTE: usize = 120;
const MAX_STAGES: usize = 64;

tokio::task_local! {
    static CURRENT: Arc<Context>;
}

/// Fixed operation names prevent paths, request bodies or identities from entering diagnostics.
#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    DraftPreview,
    DraftApply,
    ChainSave,
    DeploymentCreate,
    DeploymentPlan,
    DeploymentArtifacts,
    DeploymentVerify,
}

struct Budget {
    started: Instant,
    used: usize,
}

impl Budget {
    fn take(&mut self, now: Instant) -> bool {
        if now.duration_since(self.started) >= Duration::from_secs(60) {
            self.started = now;
            self.used = 0;
        }
        if self.used >= MAX_OPERATIONS_PER_MINUTE {
            return false;
        }
        self.used += 1;
        true
    }
}

fn permitted() -> bool {
    // Share the existing performance switch; Console startup already validates its spelling.
    if matches!(
        std::env::var("BROCADE_PERFORMANCE_ENABLED").as_deref(),
        Ok("false" | "0")
    ) {
        return false;
    }
    static BUDGET: OnceLock<Mutex<Budget>> = OnceLock::new();
    let budget = BUDGET.get_or_init(|| {
        Mutex::new(Budget {
            started: Instant::now(),
            used: 0,
        })
    });
    budget
        .lock()
        .is_ok_and(|mut budget| budget.take(Instant::now()))
}

struct Context {
    id: u64,
    operation: Operation,
    operations: usize,
    started: Instant,
    stages: AtomicUsize,
    #[cfg(test)]
    captured: Option<Arc<Mutex<Vec<Record>>>>,
}

#[derive(Clone, Serialize)]
struct Record {
    event: &'static str,
    operation_id: u64,
    operation: Operation,
    operations: usize,
    stage: &'static str,
    outcome: &'static str,
    stage_ms: f64,
    total_ms: f64,
}

impl Context {
    fn emit(&self, stage: &'static str, outcome: &'static str, elapsed: Duration) {
        let record = Record {
            event: "model_timing",
            operation_id: self.id,
            operation: self.operation,
            operations: self.operations,
            stage,
            outcome,
            stage_ms: elapsed.as_secs_f64() * 1000.0,
            total_ms: self.started.elapsed().as_secs_f64() * 1000.0,
        };
        #[cfg(test)]
        if let Some(captured) = &self.captured {
            captured.lock().unwrap().push(record);
            return;
        }
        if let Ok(mut line) = serde_json::to_vec(&record) {
            line.push(b'\n');
            // Diagnostics must not turn a closed stderr into a failed model transaction.
            let _ = std::io::stderr().lock().write_all(&line);
        }
    }
}

struct Stage {
    context: Arc<Context>,
    name: &'static str,
    started: Instant,
    finished: bool,
}

impl Stage {
    fn begin(context: Arc<Context>, name: &'static str) -> Option<Self> {
        if context.stages.fetch_add(1, Ordering::Relaxed) >= MAX_STAGES {
            return None;
        }
        context.emit(name, "started", Duration::ZERO);
        Some(Self {
            context,
            name,
            started: Instant::now(),
            finished: false,
        })
    }

    fn finish(mut self, success: bool) {
        self.finished = true;
        self.context.emit(
            self.name,
            if success { "ok" } else { "error" },
            self.started.elapsed(),
        );
    }
}

impl Drop for Stage {
    fn drop(&mut self) {
        if !self.finished {
            self.context
                .emit(self.name, "cancelled", self.started.elapsed());
        }
    }
}

/// Diagnose a complete operation, including errors and cancellation. A process-local numeric
/// id correlates its stages without exposing the operator, revision, URL or configuration.
pub async fn observe<T, E>(
    operation: Operation,
    operations: usize,
    future: impl Future<Output = Result<T, E>>,
) -> Result<T, E> {
    if !permitted() {
        return future.await;
    }
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    let context = Arc::new(Context {
        id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
        operation,
        operations,
        started: Instant::now(),
        stages: AtomicUsize::new(0),
        #[cfg(test)]
        captured: None,
    });
    CURRENT
        .scope(context, measure("request_total", future))
        .await
}

/// Measure an asynchronous stage. Use only a fixed stage name, never a value from a request.
pub async fn measure<T, E>(
    name: &'static str,
    future: impl Future<Output = Result<T, E>>,
) -> Result<T, E> {
    let stage = CURRENT
        .try_with(|context| Stage::begin(Arc::clone(context), name))
        .ok()
        .flatten();
    let result = future.await;
    if let Some(stage) = stage {
        stage.finish(result.is_ok());
    }
    result
}

/// Measure synchronous rendering separately from asynchronous compilation and database work.
pub fn measure_sync<T, E>(name: &'static str, run: impl FnOnce() -> Result<T, E>) -> Result<T, E> {
    let stage = CURRENT
        .try_with(|context| Stage::begin(Arc::clone(context), name))
        .ok()
        .flatten();
    let result = run();
    if let Some(stage) = stage {
        stage.finish(result.is_ok());
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context(id: u64) -> Arc<Context> {
        Arc::new(Context {
            id,
            operation: Operation::DraftApply,
            operations: 2,
            started: Instant::now(),
            stages: AtomicUsize::new(0),
            captured: Some(Arc::new(Mutex::new(Vec::new()))),
        })
    }

    #[tokio::test]
    async fn failure_and_cancellation_are_visible_without_logging_values() {
        let context = context(1);
        CURRENT
            .scope(context.clone(), async {
                let error =
                    measure("write_ops", async { Err::<(), _>("private configuration") }).await;
                assert!(error.is_err());
            })
            .await;
        let mut cancelled = Box::pin(CURRENT.scope(
            context.clone(),
            measure("commit_tx", std::future::pending::<Result<(), ()>>()),
        ));
        std::future::poll_fn(|cx| {
            assert!(cancelled.as_mut().poll(cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
        drop(cancelled);
        let records = context.captured.as_ref().unwrap().lock().unwrap();
        assert_eq!(
            records.iter().map(|r| r.outcome).collect::<Vec<_>>(),
            ["started", "error", "started", "cancelled"]
        );
        assert!(!serde_json::to_string(&*records)
            .unwrap()
            .contains("private"));
        assert!(records
            .iter()
            .all(|r| r.operation_id == 1 && r.stage_ms >= 0.0));
    }

    #[tokio::test]
    async fn concurrent_scopes_keep_their_own_stage_ids_and_have_a_fixed_bound() {
        let left = context(10);
        let right = context(20);
        let run = |context: Arc<Context>| async move {
            CURRENT
                .scope(context, async {
                    for _ in 0..MAX_STAGES + 5 {
                        measure("read_snapshot", async {
                            tokio::task::yield_now().await;
                            Ok::<_, ()>(())
                        })
                        .await
                        .unwrap();
                    }
                })
                .await;
        };
        tokio::join!(run(left.clone()), run(right.clone()));
        for context in [left, right] {
            let records = context.captured.as_ref().unwrap().lock().unwrap();
            assert_eq!(records.len(), MAX_STAGES * 2);
            assert!(records.iter().all(|r| r.operation_id == context.id));
        }
    }

    #[test]
    fn rate_budget_is_bounded_and_resets_after_one_minute() {
        let now = Instant::now();
        let mut budget = Budget {
            started: now,
            used: 0,
        };
        for _ in 0..MAX_OPERATIONS_PER_MINUTE {
            assert!(budget.take(now));
        }
        assert!(!budget.take(now));
        assert!(budget.take(now + Duration::from_secs(60)));
    }
}
