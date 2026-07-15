#![cfg(windows)]

use std::{
    sync::mpsc::{self, Receiver, Sender},
    thread::JoinHandle,
    time::Duration,
};
use windows::{
    core::PCWSTR,
    Win32::{
        Foundation::{
            CloseHandle, GetLastError, HANDLE, WAIT_ABANDONED_0, WAIT_OBJECT_0, WAIT_TIMEOUT,
        },
        System::Threading::{
            CreateEventW, CreateMutexW, ReleaseMutex, SetEvent, WaitForSingleObject,
        },
    },
};

const INSTANCE_MUTEX_NAME: &str = "Local\\com.pointi.agentjuice.instance-owner.v2";
const INSTANCE_EVENT_NAME: &str = "Local\\com.pointi.agentjuice.instance.v2";
const SECONDARY_HANDOFF_TIMEOUT: Duration = Duration::from_millis(750);
const MAX_BOUNDED_WAIT_MS: u128 = (u32::MAX - 1) as u128;

pub enum AcquireResult {
    Primary(InstanceEvent),
    Secondary,
}

pub struct InstanceEvent {
    _lease: PrimaryLease,
    event: OwnedHandle,
}

unsafe impl Send for InstanceEvent {}

impl InstanceEvent {
    pub fn wait(&self, timeout: Duration) -> anyhow::Result<bool> {
        match unsafe { WaitForSingleObject(self.event.0, bounded_timeout_ms(timeout)) } {
            WAIT_OBJECT_0 => Ok(true),
            WAIT_TIMEOUT => Ok(false),
            other => {
                let error = unsafe { GetLastError() };
                anyhow::bail!(
                    "single-instance event wait failed: result {}, win32 error {}",
                    other.0,
                    error.0
                )
            }
        }
    }
}

struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            let _ = unsafe { CloseHandle(self.0) };
        }
    }
}

#[derive(Clone, Copy)]
struct BorrowedEventHandle(HANDLE);

unsafe impl Send for BorrowedEventHandle {}

impl BorrowedEventHandle {
    fn signal(self) -> anyhow::Result<()> {
        unsafe { SetEvent(self.0)? };
        Ok(())
    }
}

struct NamedMutex {
    handle: HANDLE,
    owned: bool,
}

impl NamedMutex {
    fn create(name: &str) -> anyhow::Result<Self> {
        let wide = wide_name(name);
        let handle = unsafe { CreateMutexW(None, false, PCWSTR(wide.as_ptr()))? };
        Ok(Self {
            handle,
            owned: false,
        })
    }

    fn wait(&mut self, timeout: Duration) -> anyhow::Result<MutexWait> {
        match unsafe { WaitForSingleObject(self.handle, bounded_timeout_ms(timeout)) } {
            WAIT_OBJECT_0 | WAIT_ABANDONED_0 => {
                self.owned = true;
                Ok(MutexWait::Acquired)
            }
            WAIT_TIMEOUT => Ok(MutexWait::TimedOut),
            other => {
                let error = unsafe { GetLastError() };
                anyhow::bail!(
                    "single-instance mutex wait failed: result {}, win32 error {}",
                    other.0,
                    error.0
                )
            }
        }
    }
}

impl Drop for NamedMutex {
    fn drop(&mut self) {
        if self.owned {
            let _ = unsafe { ReleaseMutex(self.handle) };
        }
        if !self.handle.is_invalid() {
            let _ = unsafe { CloseHandle(self.handle) };
        }
    }
}

#[derive(Clone, Copy)]
enum MutexWait {
    Acquired,
    TimedOut,
}

#[derive(Clone, Copy)]
enum OwnerOutcome {
    Primary,
    Secondary,
}

enum LeaseAcquisition {
    Primary(PrimaryLease),
    Secondary,
}

struct PrimaryLease {
    stop_sender: Option<Sender<()>>,
    owner_thread: Option<JoinHandle<()>>,
}

impl PrimaryLease {
    fn acquire(
        mutex_name: &str,
        event: BorrowedEventHandle,
        handoff_timeout: Duration,
    ) -> anyhow::Result<LeaseAcquisition> {
        let mutex_name = mutex_name.to_owned();
        let (outcome_sender, outcome_receiver) = mpsc::sync_channel(1);
        let (stop_sender, stop_receiver) = mpsc::channel();
        let owner_thread = std::thread::Builder::new()
            .name("juice-instance-owner".into())
            .spawn(move || {
                run_owner_thread(
                    &mutex_name,
                    event,
                    handoff_timeout,
                    outcome_sender,
                    stop_receiver,
                )
            })?;

        match outcome_receiver.recv() {
            Ok(Ok(OwnerOutcome::Primary)) => Ok(LeaseAcquisition::Primary(Self {
                stop_sender: Some(stop_sender),
                owner_thread: Some(owner_thread),
            })),
            Ok(Ok(OwnerOutcome::Secondary)) => {
                drop(stop_sender);
                join_owner_thread(owner_thread)?;
                Ok(LeaseAcquisition::Secondary)
            }
            Ok(Err(error)) => {
                drop(stop_sender);
                join_owner_thread(owner_thread)?;
                Err(error)
            }
            Err(error) => {
                drop(stop_sender);
                let panicked = owner_thread.join().is_err();
                if panicked {
                    anyhow::bail!("single-instance owner thread panicked before reporting state")
                }
                anyhow::bail!(
                    "single-instance owner thread exited without reporting state: {error}"
                )
            }
        }
    }
}

