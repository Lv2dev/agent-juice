use super::core::*;

fn begin(machine: &mut Machine) -> Timer {
    let (timer, fresh) = machine.request().unwrap();
    assert!(fresh);
    assert_eq!(timer.delay_secs, INITIAL_WAIT_SECS);
    timer
}

fn dialog(machine: &mut Machine, timer: Timer) -> DialogToken {
    match machine.recover(timer.request_id, Some(timer)) {
        Recovery::Show(token) => token,
        other => panic!("expected a new recovery dialog, got {other:?}"),
    }
}

#[test]
fn listener_can_read_a_missed_request_after_repeated_waits() {
    let mut machine = Machine::default();
    assert_eq!(machine.pending_request(), None);
    assert_eq!(machine.retired_through(), None);
    let mut timer = begin(&mut machine);
    let request_id = timer.request_id;
    for _ in 0..2 {
        let token = dialog(&mut machine, timer);
        let Resolution::Wait(next) = machine.choose(token, Choice::Wait) else {
            panic!()
        };
        timer = next;
        assert_eq!(machine.pending_request(), Some((request_id, None)));
        assert_eq!(machine.retired_through(), None);
    }
    assert_eq!(machine.request(), Some((timer, false)));
    assert!(machine.complete(request_id).is_some());
    assert_eq!(machine.pending_request(), None);
    assert_eq!(machine.retired_through(), Some(request_id));
}

#[test]
fn listener_reads_current_retry_and_cancel_watermark_without_changing_either() {
    let mut machine = Machine::default();
    let retired = begin(&mut machine).request_id;
    machine.cancel_current().unwrap();
    let request_id = begin(&mut machine).request_id;
    let mut retry_id = None;
    for _ in 0..2 {
        let Recovery::Show(token) = machine.fail_and_recover(request_id, retry_id, true) else {
            panic!()
        };
        let Resolution::Retry(timer, next_retry) = machine.choose(token, Choice::Wait) else {
            panic!()
        };
        retry_id = Some(next_retry);
        let waiting = dialog(&mut machine, timer);
        let Resolution::Wait(_) = machine.choose(waiting, Choice::Wait) else {
            panic!()
        };
        assert_eq!(machine.pending_request(), Some((request_id, retry_id)));
        assert_eq!(machine.retired_through(), Some(retired));
    }
    machine.cancel_current().unwrap();
    assert_eq!(machine.pending_request(), None);
    assert_eq!(machine.retired_through(), Some(request_id));
}

#[test]
fn old_completion_timeout_and_cancel_cannot_consume_new_request() {
    let mut machine = Machine::default();
    let old = begin(&mut machine);
    machine.cancel(old.request_id).unwrap();
    let current = begin(&mut machine);
    assert!(current.request_id > old.request_id);
    assert_eq!(machine.complete(old.request_id), None);
    assert_eq!(machine.cancel(old.request_id), None);
    assert_eq!(machine.recover(old.request_id, Some(old)), Recovery::None);
    assert_eq!(machine.request(), Some((current, false)));
    assert!(machine.complete(current.request_id).is_some());
    assert_eq!(
        machine.request(),
        None,
        "completion commits exit exactly once"
    );
    assert_eq!(machine.complete(current.request_id), None);
}

#[test]
fn old_cancel_cannot_consume_current_request() {
    let mut machine = Machine::default();
    let old = begin(&mut machine);
    machine.cancel(old.request_id).unwrap();
    let current = begin(&mut machine);
    assert_eq!(machine.cancel(old.request_id), None);
    assert_eq!(machine.request(), Some((current, false)));
}

#[test]
fn old_timeout_cannot_consume_current_request() {
    let mut machine = Machine::default();
    let old = begin(&mut machine);
    machine.cancel(old.request_id).unwrap();
    let current = begin(&mut machine);
    assert_eq!(machine.recover(old.request_id, Some(old)), Recovery::None);
    assert_eq!(machine.request(), Some((current, false)));
}

#[test]
fn unresponsive_frontend_still_has_native_recovery() {
    let mut machine = Machine::default();
    let timer = begin(&mut machine);
    let token = dialog(&mut machine, timer);
    assert!(machine.dialog_created(token, 123));
    assert_eq!(
        machine.recover(timer.request_id, None),
        Recovery::Focus(123)
    );
    assert!(matches!(
        machine.choose(token, Choice::Force),
        Resolution::Exit(_)
    ));
    assert_eq!(machine.request(), None);
}

