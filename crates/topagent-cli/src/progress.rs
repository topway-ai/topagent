use anyhow::{Context, Result};
use crate::commands::surface::PRODUCT_NAME;
use std::sync::{Arc, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use topagent_core::{ProgressCallback, ProgressKind, ProgressUpdate, TelegramAdapter};
use tracing::error;

type RenderFn = Box<dyn Fn(&ProgressUpdate, Duration, bool) + Send + 'static>;

pub struct LiveProgress {
    callback: ProgressCallback,
    worker: Option<JoinHandle<()>>,
}

impl LiveProgress {
    fn new(
        interval: Duration,
        initial: ProgressUpdate,
        render: RenderFn,
        render_initial: bool,
    ) -> Self {
        let (tx, rx) = mpsc::channel::<ProgressUpdate>();
        let callback: ProgressCallback = Arc::new(move |update| {
            let _ = tx.send(update);
        });

        let worker = thread::spawn(move || {
            let started_at = Instant::now();
            let mut latest = initial;
            let mut last_rendered_at = Instant::now();
            let mut pending_render = false;

            if render_initial {
                render(&latest, Duration::ZERO, false);
                last_rendered_at = Instant::now();
            }

            loop {
                match rx.recv_timeout(interval) {
                    Ok(update) => {
                        let changed = update != latest;
                        latest = update;
                        if latest.is_terminal() {
                            render(&latest, started_at.elapsed(), false);
                            break;
                        }
                        if changed {
                            if last_rendered_at.elapsed() >= interval
                                || matches!(
                                    latest.kind,
                                    ProgressKind::Stopping | ProgressKind::Blocked
                                )
                            {
                                // Enough time passed — render now.
                                render(&latest, started_at.elapsed(), false);
                                last_rendered_at = Instant::now();
                                pending_render = false;
                            } else {
                                // Too soon — defer to next heartbeat.
                                pending_render = true;
                            }
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        if latest.is_terminal() {
                            break;
                        }
                        // Render on heartbeat: show pending state change or "still working".
                        render(&latest, started_at.elapsed(), !pending_render);
                        last_rendered_at = Instant::now();
                        pending_render = false;
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
        });

        Self {
            callback,
            worker: Some(worker),
        }
    }

    pub fn for_cli(interval: Duration) -> Self {
        Self::new(
            interval,
            ProgressUpdate::received(),
            Box::new(|update, elapsed, heartbeat| {
                eprintln!("status: {}", format_cli_status(update, elapsed, heartbeat));
            }),
            true,
        )
    }

    pub fn for_telegram(
        interval: Duration,
        adapter: TelegramAdapter,
        chat_id: i64,
    ) -> Result<Self> {
        let initial = ProgressUpdate::received();
        let status_text = format_telegram_status(&initial, Duration::ZERO, false);
        let status_message = adapter
            .send_message_to_chat(chat_id, &status_text)
            .context("failed to send Telegram status message")?;
        let status_message_id = status_message.message_id;

        Ok(Self::new(
            interval,
            initial,
            Box::new(move |update, elapsed, heartbeat| {
                let text = format_telegram_status(update, elapsed, heartbeat);
                if let Err(err) = adapter.edit_message_text(chat_id, status_message_id, &text, None)
                {
                    let message = err.to_string();
                    if !message.contains("message is not modified") {
                        error!("failed to update Telegram status message: {}", err);
                    }
                }
            }),
            false,
        ))
    }

    pub fn callback(&self) -> ProgressCallback {
        self.callback.clone()
    }

    pub fn wait(mut self) {
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn format_cli_status(update: &ProgressUpdate, elapsed: Duration, heartbeat: bool) -> String {
    let seconds = elapsed.as_secs();
    match update.kind {
        ProgressKind::Completed => format!("completed in {}s", seconds),
        ProgressKind::Failed => {
            format!("failed after {}s: {}", seconds, trim_failed_message(update))
        }
        ProgressKind::Stopped => format!("stopped after {}s", seconds),
        ProgressKind::Stopping => {
            if heartbeat {
                format!("still stopping ({}s): {}", seconds, update.message)
            } else {
                update.message.clone()
            }
        }
        ProgressKind::Blocked => {
            if heartbeat {
                format!("still blocked ({}s): {}", seconds, update.message)
            } else {
                update.message.clone()
            }
        }
        _ => {
            if heartbeat {
                format!("still working ({}s): {}", seconds, update.message)
            } else {
                update.message.clone()
            }
        }
    }
}

fn format_telegram_status(update: &ProgressUpdate, elapsed: Duration, heartbeat: bool) -> String {
    let seconds = elapsed.as_secs();
    match update.kind {
        ProgressKind::Completed => format!("{PRODUCT_NAME} completed in {}s.", seconds),
        ProgressKind::Failed => format!(
            "{PRODUCT_NAME} failed after {}s.\n{}",
            seconds,
            trim_failed_message(update)
        ),
        ProgressKind::Stopped => format!("{PRODUCT_NAME} stopped after {}s.", seconds),
        ProgressKind::Stopping => format!(
            "{PRODUCT_NAME} is stopping.\n{}\nElapsed: {}s",
            update.message, seconds
        ),
        ProgressKind::Blocked => {
            let heading = if heartbeat {
                format!("{PRODUCT_NAME} is still blocked.")
            } else {
                format!("{PRODUCT_NAME} is blocked.")
            };
            format!("{}\n{}\nElapsed: {}s", heading, update.message, seconds)
        }
        ProgressKind::Retrying => format!(
            "{PRODUCT_NAME} is retrying.\n{}\nElapsed: {}s",
            update.message, seconds
        ),
        _ => {
            let heading = if heartbeat {
                format!("{PRODUCT_NAME} is still working.")
            } else {
                format!("{PRODUCT_NAME} is working.")
            };
            format!("{}\n{}\nElapsed: {}s", heading, update.message, seconds)
        }
    }
}

fn trim_failed_message(update: &ProgressUpdate) -> &str {
    update
        .message
        .strip_prefix("Failed: ")
        .unwrap_or(update.message.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn capture_progress(interval: Duration) -> (LiveProgress, Arc<Mutex<Vec<String>>>) {
        let rendered = Arc::new(Mutex::new(Vec::new()));
        let sink = rendered.clone();
        let progress = LiveProgress::new(
            interval,
            ProgressUpdate::received(),
            Box::new(move |update, elapsed, heartbeat| {
                sink.lock()
                    .unwrap()
                    .push(format_cli_status(update, elapsed, heartbeat));
            }),
            true,
        );
        (progress, rendered)
    }

    #[test]
    fn test_live_progress_emits_heartbeat_for_long_running_work() {
        let (progress, rendered) = capture_progress(Duration::from_millis(25));
        (progress.callback())(ProgressUpdate::researching());
        std::thread::sleep(Duration::from_millis(60));
        (progress.callback())(ProgressUpdate::completed());
        progress.wait();

        let rendered = rendered.lock().unwrap();
        assert!(rendered.iter().any(|line| line.contains("still working")));
        assert!(rendered.iter().any(|line| line.contains("completed")));
    }

    #[test]
    fn test_live_progress_renders_stopped_terminal_state() {
        let (progress, rendered) = capture_progress(Duration::from_millis(25));
        (progress.callback())(ProgressUpdate::stopped());
        progress.wait();

        let rendered = rendered.lock().unwrap();
        assert!(rendered.iter().any(|line| line.contains("stopped after")));
    }

    #[test]
    fn test_live_progress_renders_stopping_state() {
        let (progress, rendered) = capture_progress(Duration::from_millis(25));
        (progress.callback())(ProgressUpdate::stopping());
        std::thread::sleep(Duration::from_millis(30));
        (progress.callback())(ProgressUpdate::stopped());
        progress.wait();

        let rendered = rendered.lock().unwrap();
        assert!(rendered.iter().any(|line| line.contains("Stopping after")));
        assert!(rendered.iter().any(|line| line.contains("stopped after")));
    }

    #[test]
    fn test_live_progress_renders_blocked_state_immediately() {
        let (progress, rendered) = capture_progress(Duration::from_millis(100));
        (progress.callback())(ProgressUpdate::blocked("Blocked: approval required."));
        std::thread::sleep(Duration::from_millis(20));
        (progress.callback())(ProgressUpdate::stopped());
        progress.wait();

        let rendered = rendered.lock().unwrap();
        assert!(
            rendered
                .iter()
                .any(|line| line.contains("Blocked: approval required."))
        );
    }

    #[test]
    fn test_live_progress_dedupes_identical_retry_updates() {
        let (progress, rendered) = capture_progress(Duration::from_millis(100));
        let retry = ProgressUpdate::retrying("Telegram polling failed, retrying connection...");

        (progress.callback())(retry.clone());
        (progress.callback())(retry.clone());
        (progress.callback())(retry);
        // Wait for the heartbeat to flush the deferred retry update.
        std::thread::sleep(Duration::from_millis(120));
        (progress.callback())(ProgressUpdate::completed());
        progress.wait();

        let rendered = rendered.lock().unwrap();
        let retry_lines = rendered
            .iter()
            .filter(|line| line.contains("Telegram polling failed, retrying connection"))
            .count();
        assert_eq!(retry_lines, 1, "identical retry updates should not spam");
        assert!(rendered.iter().any(|line| line.contains("completed")));
    }
}