impl Drop for PrimaryLease {
    fn drop(&mut self) {
        drop(self.stop_sender.take());
        if let Some(owner_thread) = self.owner_thread.take() {
            let _ = owner_thread.join();
        }
    }
}

fn run_owner_thread(
    mutex_name: &str,
    event: BorrowedEventHandle,
    handoff_timeout: Duration,
    outcome_sender: mpsc::SyncSender<anyhow::Result<OwnerOutcome>>,
    stop_receiver: Receiver<()>,
) {
    let mut mutex = match NamedMutex::create(mutex_name) {
        Ok(mutex) => mutex,
        Err(error) => {
            let _ = outcome_sender.send(Err(error));
            return;
        }
    };

    let outcome = match mutex.wait(Duration::ZERO) {
        Ok(MutexWait::Acquired) => Ok(OwnerOutcome::Primary),
        Ok(MutexWait::TimedOut) => event.signal().and_then(|()| {
            mutex.wait(handoff_timeout).map(|wait| match wait {
                MutexWait::Acquired => OwnerOutcome::Primary,
                MutexWait::TimedOut => OwnerOutcome::Secondary,
            })
        }),
        Err(error) => Err(error),
    };

    match outcome {
        Ok(OwnerOutcome::Primary) => {
            if outcome_sender.send(Ok(OwnerOutcome::Primary)).is_ok() {
                let _ = stop_receiver.recv();
            }
        }
        Ok(OwnerOutcome::Secondary) => {
            let _ = outcome_sender.send(Ok(OwnerOutcome::Secondary));
        }
        Err(error) => {
            let _ = outcome_sender.send(Err(error));
        }
    }
}

fn join_owner_thread(owner_thread: JoinHandle<()>) -> anyhow::Result<()> {
    owner_thread
        .join()
        .map_err(|_| anyhow::anyhow!("single-instance owner thread panicked"))
}

fn create_activation_event(name: &str) -> anyhow::Result<OwnedHandle> {
    let wide = wide_name(name);
    let handle = unsafe { CreateEventW(None, false, false, PCWSTR(wide.as_ptr()))? };
    Ok(OwnedHandle(handle))
}

fn acquire_named(
    mutex_name: &str,
    event_name: &str,
    handoff_timeout: Duration,
) -> anyhow::Result<AcquireResult> {
    let event = create_activation_event(event_name)?;
    match PrimaryLease::acquire(mutex_name, BorrowedEventHandle(event.0), handoff_timeout)? {
        LeaseAcquisition::Primary(lease) => Ok(AcquireResult::Primary(InstanceEvent {
            _lease: lease,
            event,
        })),
        LeaseAcquisition::Secondary => Ok(AcquireResult::Secondary),
    }
}

fn bounded_timeout_ms(timeout: Duration) -> u32 {
    timeout.as_millis().min(MAX_BOUNDED_WAIT_MS) as u32
}

fn wide_name(name: &str) -> Vec<u16> {
    name.encode_utf16().chain(Some(0)).collect()
}

