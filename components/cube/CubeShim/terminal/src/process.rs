//! Admission may be released only after the Guest confirms process exit.
use async_trait::async_trait;
use std::time::Duration;

#[derive(Clone, Copy)]
pub enum State {
    Reserved,
    Started,
    Exited(u32),
}

#[async_trait]
pub trait Process: Send {
    async fn kill(&mut self) -> Result<(), String>;
    /// Returns a Guest exit receipt, never cached defaults or a transport failure code.
    async fn wait(&mut self) -> Result<u32, String>;
    async fn close_io(&mut self) -> Result<(), String>;
    async fn delete(&mut self) -> Result<(), String>;
}

pub async fn cleanup(process: &mut impl Process, state: State) -> Result<Option<u32>, String> {
    tokio::time::timeout(Duration::from_secs(5), async {
        let exit = match state {
            State::Reserved => None,
            State::Exited(code) => Some(code),
            State::Started => {
                process.kill().await?;
                Some(process.wait().await?)
            }
        };
        if exit.is_some() {
            // Exited processes may already have closed stdin. Delete remains required.
            let _ = process.close_io().await;
        }
        process.delete().await?;
        Ok(exit)
    })
    .await
    .map_err(|_| "persistent terminal cleanup timed out")?
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tokio::sync::Notify;

    struct FakeProcess {
        calls: Arc<Mutex<Vec<&'static str>>>,
        exited: Arc<Notify>,
        fail_wait: bool,
    }
    #[async_trait]
    impl Process for FakeProcess {
        async fn kill(&mut self) -> Result<(), String> {
            self.calls.lock().unwrap().push("kill");
            Ok(())
        }
        async fn wait(&mut self) -> Result<u32, String> {
            self.calls.lock().unwrap().push("wait");
            if self.fail_wait {
                return Err("Guest exit unconfirmed".into());
            }
            self.exited.notified().await;
            Ok(137)
        }
        async fn close_io(&mut self) -> Result<(), String> {
            self.calls.lock().unwrap().push("close");
            Ok(())
        }
        async fn delete(&mut self) -> Result<(), String> {
            self.calls.lock().unwrap().push("delete");
            Ok(())
        }
    }
    #[tokio::test]
    async fn kill_acknowledgement_does_not_complete_cleanup_before_exit() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let exited = Arc::new(Notify::new());
        let mut process = FakeProcess {
            calls: calls.clone(),
            exited: exited.clone(),
            fail_wait: false,
        };
        let cleanup = tokio::spawn(async move { cleanup(&mut process, State::Started).await });
        tokio::time::timeout(Duration::from_secs(1), async {
            while !calls.lock().unwrap().contains(&"wait") {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(!cleanup.is_finished());
        assert_eq!(*calls.lock().unwrap(), ["kill", "wait"]);
        exited.notify_one();
        assert_eq!(cleanup.await.unwrap().unwrap(), Some(137));
        assert_eq!(*calls.lock().unwrap(), ["kill", "wait", "close", "delete"]);
    }
    #[tokio::test]
    async fn missing_exit_receipt_never_deletes_the_exec() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let mut process = FakeProcess {
            calls: calls.clone(),
            exited: Arc::new(Notify::new()),
            fail_wait: true,
        };
        assert!(cleanup(&mut process, State::Started).await.is_err());
        assert_eq!(*calls.lock().unwrap(), ["kill", "wait"]);
        assert_eq!(cleanup(&mut process, State::Reserved).await.unwrap(), None);
        assert_eq!(*calls.lock().unwrap(), ["kill", "wait", "delete"]);
    }
}
