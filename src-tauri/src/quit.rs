use std::sync::Mutex;
use tauri::{Emitter, Manager};

// This module has no platform or Tauri dependencies and is also tested with rustc.
pub(crate) mod core {
    pub const INITIAL_WAIT_SECS: u64 = 2;
    pub const RECOVERY_WAIT_SECS: u64 = 10;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Timer {
        pub request_id: u64,
        pub generation: u64,
        pub delay_secs: u64,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct DialogToken {
        pub request_id: u64,
        pub ticket: u64,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum RecoveryReason {
        Unresponsive,
        SaveFailed,
        RepeatedRequest,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Recovery {
        None,
        Show(DialogToken),
        Focus(isize),
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Choice {
        Wait,
        Force,
        Cancel,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Retired {
        pub request_id: u64,
        pub dialog_handle: Option<isize>,
        pub dialog_token: Option<DialogToken>,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Resolution {
        None,
        Wait(Timer),
        Retry(Timer, u64),
        Exit(Retired),
        Cancel(Retired),
    }

    #[derive(Debug)]
    struct Dialog {
        token: DialogToken,
        handle: Option<isize>,
    }

    #[derive(Debug)]
    struct Attempt {
        timer: Timer,
        retry_id: Option<u64>,
        dialog: Option<Dialog>,
        retryable: bool,
    }

    #[derive(Debug, Default)]
    pub struct Machine {
        next_request: u64,
        next_dialog: u64,
        pending: Option<Attempt>,
        exiting: bool,
    }

    impl Machine {
        pub fn pending_request(&self) -> Option<(u64, Option<u64>)> {
            let attempt = self.pending.as_ref()?;
            Some((attempt.timer.request_id, attempt.retry_id))
        }

        pub fn retired_through(&self) -> Option<u64> {
            let retired = self.next_request - u64::from(self.pending.is_some());
            (retired > 0).then_some(retired)
        }

        pub fn request(&mut self) -> Option<(Timer, bool)> {
            if self.exiting {
                return None;
            }
            if let Some(attempt) = &self.pending {
                return Some((attempt.timer, false));
            }
            self.next_request = self.next_request.checked_add(1)?;
            let timer = Timer {
                request_id: self.next_request,
                generation: 0,
                delay_secs: INITIAL_WAIT_SECS,
            };
            self.pending = Some(Attempt {
                timer,
                retry_id: None,
                dialog: None,
                retryable: false,
            });
            Some((timer, true))
        }

        pub fn recover(&mut self, request_id: u64, timer: Option<Timer>) -> Recovery {
            let Some(attempt) = self.pending.as_mut() else {
                return Recovery::None;
            };
            if attempt.timer.request_id != request_id
                || timer.is_some_and(|timer| timer != attempt.timer)
            {
                return Recovery::None;
            }
            if let Some(dialog) = &attempt.dialog {
                return dialog.handle.map_or(Recovery::None, Recovery::Focus);
            }
            let Some(ticket) = self.next_dialog.checked_add(1) else {
                return Recovery::None;
            };
            self.next_dialog = ticket;
            let token = DialogToken { request_id, ticket };
            attempt.dialog = Some(Dialog {
                token,
                handle: None,
            });
            Recovery::Show(token)
        }

        pub fn fail_and_recover(
            &mut self,
            request_id: u64,
            retry_id: Option<u64>,
            retryable: bool,
        ) -> Recovery {
            let Some(attempt) = self.pending.as_mut() else {
                return Recovery::None;
            };
            if attempt.timer.request_id != request_id || attempt.retry_id != retry_id {
                return Recovery::None;
            }
            attempt.retryable |= retryable;
            self.recover(request_id, None)
        }

        pub fn can_retry(&self, token: DialogToken) -> Option<bool> {
            self.pending
                .as_ref()
                .filter(|_| self.dialog_is_current(token))
                .map(|attempt| attempt.retryable)
        }

        pub fn dialog_is_current(&self, token: DialogToken) -> bool {
            self.pending
                .as_ref()
                .and_then(|attempt| attempt.dialog.as_ref())
                .is_some_and(|dialog| dialog.token == token)
        }

        pub fn dialog_created(&mut self, token: DialogToken, handle: isize) -> bool {
            let Some(dialog) = self
                .pending
                .as_mut()
                .and_then(|attempt| attempt.dialog.as_mut())
            else {
                return false;
            };
            if dialog.token != token {
                return false;
            }
            dialog.handle = Some(handle);
            true
        }

        pub fn dialog_closed(&mut self, token: DialogToken) {
            if let Some(dialog) = self
                .pending
                .as_mut()
                .and_then(|attempt| attempt.dialog.as_mut())
            {
                if dialog.token == token {
                    dialog.handle = None;
                }
            }
        }

        fn retire(&mut self, request_id: u64, exiting: bool) -> Option<Retired> {
            if self.pending.as_ref()?.timer.request_id != request_id {
                return None;
            }
            let attempt = self.pending.take()?;
            self.exiting = exiting;
            Some(Retired {
                request_id,
                dialog_token: attempt.dialog.as_ref().map(|dialog| dialog.token),
                dialog_handle: attempt.dialog.and_then(|dialog| dialog.handle),
            })
        }

        pub fn complete(&mut self, request_id: u64) -> Option<Retired> {
            self.retire(request_id, true)
        }

        pub fn cancel(&mut self, request_id: u64) -> Option<Retired> {
            self.retire(request_id, false)
        }

        pub fn cancel_current(&mut self) -> Option<Retired> {
            let request_id = self.pending.as_ref()?.timer.request_id;
            self.cancel(request_id)
        }

        pub fn choose(&mut self, token: DialogToken, choice: Choice) -> Resolution {
            if !self.dialog_is_current(token) {
                return Resolution::None;
            }
            match choice {
                Choice::Wait => {
                    let attempt = self.pending.as_mut().unwrap();
                    let Some(generation) = attempt.timer.generation.checked_add(1) else {
                        return self
                            .cancel(token.request_id)
                            .map_or(Resolution::None, Resolution::Cancel);
                    };
                    attempt.dialog = None;
                    attempt.timer.generation = generation;
                    attempt.timer.delay_secs = RECOVERY_WAIT_SECS;
                    if attempt.retryable {
                        attempt.retryable = false;
                        attempt.retry_id = Some(token.ticket);
                        Resolution::Retry(attempt.timer, token.ticket)
                    } else {
                        Resolution::Wait(attempt.timer)
                    }
                }
                Choice::Force => self
                    .complete(token.request_id)
                    .map_or(Resolution::None, Resolution::Exit),
                Choice::Cancel => self
                    .cancel(token.request_id)
                    .map_or(Resolution::None, Resolution::Cancel),
            }
        }
    }

    pub fn parse_request_id(value: &str) -> Option<u64> {
        let id = value.parse::<u64>().ok()?;
        (id > 0 && id.to_string() == value).then_some(id)
    }

    pub fn message_ticket_matches(token: DialogToken, low: usize, high: isize) -> bool {
        let ticket = low as u32 as u64 | ((high as u32 as u64) << 32);
        token.ticket == ticket
    }

    pub struct DialogStrings {
        pub title: &'static str,
        pub instruction: &'static str,
        pub content: &'static str,
        pub wait: &'static str,
        pub force: &'static str,
        pub cancel: &'static str,
    }

    pub fn dialog_strings(
        language: &str,
        system_language: u16,
        reason: RecoveryReason,
    ) -> DialogStrings {
        let korean = match language {
            "ko" => true,
            "en" => false,
            _ => system_language & 0x03ff == 0x0012,
        };
        if korean {
            DialogStrings {
                title: "Juice 종료",
                instruction: "설정 저장이 완료되지 않았습니다",
                content: if reason == RecoveryReason::SaveFailed {
                    "설정을 저장하지 못했습니다. 저장하지 않고 종료하면 최근 변경 사항이 사라질 수 있습니다."
                } else {
                    "설정 저장 응답을 기다리고 있습니다. 저장하지 않고 종료하면 최근 변경 사항이 사라질 수 있습니다."
                },
                wait: if reason == RecoveryReason::SaveFailed {
                    "다시 저장"
                } else {
                    "계속 기다림"
                },
                force: "저장하지 않고 종료",
                cancel: "취소",
            }
        } else {
            DialogStrings {
                title: "Quit Juice",
                instruction: "Settings have not finished saving",
                content: if reason == RecoveryReason::SaveFailed {
                    "Settings could not be saved. Quitting without saving may lose your recent changes."
                } else {
                    "Waiting for settings to finish saving. Quitting without saving may lose your recent changes."
                },
                wait: if reason == RecoveryReason::SaveFailed {
                    "Retry saving"
                } else {
                    "Keep waiting"
                },
                force: "Quit without saving",
                cancel: "Cancel",
            }
        }
    }
}

#[derive(Default)]
pub(crate) struct QuitState(Mutex<core::Machine>);

#[derive(Clone, serde::Serialize)]
struct Requested {
    request_id: String,
    retry_id: Option<String>,
}

impl Requested {
    fn new(request_id: u64, retry_id: Option<u64>) -> Self {
        Self {
            request_id: request_id.to_string(),
            retry_id: retry_id.map(|id| id.to_string()),
        }
    }
}

#[derive(serde::Serialize)]
pub(crate) struct QuitRequestSnapshot {
    pending: Option<Requested>,
    retired_through: Option<String>,
}

#[derive(Clone, serde::Serialize)]
struct Cancelled {
    request_id: String,
    reason: &'static str,
}

fn with_state<T>(
    app: &tauri::AppHandle,
    update: impl FnOnce(&mut core::Machine) -> T,
) -> Option<T> {
    let state = app.try_state::<QuitState>()?;
    let mut machine = state
        .0
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    Some(update(&mut machine))
}

fn publish_cancel(app: &tauri::AppHandle, retired: core::Retired, reason: &'static str) {
    dismiss(retired.dialog_handle, retired.dialog_token);
    let _ = app.emit(
        "app-quit-cancelled",
        Cancelled {
            request_id: retired.request_id.to_string(),
            reason,
        },
    );
}

pub(crate) fn cancel_for_activation(app: &tauri::AppHandle) {
    if let Some(Some(retired)) = with_state(app, core::Machine::cancel_current) {
        publish_cancel(app, retired, "activation");
    }
}

fn schedule_timeout(app: tauri::AppHandle, timer: core::Timer) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(timer.delay_secs)).await;
        recover(
            &app,
            timer.request_id,
            Some(timer),
            core::RecoveryReason::Unresponsive,
        );
    });
}

pub(crate) fn request_app_quit(app: &tauri::AppHandle) {
    let Some(Some((timer, fresh))) = with_state(app, core::Machine::request) else {
        return;
    };
    if fresh {
        start_save(app, timer, None);
    } else {
        recover(
            app,
            timer.request_id,
            None,
            core::RecoveryReason::RepeatedRequest,
        );
    }
}

fn start_save(app: &tauri::AppHandle, timer: core::Timer, retry_id: Option<u64>) {
    schedule_timeout(app.clone(), timer);
    if app
        .emit(
            "app-quit-requested",
            Requested::new(timer.request_id, retry_id),
        )
        .is_err()
    {
        report_failure(app, timer.request_id, retry_id, retry_id.is_some());
    }
}

fn recover(
    app: &tauri::AppHandle,
    request_id: u64,
    timer: Option<core::Timer>,
    reason: core::RecoveryReason,
) {
    let recovery = with_state(app, |machine| machine.recover(request_id, timer));
    show_recovery(app, recovery, reason);
}

fn report_failure(app: &tauri::AppHandle, request_id: u64, retry_id: Option<u64>, retryable: bool) {
    let recovery = with_state(app, |machine| {
        machine.fail_and_recover(request_id, retry_id, retryable)
    });
    let reason = if retryable {
        core::RecoveryReason::SaveFailed
    } else {
        core::RecoveryReason::Unresponsive
    };
    show_recovery(app, recovery, reason);
}

fn show_recovery(
    app: &tauri::AppHandle,
    recovery: Option<core::Recovery>,
    reason: core::RecoveryReason,
) {
    match recovery {
        Some(core::Recovery::Show(token)) => spawn_dialog(app, token, reason),
        Some(core::Recovery::Focus(handle)) => focus_dialog(handle),
        _ => {}
    }
}

fn dialog_failed(app: &tauri::AppHandle, token: core::DialogToken) {
    if let Some(core::Resolution::Cancel(retired)) =
        with_state(app, |machine| machine.choose(token, core::Choice::Cancel))
    {
        publish_cancel(app, retired, "native-dialog-unavailable");
    }
}

fn resolve_dialog(app: &tauri::AppHandle, token: core::DialogToken, choice: core::Choice) {
    match with_state(app, |machine| machine.choose(token, choice)) {
        Some(core::Resolution::Wait(timer)) => schedule_timeout(app.clone(), timer),
        Some(core::Resolution::Retry(timer, retry_id)) => {
            start_save(app, timer, Some(retry_id));
        }
        Some(core::Resolution::Exit(_)) => super::exit_after_taskbar_cleanup(app.clone()),
        Some(core::Resolution::Cancel(retired)) => publish_cancel(app, retired, "cancel"),
        _ => {}
    }
}

#[tauri::command]
pub(crate) fn get_pending_quit_request(
    window: tauri::Window,
    app: tauri::AppHandle,
) -> Result<QuitRequestSnapshot, String> {
    super::ensure_panel_command(window.label())?;
    with_state(&app, |machine| QuitRequestSnapshot {
        pending: machine
            .pending_request()
            .map(|(request_id, retry_id)| Requested::new(request_id, retry_id)),
        retired_through: machine.retired_through().map(|id| id.to_string()),
    })
    .ok_or_else(|| "quit state unavailable".into())
}

#[tauri::command]
pub(crate) fn complete_app_quit(
    window: tauri::Window,
    app: tauri::AppHandle,
    request_id: String,
) -> Result<bool, String> {
    super::ensure_panel_command(window.label())?;
    let request_id = core::parse_request_id(&request_id).ok_or("invalid quit request ID")?;
    let Some(Some(retired)) = with_state(&app, |machine| machine.complete(request_id)) else {
        return Ok(false);
    };
    dismiss(retired.dialog_handle, retired.dialog_token);
    super::exit_after_taskbar_cleanup(app);
    Ok(true)
}

#[tauri::command]
pub(crate) fn fail_app_quit(
    window: tauri::Window,
    app: tauri::AppHandle,
    request_id: String,
    retry_id: Option<String>,
    retryable: Option<bool>,
) -> Result<(), String> {
    super::ensure_panel_command(window.label())?;
    let request_id = core::parse_request_id(&request_id).ok_or("invalid quit request ID")?;
    let retry_id = retry_id
        .as_deref()
        .map(|id| core::parse_request_id(id).ok_or("invalid quit retry ID"))
        .transpose()?;
    report_failure(&app, request_id, retry_id, retryable == Some(true));
    Ok(())
}

#[cfg(windows)]
fn dismiss(handle: Option<isize>, token: Option<core::DialogToken>) {
    if let (Some(handle), Some(token)) = (handle, token) {
        native::post_dismiss(windows::Win32::Foundation::HWND(handle as *mut _), token);
    }
}

#[cfg(not(windows))]
fn dismiss(_: Option<isize>, _: Option<core::DialogToken>) {}

#[cfg(windows)]
fn focus_dialog(handle: isize) {
    use windows::Win32::{Foundation::HWND, UI::WindowsAndMessaging::SetForegroundWindow};
    let _ = unsafe { SetForegroundWindow(HWND(handle as *mut _)) };
}

#[cfg(not(windows))]
fn focus_dialog(_: isize) {}

#[cfg(windows)]
fn spawn_dialog(app: &tauri::AppHandle, token: core::DialogToken, reason: core::RecoveryReason) {
    let dialog_app = app.clone();
    if std::thread::Builder::new()
        .name("juice-quit-recovery".into())
        .spawn(move || {
            if with_state(&dialog_app, |machine| machine.dialog_is_current(token)) != Some(true) {
                return;
            }
            match native::show(&dialog_app, token, reason) {
                Ok(choice) => resolve_dialog(&dialog_app, token, choice),
                Err(()) => dialog_failed(&dialog_app, token),
            }
        })
        .is_err()
    {
        dialog_failed(app, token);
    }
}

#[cfg(not(windows))]
fn spawn_dialog(app: &tauri::AppHandle, token: core::DialogToken, _: core::RecoveryReason) {
    dialog_failed(app, token);
}

#[cfg(windows)]
mod native {
    use super::*;
    use std::{cell::Cell, sync::OnceLock};
    use windows::{
        core::{HRESULT, PCWSTR},
        Win32::{
            Foundation::{HWND, LPARAM, LRESULT, WPARAM},
            UI::{
                Controls::{
                    TaskDialogIndirect, TASKDIALOGCONFIG, TASKDIALOG_BUTTON,
                    TASKDIALOG_NOTIFICATIONS, TDE_CONTENT, TDF_ALLOW_DIALOG_CANCELLATION,
                    TDF_CALLBACK_TIMER, TDM_CLICK_BUTTON, TDM_SET_ELEMENT_TEXT, TDN_CREATED,
                    TDN_DESTROYED, TDN_TIMER,
                },
                Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
                WindowsAndMessaging::{
                    PostMessageW, RegisterWindowMessageW, SendMessageW, SetDlgItemTextW,
                    WM_NCDESTROY,
                },
            },
        },
    };

    const WAIT: i32 = 101;
    const FORCE: i32 = 102;
    const CANCEL: i32 = 2;
    const SUBCLASS_ID: usize = 0x4a515554;

    struct Context {
        app: tauri::AppHandle,
        token: core::DialogToken,
        language: String,
        system_language: u16,
        retryable: Cell<bool>,
    }

    fn dismiss_message() -> u32 {
        static MESSAGE: OnceLock<u32> = OnceLock::new();
        *MESSAGE.get_or_init(|| unsafe {
            RegisterWindowMessageW(windows::core::w!("Juice.QuitRecovery.Dismiss.v1"))
        })
    }

    pub(super) fn post_dismiss(hwnd: HWND, token: core::DialogToken) {
        let message = dismiss_message();
        if message != 0 {
            let _ = unsafe {
                PostMessageW(
                    Some(hwnd),
                    message,
                    WPARAM(token.ticket as u32 as usize),
                    LPARAM((token.ticket >> 32) as u32 as isize),
                )
            };
        }
    }

    fn close_on_dialog_thread(hwnd: HWND) {
        let _ = unsafe {
            SendMessageW(
                hwnd,
                TDM_CLICK_BUTTON.0 as u32,
                Some(WPARAM(CANCEL as usize)),
                Some(LPARAM(0)),
            )
        };
    }

    unsafe extern "system" fn subclass(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
        id: usize,
        data: usize,
    ) -> LRESULT {
        if message != 0 && message == dismiss_message() {
            let context = unsafe { &*(data as *const Context) };
            if core::message_ticket_matches(context.token, wparam.0, lparam.0) {
                close_on_dialog_thread(hwnd);
            }
            return LRESULT(0);
        }
        if message == WM_NCDESTROY {
            let _ = unsafe { RemoveWindowSubclass(hwnd, Some(subclass), id) };
        }
        unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
    }

    fn update_retry_label(hwnd: HWND, context: &Context, retryable: bool) {
        let reason = if retryable {
            core::RecoveryReason::SaveFailed
        } else {
            core::RecoveryReason::Unresponsive
        };
        let strings = core::dialog_strings(&context.language, context.system_language, reason);
        let wait = wide(strings.wait);
        let content = wide(strings.content);
        let _ = unsafe { SetDlgItemTextW(hwnd, WAIT, PCWSTR(wait.as_ptr())) };
        let _ = unsafe {
            SendMessageW(
                hwnd,
                TDM_SET_ELEMENT_TEXT.0 as u32,
                Some(WPARAM(TDE_CONTENT.0 as usize)),
                Some(LPARAM(content.as_ptr() as isize)),
            )
        };
    }

    unsafe extern "system" fn callback(
        hwnd: HWND,
        message: TASKDIALOG_NOTIFICATIONS,
        _: WPARAM,
        _: LPARAM,
        data: isize,
    ) -> HRESULT {
        // TaskDialogIndirect is synchronous; Context stays alive until all callbacks end.
        let context = unsafe { &*(data as *const Context) };
        if message == TDN_CREATED {
            let _ = unsafe { SetWindowSubclass(hwnd, Some(subclass), SUBCLASS_ID, data as usize) };
            if with_state(&context.app, |machine| {
                machine.dialog_created(context.token, hwnd.0 as isize)
            }) != Some(true)
            {
                post_dismiss(hwnd, context.token);
            }
        } else if message == TDN_DESTROYED {
            let _ = with_state(&context.app, |machine| machine.dialog_closed(context.token));
        } else if message == TDN_TIMER {
            match with_state(&context.app, |machine| machine.can_retry(context.token)).flatten() {
                None => close_on_dialog_thread(hwnd),
                Some(retryable) if context.retryable.replace(retryable) != retryable => {
                    update_retry_label(hwnd, context, retryable);
                }
                _ => {}
            }
        }
        HRESULT(0)
    }

    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(Some(0)).collect()
    }

    pub(super) fn show(
        app: &tauri::AppHandle,
        token: core::DialogToken,
        reason: core::RecoveryReason,
    ) -> Result<core::Choice, ()> {
        // Never wait for settings publication while a failed save may hold its lock.
        let language = super::super::SETTINGS_PUBLICATION_STATE
            .try_lock()
            .ok()
            .and_then(|state| {
                state
                    .settings
                    .as_ref()
                    .map(|settings| settings.language.clone())
            })
            .unwrap_or_else(|| "system".into());
        let retryable = with_state(app, |machine| machine.can_retry(token))
            .flatten()
            .unwrap_or(false);
        let reason = if retryable {
            core::RecoveryReason::SaveFailed
        } else {
            reason
        };
        let system_language = super::super::system_ui_language();
        let strings = core::dialog_strings(&language, system_language, reason);
        let title = wide(strings.title);
        let instruction = wide(strings.instruction);
        let content = wide(strings.content);
        let wait = wide(strings.wait);
        let force = wide(strings.force);
        let cancel = wide(strings.cancel);
        let buttons = [
            TASKDIALOG_BUTTON {
                nButtonID: WAIT,
                pszButtonText: PCWSTR(wait.as_ptr()),
            },
            TASKDIALOG_BUTTON {
                nButtonID: FORCE,
                pszButtonText: PCWSTR(force.as_ptr()),
            },
            TASKDIALOG_BUTTON {
                nButtonID: CANCEL,
                pszButtonText: PCWSTR(cancel.as_ptr()),
            },
        ];
        let context = Context {
            app: app.clone(),
            token,
            language,
            system_language,
            retryable: Cell::new(retryable),
        };
        let config = TASKDIALOGCONFIG {
            cbSize: std::mem::size_of::<TASKDIALOGCONFIG>() as u32,
            dwFlags: TDF_ALLOW_DIALOG_CANCELLATION | TDF_CALLBACK_TIMER,
            pszWindowTitle: PCWSTR(title.as_ptr()),
            pszMainInstruction: PCWSTR(instruction.as_ptr()),
            pszContent: PCWSTR(content.as_ptr()),
            cButtons: buttons.len() as u32,
            pButtons: buttons.as_ptr(),
            nDefaultButton: WAIT,
            pfCallback: Some(callback),
            lpCallbackData: &context as *const Context as isize,
            ..Default::default()
        };
        let mut button = CANCEL;
        let result = unsafe { TaskDialogIndirect(&config, Some(&mut button), None, None) };
        let _ = with_state(app, |machine| machine.dialog_closed(token));
        result.map_err(|_| ())?;
        Ok(match button {
            WAIT => core::Choice::Wait,
            FORCE => core::Choice::Force,
            _ => core::Choice::Cancel,
        })
    }
}

#[cfg(test)]
#[path = "quit-core.test.rs"]
mod tests;
