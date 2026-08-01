use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc, Mutex,
};
use tokio::sync::watch;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ActivitySnapshot {
    pub active: bool,
    pub generation: u64,
}

impl Default for ActivitySnapshot {
    fn default() -> Self {
        Self {
            active: true,
            generation: 0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ActivitySignal {
    SessionLocked,
    SessionUnlocked,
    DisplayOff,
    DisplayOn,
    DisplayDimmed,
}

fn display_signal(value: u32) -> Option<ActivitySignal> {
    match value {
        0 => Some(ActivitySignal::DisplayOff),
        1 => Some(ActivitySignal::DisplayOn),
        2 => Some(ActivitySignal::DisplayDimmed),
        _ => None,
    }
}

fn initial_session_signal(
    session_registered: bool,
    current_session_locked: Option<bool>,
) -> ActivitySignal {
    if session_registered && current_session_locked == Some(true) {
        ActivitySignal::SessionLocked
    } else {
        ActivitySignal::SessionUnlocked
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct ActivityInputs {
    session_locked: bool,
    display_off: bool,
}

impl ActivityInputs {
    fn active(self) -> bool {
        !self.session_locked && !self.display_off
    }

    fn apply(&mut self, signal: ActivitySignal) -> bool {
        let previous = *self;
        match signal {
            ActivitySignal::SessionLocked => self.session_locked = true,
            ActivitySignal::SessionUnlocked => self.session_locked = false,
            ActivitySignal::DisplayOff => self.display_off = true,
            ActivitySignal::DisplayOn | ActivitySignal::DisplayDimmed => self.display_off = false,
        }
        *self != previous
    }
}

struct WatcherContext {
    inputs: Mutex<ActivityInputs>,
    sender: watch::Sender<ActivitySnapshot>,
    display_state_seen: AtomicBool,
    publish_gate: Arc<Mutex<()>>,
}

impl WatcherContext {
    fn signal(&self, signal: ActivitySignal) {
        let _publish_guard = self
            .publish_gate
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let mut inputs = self.inputs.lock().unwrap_or_else(|err| err.into_inner());
        if !inputs.apply(signal) {
            return;
        }
        let generation = self.sender.borrow().generation.wrapping_add(1);
        self.sender.send_replace(ActivitySnapshot {
            active: inputs.active(),
            generation,
        });
    }
}

#[derive(Clone)]
pub struct SystemActivityShutdown {
    stop_sender: mpsc::SyncSender<()>,
    shutdown_requested: Arc<AtomicBool>,
}

impl SystemActivityShutdown {
    pub fn stop(&self) {
        if !self.shutdown_requested.swap(true, Ordering::AcqRel) {
            let _ = self.stop_sender.try_send(());
        }
    }
}

pub struct SystemActivityMonitor {
    receiver: Option<watch::Receiver<ActivitySnapshot>>,
    _watcher_ready: Arc<AtomicBool>,
    _watcher_stopped: Arc<AtomicBool>,
    shutdown_requested: Arc<AtomicBool>,
    publish_gate: Arc<Mutex<()>>,
}

impl SystemActivityMonitor {
    pub fn start() -> (Self, SystemActivityShutdown) {
        let (sender, receiver) = watch::channel(ActivitySnapshot::default());
        let (stop_sender, stop_receiver) = mpsc::sync_channel(1);
        let watcher_ready = Arc::new(AtomicBool::new(false));
        let watcher_stopped = Arc::new(AtomicBool::new(false));
        let shutdown_requested = Arc::new(AtomicBool::new(false));
        let publish_gate = Arc::new(Mutex::new(()));
        let thread_ready = watcher_ready.clone();
        let thread_stopped = watcher_stopped.clone();
        let thread_publish_gate = publish_gate.clone();
        let spawn_result = std::thread::Builder::new()
            .name("juice-system-activity".into())
            .spawn(move || {
                if let Err(err) =
                    run_native_watcher(sender, thread_ready, thread_publish_gate, stop_receiver)
                {
                    eprintln!("[system-activity] watcher stopped: {err:#}");
                }
                thread_stopped.store(true, Ordering::Release);
            });

        let watcher_started = match spawn_result {
            Ok(_thread) => true,
            Err(err) => {
                eprintln!(
                    "[system-activity] watcher unavailable; collection remains enabled: {err}"
                );
                watcher_stopped.store(true, Ordering::Release);
                false
            }
        };

        (
            Self {
                receiver: watcher_started.then_some(receiver),
                _watcher_ready: watcher_ready,
                _watcher_stopped: watcher_stopped,
                shutdown_requested: shutdown_requested.clone(),
                publish_gate,
            },
            SystemActivityShutdown {
                stop_sender,
                shutdown_requested,
            },
        )
    }

    pub fn is_shutdown(&self) -> bool {
        self.shutdown_requested.load(Ordering::Acquire)
    }

    pub fn snapshot(&self) -> ActivitySnapshot {
        if self.is_shutdown() {
            return ActivitySnapshot {
                active: false,
                generation: self
                    .receiver
                    .as_ref()
                    .map_or(0, |receiver| receiver.borrow().generation),
            };
        }
        let Some(receiver) = self.receiver.as_ref() else {
            return ActivitySnapshot::default();
        };
        if receiver.has_changed().is_err() {
            return ActivitySnapshot {
                active: true,
                generation: receiver.borrow().generation,
            };
        }
        *receiver.borrow()
    }

    pub fn publish_if_current(&self, started: ActivitySnapshot, publish: impl FnOnce()) -> bool {
        let _publish_guard = self
            .publish_gate
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        if !should_publish_collection(started, self.snapshot()) {
            return false;
        }
        publish();
        true
    }

    pub async fn wait_until_ready_or_timeout(&self, duration: std::time::Duration) {
        let deadline = tokio::time::Instant::now() + duration;
        while !self._watcher_ready.load(Ordering::Acquire) {
            if self.is_shutdown() {
                return;
            }
            let closed = match self.receiver.as_ref() {
                Some(receiver) => receiver.has_changed().is_err(),
                None => true,
            };
            if closed || tokio::time::Instant::now() >= deadline {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    pub async fn wait_until_active(&mut self) {
        loop {
            if self.is_shutdown() {
                return;
            }
            if self.snapshot().active {
                return;
            }
            let Some(receiver) = self.receiver.as_mut() else {
                return;
            };
            if receiver.changed().await.is_err() {
                self.receiver = None;
                return;
            }
        }
    }

    pub async fn wait_for_change_or_timeout(
        &mut self,
        generation: u64,
        duration: std::time::Duration,
    ) {
        let deadline = tokio::time::Instant::now() + duration;
        loop {
            if self.is_shutdown() {
                return;
            }
            if self.snapshot().generation != generation {
                return;
            }
            let Some(receiver) = self.receiver.as_mut() else {
                tokio::time::sleep_until(deadline).await;
                return;
            };
            match tokio::time::timeout_at(deadline, receiver.changed()).await {
                Ok(Ok(())) => {}
                Ok(Err(_)) => {
                    self.receiver = None;
                    tokio::time::sleep_until(deadline).await;
                    return;
                }
                Err(_) => return,
            }
        }
    }
}

pub fn should_publish_collection(started: ActivitySnapshot, finished: ActivitySnapshot) -> bool {
    started.active && finished.active && started.generation == finished.generation
}

#[cfg(windows)]
fn run_native_watcher(
    sender: watch::Sender<ActivitySnapshot>,
    watcher_ready: Arc<AtomicBool>,
    publish_gate: Arc<Mutex<()>>,
    stop_receiver: mpsc::Receiver<()>,
) -> anyhow::Result<()> {
    use windows::{
        core::{w, Error, PCWSTR},
        Win32::{
            Foundation::{HANDLE, HINSTANCE},
            System::{
                LibraryLoader::GetModuleHandleW,
                Power::{RegisterPowerSettingNotification, UnregisterPowerSettingNotification},
                RemoteDesktop::{
                    WTSRegisterSessionNotification, WTSUnRegisterSessionNotification,
                    NOTIFY_FOR_THIS_SESSION,
                },
                SystemServices::GUID_SESSION_DISPLAY_STATUS,
            },
            UI::WindowsAndMessaging::{
                CreateWindowExW, DestroyWindow, RegisterClassW, SetWindowLongPtrW,
                UnregisterClassW, DEVICE_NOTIFY_WINDOW_HANDLE, GWLP_USERDATA, MSG, WNDCLASSW,
                WS_OVERLAPPED,
            },
        },
    };

    let context = Arc::new(WatcherContext {
        inputs: Mutex::new(ActivityInputs::default()),
        sender,
        display_state_seen: AtomicBool::new(false),
        publish_gate,
    });
    let instance = HINSTANCE(unsafe { GetModuleHandleW(PCWSTR::null())? }.0);
    let class_name = w!("AgentJuiceSystemActivity");
    let window_class = WNDCLASSW {
        lpfnWndProc: Some(system_activity_window_proc),
        hInstance: instance,
        lpszClassName: class_name,
        ..Default::default()
    };
    let atom = unsafe { RegisterClassW(&window_class) };
    if atom == 0 {
        return Err(Error::from_win32().into());
    }

    let window = match unsafe {
        CreateWindowExW(
            Default::default(),
            class_name,
            PCWSTR::null(),
            WS_OVERLAPPED,
            0,
            0,
            1,
            1,
            None,
            None,
            Some(instance),
            None,
        )
    } {
        Ok(window) => window,
        Err(err) => {
            let _ = unsafe { UnregisterClassW(class_name, Some(instance)) };
            return Err(err.into());
        }
    };
    unsafe {
        SetWindowLongPtrW(
            window,
            GWLP_USERDATA,
            Arc::as_ptr(&context) as *const () as isize,
        );
    }

    let power_notification = match unsafe {
        RegisterPowerSettingNotification(
            HANDLE(window.0),
            &GUID_SESSION_DISPLAY_STATUS,
            DEVICE_NOTIFY_WINDOW_HANDLE,
        )
    } {
        Ok(handle) => Some(handle),
        Err(err) => {
            eprintln!(
                "[system-activity] display-off detection unavailable; session-lock gating remains active: {err}"
            );
            None
        }
    };
    let mut session_registered = false;
    let mut session_error = None;
    for attempt in 0..4 {
        match unsafe { WTSRegisterSessionNotification(window, NOTIFY_FOR_THIS_SESSION) } {
            Ok(()) => {
                session_registered = true;
                break;
            }
            Err(err) => {
                session_error = Some(err);
                if attempt < 3 {
                    std::thread::sleep(std::time::Duration::from_millis(100u64 << attempt));
                }
            }
        }
    }
    if !session_registered {
        let error = session_error
            .map(|err| err.to_string())
            .unwrap_or_else(|| "unknown registration error".into());
        eprintln!(
            "[system-activity] session lock detection unavailable after bounded retries; display-state gating remains active: {error}"
        );
    }
    let current_lock_state = if session_registered {
        current_session_locked()
    } else {
        None
    };
    context.signal(initial_session_signal(
        session_registered,
        current_lock_state,
    ));
    if !session_registered && power_notification.is_none() {
        unsafe {
            SetWindowLongPtrW(window, GWLP_USERDATA, 0);
        }
        let _ = unsafe { DestroyWindow(window) };
        let _ = unsafe { UnregisterClassW(class_name, Some(instance)) };
        return Err(anyhow::anyhow!(
            "session and display notification registration both failed"
        ));
    }

    let mut initial_message = MSG::default();
    if power_notification.is_some() {
        let initial_display_deadline =
            std::time::Instant::now() + std::time::Duration::from_millis(500);
        while !context.display_state_seen.load(Ordering::Acquire)
            && std::time::Instant::now() < initial_display_deadline
        {
            pump_native_messages(&mut initial_message);
            if !context.display_state_seen.load(Ordering::Acquire) {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
    }
    pump_native_messages(&mut initial_message);
    watcher_ready.store(true, Ordering::Release);

    let mut message = MSG::default();
    loop {
        pump_native_messages(&mut message);
        match stop_receiver.recv_timeout(std::time::Duration::from_millis(50)) {
            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }

    if let Some(power_notification) = power_notification {
        let _ = unsafe { UnregisterPowerSettingNotification(power_notification) };
    }
    if session_registered {
        let _ = unsafe { WTSUnRegisterSessionNotification(window) };
    }
    unsafe {
        SetWindowLongPtrW(window, GWLP_USERDATA, 0);
    }
    let _ = unsafe { DestroyWindow(window) };
    let _ = unsafe { UnregisterClassW(class_name, Some(instance)) };
    Ok(())
}

#[cfg(windows)]
fn pump_native_messages(message: &mut windows::Win32::UI::WindowsAndMessaging::MSG) {
    use windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, PeekMessageW, TranslateMessage, PM_REMOVE,
    };

    while unsafe { PeekMessageW(message, None, 0, 0, PM_REMOVE) }.as_bool() {
        unsafe {
            let _ = TranslateMessage(message);
            DispatchMessageW(message);
        }
    }
}

#[cfg(windows)]
fn current_session_locked() -> Option<bool> {
    use windows::{
        core::PWSTR,
        Win32::System::RemoteDesktop::{
            WTSFreeMemory, WTSQuerySessionInformationW, WTSSessionInfoEx, WTSINFOEXW,
            WTS_CURRENT_SESSION, WTS_SESSIONSTATE_LOCK,
        },
    };

    let mut buffer = PWSTR::null();
    let mut bytes_returned = 0u32;
    if unsafe {
        WTSQuerySessionInformationW(
            None,
            WTS_CURRENT_SESSION,
            WTSSessionInfoEx,
            &mut buffer,
            &mut bytes_returned,
        )
    }
    .is_err()
    {
        return None;
    }

    let result = if buffer.0.is_null() || bytes_returned < std::mem::size_of::<WTSINFOEXW>() as u32
    {
        None
    } else {
        let info = unsafe { &*(buffer.0 as *const WTSINFOEXW) };
        if info.Level != 1 {
            None
        } else {
            let level = unsafe { info.Data.WTSInfoExLevel1 };
            Some(level.SessionFlags == WTS_SESSIONSTATE_LOCK as i32)
        }
    };
    if !buffer.0.is_null() {
        unsafe {
            WTSFreeMemory(buffer.0.cast());
        }
    }
    result
}

#[cfg(not(windows))]
fn run_native_watcher(
    _sender: watch::Sender<ActivitySnapshot>,
    watcher_ready: Arc<AtomicBool>,
    _publish_gate: Arc<Mutex<()>>,
    _stop_receiver: mpsc::Receiver<()>,
) -> anyhow::Result<()> {
    watcher_ready.store(true, Ordering::Release);
    Ok(())
}

#[cfg(windows)]
unsafe extern "system" fn system_activity_window_proc(
    window: windows::Win32::Foundation::HWND,
    message: u32,
    wparam: windows::Win32::Foundation::WPARAM,
    lparam: windows::Win32::Foundation::LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    use windows::Win32::{
        System::{Power::POWERBROADCAST_SETTING, SystemServices::GUID_SESSION_DISPLAY_STATUS},
        UI::WindowsAndMessaging::{
            DefWindowProcW, GetWindowLongPtrW, GWLP_USERDATA, PBT_POWERSETTINGCHANGE,
            WM_POWERBROADCAST, WM_WTSSESSION_CHANGE, WTS_SESSION_LOCK, WTS_SESSION_UNLOCK,
        },
    };

    let context = unsafe { GetWindowLongPtrW(window, GWLP_USERDATA) as *const WatcherContext };
    if !context.is_null() {
        let context = unsafe { &*context };
        match message {
            WM_WTSSESSION_CHANGE if wparam.0 as u32 == WTS_SESSION_LOCK => {
                context.signal(ActivitySignal::SessionLocked);
            }
            WM_WTSSESSION_CHANGE if wparam.0 as u32 == WTS_SESSION_UNLOCK => {
                context.signal(ActivitySignal::SessionUnlocked);
            }
            WM_POWERBROADCAST if wparam.0 as u32 == PBT_POWERSETTINGCHANGE && lparam.0 != 0 => {
                let setting = unsafe { &*(lparam.0 as *const POWERBROADCAST_SETTING) };
                if setting.PowerSetting == GUID_SESSION_DISPLAY_STATUS
                    && setting.DataLength >= std::mem::size_of::<u32>() as u32
                {
                    let value = unsafe { (setting.Data.as_ptr() as *const u32).read_unaligned() };
                    if let Some(signal) = display_signal(value) {
                        context.display_state_seen.store(true, Ordering::Release);
                        context.signal(signal);
                    }
                }
            }
            _ => {}
        }
    }

    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activity_state_requires_both_session_and_display_to_be_available() {
        let mut state = ActivityInputs::default();
        assert!(state.active());

        assert!(state.apply(ActivitySignal::SessionLocked));
        assert!(!state.active());
        assert!(!state.apply(ActivitySignal::SessionLocked));

        assert!(state.apply(ActivitySignal::DisplayOff));
        assert!(!state.active());
        assert!(state.apply(ActivitySignal::SessionUnlocked));
        assert!(!state.active());

        assert!(state.apply(ActivitySignal::DisplayOn));
        assert!(state.active());
    }

    #[test]
    fn dimmed_display_remains_collectable_without_duplicate_transitions() {
        let mut state = ActivityInputs {
            session_locked: false,
            display_off: true,
        };
        assert!(state.apply(ActivitySignal::DisplayDimmed));
        assert!(state.active());
        assert!(!state.apply(ActivitySignal::DisplayDimmed));
    }

    #[test]
    fn display_status_values_map_only_known_windows_states() {
        assert_eq!(display_signal(0), Some(ActivitySignal::DisplayOff));
        assert_eq!(display_signal(1), Some(ActivitySignal::DisplayOn));
        assert_eq!(display_signal(2), Some(ActivitySignal::DisplayDimmed));
        assert_eq!(display_signal(3), None);
    }

    #[test]
    fn session_state_initialization_fails_open_when_registration_or_query_fails() {
        assert_eq!(
            initial_session_signal(true, Some(true)),
            ActivitySignal::SessionLocked
        );
        assert_eq!(
            initial_session_signal(true, Some(false)),
            ActivitySignal::SessionUnlocked
        );
        assert_eq!(
            initial_session_signal(true, None),
            ActivitySignal::SessionUnlocked
        );
        assert_eq!(
            initial_session_signal(false, Some(true)),
            ActivitySignal::SessionUnlocked
        );
    }

    #[test]
    fn collection_result_is_rejected_after_any_activity_transition() {
        let started = ActivitySnapshot {
            active: true,
            generation: 4,
        };
        assert!(should_publish_collection(started, started));
        assert!(!should_publish_collection(
            started,
            ActivitySnapshot {
                active: false,
                generation: 5,
            }
        ));
        assert!(!should_publish_collection(
            started,
            ActivitySnapshot {
                active: true,
                generation: 6,
            }
        ));
    }

    #[test]
    fn publish_gate_serializes_status_emission_before_a_state_transition() {
        let (sender, receiver) = watch::channel(ActivitySnapshot::default());
        let publish_gate = Arc::new(Mutex::new(()));
        let context = Arc::new(WatcherContext {
            inputs: Mutex::new(ActivityInputs::default()),
            sender,
            display_state_seen: AtomicBool::new(false),
            publish_gate: publish_gate.clone(),
        });
        let monitor = SystemActivityMonitor {
            receiver: Some(receiver),
            _watcher_ready: Arc::new(AtomicBool::new(true)),
            _watcher_stopped: Arc::new(AtomicBool::new(false)),
            shutdown_requested: Arc::new(AtomicBool::new(false)),
            publish_gate,
        };
        let started = monitor.snapshot();
        let (publish_entered_tx, publish_entered_rx) = mpsc::channel();
        let (release_publish_tx, release_publish_rx) = mpsc::channel();
        let publisher = std::thread::spawn(move || {
            monitor.publish_if_current(started, || {
                publish_entered_tx.send(()).unwrap();
                release_publish_rx.recv().unwrap();
            })
        });
        publish_entered_rx.recv().unwrap();

        let transition_finished = Arc::new(AtomicBool::new(false));
        let transition_flag = transition_finished.clone();
        let (transition_started_tx, transition_started_rx) = mpsc::channel();
        let transitioner = std::thread::spawn(move || {
            transition_started_tx.send(()).unwrap();
            context.signal(ActivitySignal::SessionLocked);
            transition_flag.store(true, Ordering::Release);
        });
        transition_started_rx.recv().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(25));
        assert!(!transition_finished.load(Ordering::Acquire));

        release_publish_tx.send(()).unwrap();
        assert!(publisher.join().unwrap());
        transitioner.join().unwrap();
        assert!(transition_finished.load(Ordering::Acquire));
    }

    #[cfg(windows)]
    #[test]
    fn native_watcher_registers_and_stops_cleanly() {
        use windows::{
            core::{w, GUID},
            Win32::{
                Foundation::{LPARAM, WPARAM},
                System::{
                    Power::POWERBROADCAST_SETTING, SystemServices::GUID_SESSION_DISPLAY_STATUS,
                },
                UI::WindowsAndMessaging::{
                    FindWindowExW, GetWindowThreadProcessId, PostMessageW, SendMessageTimeoutW,
                    PBT_POWERSETTINGCHANGE, SMTO_ABORTIFHUNG, SMTO_BLOCK, WM_POWERBROADCAST,
                    WM_WTSSESSION_CHANGE, WTS_SESSION_LOCK, WTS_SESSION_UNLOCK,
                },
            },
        };

        #[repr(C)]
        struct TestPowerSetting {
            power_setting: GUID,
            data_length: u32,
            value: u32,
        }

        fn wait_for_activity(monitor: &SystemActivityMonitor, active: bool) {
            for _ in 0..40 {
                if monitor.snapshot().active == active {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            panic!("native watcher did not reach active={active}");
        }

        fn send_power_setting(
            window: windows::Win32::Foundation::HWND,
            setting: &TestPowerSetting,
        ) {
            let mut result = 0usize;
            let sent = unsafe {
                SendMessageTimeoutW(
                    window,
                    WM_POWERBROADCAST,
                    WPARAM(PBT_POWERSETTINGCHANGE as usize),
                    LPARAM((setting as *const TestPowerSetting) as isize),
                    SMTO_ABORTIFHUNG | SMTO_BLOCK,
                    250,
                    Some(&mut result),
                )
            };
            assert_ne!(sent.0, 0, "power setting message timed out or failed");
        }

        fn find_current_process_watcher_window() -> windows::Win32::Foundation::HWND {
            let mut after = None;
            loop {
                let candidate =
                    unsafe { FindWindowExW(None, after, w!("AgentJuiceSystemActivity"), None) }
                        .expect("watcher HWND owned by current test process");
                let mut process_id = 0;
                unsafe {
                    GetWindowThreadProcessId(candidate, Some(&mut process_id));
                }
                if process_id == std::process::id() {
                    return candidate;
                }
                after = Some(candidate);
            }
        }

        let (monitor, shutdown) = SystemActivityMonitor::start();
        for _ in 0..40 {
            if monitor._watcher_ready.load(Ordering::Acquire) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(
            monitor._watcher_ready.load(Ordering::Acquire),
            "native watcher did not finish registration"
        );
        let receiver = monitor.receiver.as_ref().expect("watcher thread spawned");
        assert!(
            receiver.has_changed().is_ok(),
            "native watcher exited during registration"
        );

        let window = find_current_process_watcher_window();
        let display_on = Box::new(TestPowerSetting {
            power_setting: GUID_SESSION_DISPLAY_STATUS,
            data_length: std::mem::size_of::<u32>() as u32,
            value: 1,
        });
        send_power_setting(window, &display_on);
        unsafe {
            PostMessageW(
                Some(window),
                WM_WTSSESSION_CHANGE,
                WPARAM(WTS_SESSION_UNLOCK as usize),
                LPARAM(0),
            )
            .unwrap();
        }
        wait_for_activity(&monitor, true);

        unsafe {
            PostMessageW(
                Some(window),
                WM_WTSSESSION_CHANGE,
                WPARAM(WTS_SESSION_LOCK as usize),
                LPARAM(0),
            )
            .unwrap();
        }
        wait_for_activity(&monitor, false);
        unsafe {
            PostMessageW(
                Some(window),
                WM_WTSSESSION_CHANGE,
                WPARAM(WTS_SESSION_UNLOCK as usize),
                LPARAM(0),
            )
            .unwrap();
        }
        wait_for_activity(&monitor, true);

        let display_off = Box::new(TestPowerSetting {
            power_setting: GUID_SESSION_DISPLAY_STATUS,
            data_length: std::mem::size_of::<u32>() as u32,
            value: 0,
        });
        send_power_setting(window, &display_off);
        wait_for_activity(&monitor, false);

        send_power_setting(window, &display_on);
        wait_for_activity(&monitor, true);

        assert_eq!(
            std::mem::offset_of!(TestPowerSetting, value),
            std::mem::offset_of!(POWERBROADCAST_SETTING, Data)
        );
        shutdown.stop();
        for _ in 0..40 {
            if monitor._watcher_stopped.load(Ordering::Acquire) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        assert!(
            monitor._watcher_stopped.load(Ordering::Acquire),
            "native watcher did not stop and release registrations"
        );
    }
}