#[test]
fn save_failure_can_recover_without_waiting_for_timeout() {
    let mut machine = Machine::default();
    let timer = begin(&mut machine);
    let Recovery::Show(token) = machine.fail_and_recover(timer.request_id, None, true) else {
        panic!()
    };
    assert_eq!(machine.can_retry(token), Some(true));
    assert_eq!(
        machine.recover(timer.request_id, Some(timer)),
        Recovery::None,
        "a racing timeout must not create a duplicate worker"
    );
    assert!(matches!(
        machine.choose(token, Choice::Cancel),
        Resolution::Cancel(_)
    ));
    assert!(machine.request().unwrap().1);
}

#[test]
fn wait_keeps_request_but_retires_old_timer_and_dialog_ticket() {
    let mut machine = Machine::default();
    let old_timer = begin(&mut machine);
    let old_dialog = dialog(&mut machine, old_timer);
    let Resolution::Wait(timer) = machine.choose(old_dialog, Choice::Wait) else {
        panic!()
    };
    assert_eq!(timer.request_id, old_timer.request_id);
    assert!(timer.generation > old_timer.generation);
    assert_eq!(timer.delay_secs, RECOVERY_WAIT_SECS);
    assert_eq!(
        machine.request(),
        Some((timer, false)),
        "do not restart the save"
    );
    assert_eq!(
        machine.recover(timer.request_id, Some(old_timer)),
        Recovery::None
    );
    let current = dialog(&mut machine, timer);
    assert_ne!(current, old_dialog);
    assert!(!machine.dialog_created(old_dialog, 44));
    for choice in [Choice::Wait, Choice::Force, Choice::Cancel] {
        assert_eq!(machine.choose(old_dialog, choice), Resolution::None);
    }
    assert!(machine.dialog_created(current, 45));
    assert!(matches!(
        machine.choose(current, Choice::Cancel),
        Resolution::Cancel(_)
    ));
}

#[test]
fn wait_does_not_prevent_the_original_save_from_completing() {
    let mut machine = Machine::default();
    let timer = begin(&mut machine);
    let token = dialog(&mut machine, timer);
    assert!(matches!(
        machine.choose(token, Choice::Wait),
        Resolution::Wait(_)
    ));
    assert!(machine.complete(timer.request_id).is_some());
    assert_eq!(machine.recover(timer.request_id, None), Recovery::None);
}

#[test]
fn completion_dismisses_current_handle_and_rejects_late_dialog_choices() {
    for choice in [Choice::Wait, Choice::Force, Choice::Cancel] {
        let mut machine = Machine::default();
        let timer = begin(&mut machine);
        let token = dialog(&mut machine, timer);
        assert!(machine.dialog_created(token, 88));
        assert_eq!(
            machine.complete(timer.request_id).unwrap().dialog_handle,
            Some(88)
        );
        assert!(!machine.dialog_is_current(token));
        assert_eq!(machine.choose(token, choice), Resolution::None);
    }
}

#[test]
fn activation_cancels_before_created_and_cannot_poison_the_next_request() {
    let mut machine = Machine::default();
    let old = begin(&mut machine);
    let old_dialog = dialog(&mut machine, old);
    assert_eq!(machine.cancel_current().unwrap().request_id, old.request_id);
    let current = begin(&mut machine);
    let current_dialog = dialog(&mut machine, current);
    assert!(!machine.dialog_created(old_dialog, 99));
    assert_eq!(machine.choose(old_dialog, Choice::Force), Resolution::None);
    assert!(machine.dialog_created(current_dialog, 100));
    assert_eq!(
        machine.recover(current.request_id, None),
        Recovery::Focus(100)
    );
}

#[test]
fn activation_dismisses_visible_dialog_and_stale_creation_cannot_register() {
    let mut machine = Machine::default();
    let timer = begin(&mut machine);
    let token = dialog(&mut machine, timer);
    assert!(machine.dialog_created(token, 111));
    assert_eq!(machine.cancel_current().unwrap().dialog_handle, Some(111));
    assert!(!machine.dialog_created(token, 112));
    assert_eq!(machine.complete(timer.request_id), None);
    assert_eq!(machine.cancel_current(), None);
}

#[test]
fn save_can_finish_before_the_dialog_handle_is_created() {
    let mut machine = Machine::default();
    let timer = begin(&mut machine);
    let token = dialog(&mut machine, timer);
    assert_eq!(
        machine.complete(timer.request_id).unwrap().dialog_handle,
        None
    );
    assert!(!machine.dialog_created(token, 777));
    assert!(!machine.dialog_is_current(token));
}

