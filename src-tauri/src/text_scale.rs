use serde::Serialize;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc, Mutex,
};

pub const CHANGED_EVENT: &str = "system-text-scale-updated";

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct TextScaleSnapshot {
    pub factor: f64,
    pub revision: u64,
}

impl Default for TextScaleSnapshot {
    fn default() -> Self {
        Self {
            factor: 1.0,
            revision: 0,
        }
    }
}

fn normalized_factor(value: f64) -> f64 {
    if value.is_finite() {
        value.clamp(1.0, 2.25)
    } else {
        1.0
    }
}

#[derive(Default)]
struct SharedState {
    source_gate: Mutex<()>,
    snapshot: Mutex<TextScaleSnapshot>,
    stopping: AtomicBool,
    ready: AtomicBool,
    stopped: AtomicBool,
}

impl SharedState {
    fn update(&self, read: impl FnOnce() -> Option<f64>) -> Option<TextScaleSnapshot> {
        let _source_guard = self
            .source_gate
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if self.stopping.load(Ordering::Acquire) {
            return None;
        }
        let factor = normalized_factor(read()?);
        if self.stopping.load(Ordering::Acquire) {
            return None;
        }
        let mut snapshot = self
            .snapshot
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if snapshot.factor == factor {
            return None;
        }
        snapshot.factor = factor;
        snapshot.revision = snapshot.revision.saturating_add(1);
        Some(*snapshot)
    }
}

pub struct SystemTextScale {
    state: Arc<SharedState>,
    stop_sender: mpsc::SyncSender<()>,
}

impl SystemTextScale {
    pub fn start(publish: impl Fn(TextScaleSnapshot) + Send + Sync + 'static) -> Self {
        let state = Arc::new(SharedState::default());
        let (stop_sender, stop_receiver) = mpsc::sync_channel(1);
        let worker_state = state.clone();
        let result = std::thread::Builder::new()
            .name("juice-text-scale".into())
            .spawn(move || {
                if let Err(error) = run_watcher(worker_state.clone(), stop_receiver, publish) {
                    eprintln!("[text-scale] system text size watcher unavailable: {error:#}");
                }
                worker_state.ready.store(true, Ordering::Release);
                worker_state.stopped.store(true, Ordering::Release);
            });
        if let Err(error) = result {
            eprintln!("[text-scale] worker unavailable: {error}");
            state.ready.store(true, Ordering::Release);
            state.stopped.store(true, Ordering::Release);
        }
        Self { state, stop_sender }
    }

    pub fn snapshot(&self) -> TextScaleSnapshot {
        *self
            .state
            .snapshot
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }

    pub fn stop(&self) {
        if !self.state.stopping.swap(true, Ordering::AcqRel) {
            let _ = self.stop_sender.try_send(());
        }
    }
}

impl Drop for SystemTextScale {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(windows)]
fn run_watcher(
    state: Arc<SharedState>,
    stop: mpsc::Receiver<()>,
    publish: impl Fn(TextScaleSnapshot) + Send + Sync + 'static,
) -> anyhow::Result<()> {
    use windows::{
        core::IInspectable,
        Foundation::TypedEventHandler,
        Win32::System::WinRT::{RoInitialize, RoUninitialize, RO_INIT_MULTITHREADED},
        UI::ViewManagement::UISettings,
    };
    unsafe {
        RoInitialize(RO_INIT_MULTITHREADED)?;
    }
    struct Apartment;
    impl Drop for Apartment {
        fn drop(&mut self) {
            unsafe {
                RoUninitialize();
            }
        }
    }
    let _apartment = Apartment;
    let settings = UISettings::new()?;
    let publish = Arc::new(publish);
    let event_state = state.clone();
    let event_publish = publish.clone();
    let handler = TypedEventHandler::<UISettings, IInspectable>::new(move |sender, _| {
        if let Some(settings) = sender.as_ref() {
            if let Some(snapshot) = event_state.update(|| settings.TextScaleFactor().ok()) {
                event_publish(snapshot);
            }
        }
        Ok(())
    });
    let token = settings.TextScaleFactorChanged(&handler)?;
    if let Some(snapshot) = state.update(|| settings.TextScaleFactor().ok()) {
        publish(snapshot);
    }
    state.ready.store(true, Ordering::Release);
    // Keep the WinRT subscription alive without polling or blocking the UI thread.
    let _ = stop.recv();
    settings.RemoveTextScaleFactorChanged(token)?;
    Ok(())
}

#[cfg(not(windows))]
fn run_watcher(
    state: Arc<SharedState>,
    stop: mpsc::Receiver<()>,
    _publish: impl Fn(TextScaleSnapshot) + Send + Sync + 'static,
) -> anyhow::Result<()> {
    state.ready.store(true, Ordering::Release);
    let _ = stop.recv();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scale_updates_are_bounded_versioned_and_ignore_failures_or_shutdown() {
        let state = SharedState::default();
        assert_eq!(state.update(|| Some(1.0)), None);
        assert_eq!(state.update(|| Some(1.5)).unwrap().revision, 1);
        assert_eq!(state.update(|| None), None);
        assert_eq!(state.update(|| Some(1.5)), None);
        assert_eq!(state.update(|| Some(9.0)).unwrap().factor, 2.25);
        assert_eq!(state.update(|| Some(f64::NAN)).unwrap().factor, 1.0);
        state.stopping.store(true, Ordering::Release);
        assert_eq!(
            state.update(|| panic!("must not read during shutdown")),
            None
        );
        assert_eq!(normalized_factor(f64::INFINITY), 1.0);
        assert_eq!(normalized_factor(-1.0), 1.0);
    }

    #[cfg(windows)]
    #[test]
    fn native_text_scale_subscription_starts_and_stops_without_changing_settings() {
        let monitor = SystemTextScale::start(|_| {});
        let wait = |flag: &AtomicBool| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
            while !flag.load(Ordering::Acquire) && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            assert!(flag.load(Ordering::Acquire));
        };
        wait(&monitor.state.ready);
        assert!(
            !monitor.state.stopped.load(Ordering::Acquire),
            "WinRT subscription failed"
        );
        assert!((1.0..=2.25).contains(&monitor.snapshot().factor));
        monitor.stop();
        monitor.stop();
        wait(&monitor.state.stopped);
    }

    #[test]
    fn slow_system_read_does_not_block_the_ui_snapshot() {
        let state = Arc::new(SharedState::default());
        let (started, started_rx) = mpsc::channel();
        let (finish, finish_rx) = mpsc::channel();
        let reader_state = state.clone();
        let worker = std::thread::spawn(move || {
            reader_state.update(|| {
                started.send(()).unwrap();
                finish_rx.recv().unwrap();
                Some(1.5)
            })
        });
        started_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .unwrap();
        assert_eq!(state.snapshot.try_lock().unwrap().factor, 1.0);
        finish.send(()).unwrap();
        assert_eq!(worker.join().unwrap().unwrap().factor, 1.5);
    }
}
