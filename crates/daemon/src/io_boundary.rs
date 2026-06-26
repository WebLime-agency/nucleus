use anyhow::Context;

pub(crate) async fn run_job_blocking<F, T>(operation: &'static str, task: F) -> anyhow::Result<T>
where
    F: FnOnce() -> anyhow::Result<T> + Send + 'static,
    T: Send + 'static,
{
    // TODO(#482): record slow blocking I/O operations so health can report a degraded state.
    tokio::task::spawn_blocking(task)
        .await
        .with_context(|| format!("blocking job I/O task '{operation}' panicked or was canceled"))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn job_blocking_operations_do_not_starve_async_tasks() {
        let blocking = tokio::spawn(run_job_blocking("test sleep", || {
            std::thread::sleep(Duration::from_millis(250));
            Ok(())
        }));

        let probe = tokio::time::timeout(Duration::from_millis(75), async {
            tokio::task::yield_now().await;
            1
        })
        .await;

        assert_eq!(probe.expect("async probe should not be starved"), 1);
        blocking
            .await
            .expect("blocking task should join")
            .expect("blocking task should succeed");
    }
}