#[test]
fn failed_dialog_or_worker_allows_a_fresh_tray_request() {
    let mut machine = Machine::default();
    let timer = begin(&mut machine);
    let token = dialog(&mut machine, timer);
    assert!(matches!(
        machine.choose(token, Choice::Cancel),
        Resolution::Cancel(_)
    ));
    let current = begin(&mut machine);
    assert!(current.request_id > timer.request_id);
    assert_eq!(machine.choose(token, Choice::Cancel), Resolution::None);
    assert_eq!(machine.request(), Some((current, false)));
}

#[test]
fn repeated_tray_requests_share_one_native_dialog_until_created() {
    let mut machine = Machine::default();
    let timer = begin(&mut machine);
    let token = dialog(&mut machine, timer);
    for _ in 0..8 {
        assert_eq!(machine.request(), Some((timer, false)));
        assert_eq!(machine.recover(timer.request_id, None), Recovery::None);
    }
    machine.dialog_created(token, 3);
    assert_eq!(machine.recover(timer.request_id, None), Recovery::Focus(3));
}

#[test]
fn request_ids_are_canonical_strings_and_keep_u64_precision() {
    assert_eq!(
        parse_request_id("9007199254740993"),
        Some(9_007_199_254_740_993)
    );
    assert_eq!(parse_request_id("18446744073709551615"), Some(u64::MAX));
    for invalid in [
        "",
        "0",
        "01",
        "+1",
        "-1",
        "1.0",
        " 1",
        "18446744073709551616",
    ] {
        assert_eq!(parse_request_id(invalid), None);
    }
}

#[test]
fn destroyed_dialog_handles_are_not_dismissed_again_or_allowed_to_clear_new_handles() {
    let mut machine = Machine::default();
    let timer = begin(&mut machine);
    let old = dialog(&mut machine, timer);
    machine.dialog_created(old, 123);
    machine.dialog_closed(old);
    let Resolution::Wait(timer) = machine.choose(old, Choice::Wait) else {
        panic!()
    };
    let current = dialog(&mut machine, timer);
    machine.dialog_created(current, 124);
    machine.dialog_closed(old);
    assert_eq!(
        machine.complete(timer.request_id).unwrap().dialog_handle,
        Some(124)
    );

    let mut machine = Machine::default();
    let timer = begin(&mut machine);
    let token = dialog(&mut machine, timer);
    machine.dialog_created(token, 125);
    machine.dialog_closed(token);
    let Resolution::Cancel(retired) = machine.choose(token, Choice::Cancel) else {
        panic!()
    };
    assert_eq!(retired.dialog_handle, None);
}

#[test]
fn native_strings_follow_settings_language_then_system_language() {
    for reason in [
        RecoveryReason::SaveFailed,
        RecoveryReason::Unresponsive,
        RecoveryReason::RepeatedRequest,
    ] {
        let korean = dialog_strings("ko", 0x0409, reason);
        let english = dialog_strings("en", 0x0412, reason);
        assert_eq!(
            korean.wait,
            if reason == RecoveryReason::SaveFailed {
                "다시 저장"
            } else {
                "계속 기다림"
            }
        );
        assert_eq!(korean.force, "저장하지 않고 종료");
        assert_eq!(korean.cancel, "취소");
        assert_eq!(
            english.wait,
            if reason == RecoveryReason::SaveFailed {
                "Retry saving"
            } else {
                "Keep waiting"
            }
        );
        assert_eq!(english.force, "Quit without saving");
        assert_eq!(english.cancel, "Cancel");
        assert_eq!(dialog_strings("system", 0x0412, reason).title, korean.title);
        assert_eq!(
            dialog_strings("system", 0x0409, reason).title,
            english.title
        );
        assert!(!korean.instruction.is_empty() && !english.content.is_empty());
    }
}