pub fn acquire() -> anyhow::Result<AcquireResult> {
    acquire_named(
        INSTANCE_MUTEX_NAME,
        INSTANCE_EVENT_NAME,
        SECONDARY_HANDOFF_TIMEOUT,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        ffi::c_void,
        sync::{
            atomic::{AtomicU64, Ordering},
            Arc, Barrier,
        },
        time::Instant,
    };
    use windows::Win32::Foundation::ERROR_ALREADY_EXISTS;

    static NEXT_TEST_NAME: AtomicU64 = AtomicU64::new(1);

    fn test_names(label: &str) -> (String, String) {
        let base = format!(
            "Local\\agent-juice-single-instance-{label}-{}-{}",
            std::process::id(),
            NEXT_TEST_NAME.fetch_add(1, Ordering::Relaxed)
        );
        (format!("{base}-owner"), format!("{base}-activation"))
    }

    fn expect_primary(result: AcquireResult) -> InstanceEvent {
        match result {
            AcquireResult::Primary(event) => event,
            AcquireResult::Secondary => panic!("expected primary instance"),
        }
    }

    fn named_event_exists(name: &str) -> bool {
        let wide = wide_name(name);
        let handle = unsafe { CreateEventW(None, false, false, PCWSTR(wide.as_ptr())) }.unwrap();
        let exists = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
        let _ = unsafe { CloseHandle(handle) };
        exists
    }

    fn named_mutex_exists(name: &str) -> bool {
        let wide = wide_name(name);
        let handle = unsafe { CreateMutexW(None, false, PCWSTR(wide.as_ptr())) }.unwrap();
        let exists = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
        let _ = unsafe { CloseHandle(handle) };
        exists
    }

    #[test]
    fn simultaneous_startup_has_exactly_one_primary() {
        let (mutex_name, event_name) = test_names("simultaneous");
        let barrier = Arc::new(Barrier::new(8));
        let mut starters = Vec::new();

        for _ in 0..8 {
            let mutex_name = mutex_name.clone();
            let event_name = event_name.clone();
            let barrier = barrier.clone();
            starters.push(std::thread::spawn(move || {
                barrier.wait();
                acquire_named(&mutex_name, &event_name, Duration::from_millis(80))
            }));
        }

        let mut primary = None;
        for starter in starters {
            match starter.join().unwrap().unwrap() {
                AcquireResult::Primary(event) => {
                    assert!(primary.is_none(), "multiple primary instances started");
                    primary = Some(event);
                }
                AcquireResult::Secondary => {}
            }
        }
        assert!(primary.is_some(), "no primary instance started");
    }

    #[test]
    fn hung_primary_is_signaled_and_secondary_wait_is_bounded() {
        let (mutex_name, event_name) = test_names("hung");
        let primary = expect_primary(
            acquire_named(&mutex_name, &event_name, Duration::from_millis(40)).unwrap(),
        );

        let started = Instant::now();
        assert!(matches!(
            acquire_named(&mutex_name, &event_name, Duration::from_millis(60)).unwrap(),
            AcquireResult::Secondary
        ));
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "secondary startup exceeded its bounded wait"
        );
        assert!(primary.wait(Duration::from_secs(1)).unwrap());
        assert!(!primary.wait(Duration::ZERO).unwrap());
    }

    #[test]
    fn quitting_primary_hands_mutex_ownership_to_waiting_secondary() {
        let (mutex_name, event_name) = test_names("handoff");
        let primary = expect_primary(
            acquire_named(&mutex_name, &event_name, Duration::from_millis(40)).unwrap(),
        );
        let successor_mutex = mutex_name.clone();
        let successor_event = event_name.clone();
        let successor = std::thread::spawn(move || {
            acquire_named(&successor_mutex, &successor_event, Duration::from_secs(2))
        });

        assert!(primary.wait(Duration::from_secs(1)).unwrap());
        drop(primary);

        let successor = expect_primary(successor.join().unwrap().unwrap());
        assert!(matches!(
            acquire_named(&mutex_name, &event_name, Duration::from_millis(40)).unwrap(),
            AcquireResult::Secondary
        ));
        assert!(successor.wait(Duration::from_secs(1)).unwrap());
    }

    #[test]
    fn abandoned_mutex_is_acquired_as_primary() {
        let (mutex_name, event_name) = test_names("abandoned");
        let owner_name = mutex_name.clone();
        let (handle_sender, handle_receiver) = mpsc::sync_channel(1);
        let owner = std::thread::spawn(move || {
            let wide = wide_name(&owner_name);
            let handle = unsafe { CreateMutexW(None, true, PCWSTR(wide.as_ptr())) }.unwrap();
            handle_sender.send(handle.0 as isize).unwrap();
        });
        let abandoned_handle = handle_receiver.recv().unwrap();
        owner.join().unwrap();
        let abandoned_handle = OwnedHandle(HANDLE(abandoned_handle as *mut c_void));

        let primary = expect_primary(
            acquire_named(&mutex_name, &event_name, Duration::from_millis(40)).unwrap(),
        );
        drop(primary);
        drop(abandoned_handle);

        assert!(!named_mutex_exists(&mutex_name));
        assert!(!named_event_exists(&event_name));
    }

    #[test]
    fn all_named_handles_close_after_primary_and_secondary_paths() {
        let (mutex_name, event_name) = test_names("cleanup");
        let primary = expect_primary(
            acquire_named(&mutex_name, &event_name, Duration::from_millis(40)).unwrap(),
        );
        assert!(named_mutex_exists(&mutex_name));
        assert!(named_event_exists(&event_name));

        assert!(matches!(
            acquire_named(&mutex_name, &event_name, Duration::from_millis(40)).unwrap(),
            AcquireResult::Secondary
        ));
        drop(primary);

        assert!(!named_mutex_exists(&mutex_name));
        assert!(!named_event_exists(&event_name));
    }

    #[test]
    fn activation_handle_closes_when_mutex_creation_fails() {
        let (collision_name, event_name) = test_names("error-cleanup");
        let collision = create_activation_event(&collision_name).unwrap();

        assert!(acquire_named(&collision_name, &event_name, Duration::from_millis(40)).is_err());
        assert!(!named_event_exists(&event_name));

        drop(collision);
        assert!(!named_event_exists(&collision_name));
    }
}