#[test]
fn failure_in_an_open_dialog_cannot_reopen_it_after_retry() {
    let mut machine = Machine::default();
    let initial = begin(&mut machine);
    let token = dialog(&mut machine, initial);
    assert!(machine.dialog_created(token, 123));
    assert_eq!(
        machine.fail_and_recover(initial.request_id, None, true),
        Recovery::Focus(123)
    );
    assert_eq!(machine.can_retry(token), Some(true));
    let Resolution::Retry(timer, retry_id) = machine.choose(token, Choice::Wait) else {
        panic!()
    };
    assert_eq!(retry_id, token.ticket);
    assert_eq!(timer.request_id, initial.request_id);
    assert_eq!(timer.delay_secs, 10);
    assert_eq!(machine.request(), Some((timer, false)));
    assert_eq!(machine.choose(token, Choice::Wait), Resolution::None);
    assert_eq!(
        machine.recover(initial.request_id, Some(initial)),
        Recovery::None
    );
    for stale_id in [None, Some(retry_id + 1)] {
        for retryable in [true, false] {
            assert_eq!(
                machine.fail_and_recover(initial.request_id, stale_id, retryable),
                Recovery::None
            );
        }
    }
    let waiting = dialog(&mut machine, timer);
    assert_eq!(machine.can_retry(waiting), Some(false));
    assert!(matches!(
        machine.choose(waiting, Choice::Wait),
        Resolution::Wait(_)
    ));
    assert!(machine.complete(initial.request_id).is_some());
}

#[test]
fn wait_keeps_the_save_id_for_both_initial_and_retried_failures() {
    for after_retry in [false, true] {
        let mut machine = Machine::default();
        let initial = begin(&mut machine);
        let request_id = initial.request_id;
        let (timer, retry_id) = if after_retry {
            let Recovery::Show(token) = machine.fail_and_recover(request_id, None, true) else {
                panic!()
            };
            let Resolution::Retry(timer, retry_id) = machine.choose(token, Choice::Wait) else {
                panic!()
            };
            (timer, Some(retry_id))
        } else {
            (initial, None)
        };
        let token = dialog(&mut machine, timer);
        let Resolution::Wait(next) = machine.choose(token, Choice::Wait) else {
            panic!()
        };
        assert!(next.generation > timer.generation);
        let Recovery::Show(token) = machine.fail_and_recover(request_id, retry_id, false) else {
            panic!()
        };
        assert_eq!(machine.can_retry(token), Some(false));
        assert!(machine.dialog_created(token, 124));
        assert_eq!(
            machine.fail_and_recover(request_id, retry_id, true),
            Recovery::Focus(124)
        );
        assert_eq!(machine.can_retry(token), Some(true));
        assert_eq!(
            machine.fail_and_recover(request_id, retry_id, false),
            Recovery::Focus(124)
        );
        assert_eq!(machine.can_retry(token), Some(true));
    }
}

#[test]
fn retry_failure_gets_a_new_ticket_and_an_old_failure_cannot_mark_a_new_request() {
    let mut machine = Machine::default();
    let old = begin(&mut machine);
    machine.cancel(old.request_id).unwrap();
    let current = begin(&mut machine);
    assert_eq!(
        machine.fail_and_recover(old.request_id, None, true),
        Recovery::None
    );
    let first = dialog(&mut machine, current);
    assert_eq!(machine.can_retry(first), Some(false));
    assert_eq!(
        machine.fail_and_recover(current.request_id, None, true),
        Recovery::None,
        "mark the queued dialog without reserving a second one"
    );
    assert_eq!(machine.can_retry(first), Some(true));
    let Resolution::Retry(_, retry_id) = machine.choose(first, Choice::Wait) else {
        panic!()
    };
    let Recovery::Show(second) = machine.fail_and_recover(current.request_id, Some(retry_id), true)
    else {
        panic!()
    };
    let Resolution::Retry(timer, next_retry) = machine.choose(second, Choice::Wait) else {
        panic!()
    };
    assert!(next_retry > retry_id);
    assert_eq!(timer.request_id, current.request_id);
    assert_eq!(timer.delay_secs, 10);
    assert_eq!(machine.choose(first, Choice::Force), Resolution::None);
    for retryable in [true, false] {
        assert_eq!(
            machine.fail_and_recover(current.request_id, Some(retry_id), retryable),
            Recovery::None
        );
    }
    let active = dialog(&mut machine, timer);
    assert_eq!(machine.can_retry(active), Some(false));
}

#[test]
fn posted_dismiss_ticket_cannot_close_a_recycled_handle_with_a_new_dialog() {
    let old = DialogToken {
        request_id: 1,
        ticket: 0x1_ffff_ffff,
    };
    let current = DialogToken {
        request_id: 2,
        ticket: 0x2_ffff_ffff,
    };
    let low = old.ticket as u32 as usize;
    let high = (old.ticket >> 32) as u32 as isize;
    assert!(message_ticket_matches(old, low, high));
    assert!(!message_ticket_matches(current, low, high));
    let max = DialogToken {
        request_id: 3,
        ticket: u64::MAX,
    };
    assert!(message_ticket_matches(max, u32::MAX as usize, -1));
}
