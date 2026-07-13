#[cfg(windows)]
use once_cell::sync::Lazy;
#[cfg(windows)]
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
#[cfg(windows)]
use windows::{
    core::{w, BOOL, PCWSTR, PWSTR},
    Win32::{
        Foundation::{HWND, LPARAM, RECT},
        Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED},
        Graphics::Gdi::{
            GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
        },
        System::Threading::GetCurrentThreadId,
        UI::{
            Controls::{
                TOOLTIPS_CLASSW, TTF_ABSOLUTE, TTF_TRACK, TTM_ADDTOOLW, TTM_DELTOOLW,
                TTM_GETBUBBLESIZE, TTM_SETMAXTIPWIDTH, TTM_TRACKACTIVATE, TTM_TRACKPOSITION,
                TTM_UPDATETIPTEXTW, TTS_ALWAYSTIP, TTS_NOPREFIX, TTTOOLINFOW,
            },
            WindowsAndMessaging::{
                CreateWindowExW, DestroyWindow, DispatchMessageW, EnumWindows, FindWindowW,
                GetClassNameW, GetWindowRect, GetWindowTextW, GetWindowThreadProcessId, IsIconic,
                IsWindow, IsWindowVisible, PeekMessageW, SendMessageW, ShowWindow,
                TranslateMessage, MSG, PM_REMOVE, SW_HIDE, SW_SHOWNOACTIVATE, WINDOW_STYLE,
                WS_EX_NOACTIVATE, WS_EX_TOPMOST, WS_POPUP,
            },
        },
    },
};

#[cfg(windows)]
#[derive(Clone)]
struct NativeTooltip {
    hwnd: isize,
    owner_thread_id: u32,
    text: Arc<Vec<u16>>,
}

#[cfg(windows)]
static NATIVE_TOOLTIPS: Lazy<Mutex<HashMap<isize, NativeTooltip>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

#[cfg(windows)]
fn tracking_tool_info(tooltip: HWND, text: &[u16]) -> TTTOOLINFOW {
    TTTOOLINFOW {
        cbSize: std::mem::size_of::<TTTOOLINFOW>() as u32,
        uFlags: TTF_TRACK | TTF_ABSOLUTE,
        hwnd: tooltip,
        uId: 1,
        lpszText: PWSTR(text.as_ptr() as *mut u16),
        ..Default::default()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DockRect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

pub fn taskbar_tooltip_anchor(
    bar: DockRect,
    work_area: DockRect,
    bubble_size: (i32, i32),
) -> (i32, i32) {
    const EDGE_GAP: i32 = 8;

    let bar_right = bar.x.saturating_add(bar.width);
    let bar_bottom = bar.y.saturating_add(bar.height);
    let work_right = work_area.x.saturating_add(work_area.width);
    let work_bottom = work_area.y.saturating_add(work_area.height);
    let (bubble_width, bubble_height) = (bubble_size.0.max(0), bubble_size.1.max(0));
    let candidate = if bar_bottom <= work_area.y {
        (
            bar.x.saturating_add(EDGE_GAP),
            bar_bottom.saturating_add(EDGE_GAP),
        )
    } else if bar.y >= work_bottom {
        (
            bar.x.saturating_add(EDGE_GAP),
            bar.y.saturating_sub(EDGE_GAP).saturating_sub(bubble_height),
        )
    } else if bar_right <= work_area.x {
        (
            bar_right.saturating_add(EDGE_GAP),
            bar.y.saturating_add(EDGE_GAP),
        )
    } else if bar.x >= work_right {
        (
            bar.x.saturating_sub(EDGE_GAP).saturating_sub(bubble_width),
            bar.y.saturating_add(EDGE_GAP),
        )
    } else {
        (
            bar.x.saturating_add(EDGE_GAP),
            bar.y.saturating_sub(EDGE_GAP).saturating_sub(bubble_height),
        )
    };

    fn clamp_axis(value: i32, size: i32, start: i32, length: i32) -> i32 {
        let end = start.saturating_add(length.max(0));
        let last = end.saturating_sub(size).max(start);
        value.clamp(start, last)
    }

    (
        clamp_axis(candidate.0, bubble_width, work_area.x, work_area.width),
        clamp_axis(candidate.1, bubble_height, work_area.y, work_area.height),
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskbarTarget {
    pub key: String,
    pub rect: DockRect,
    pub monitor: DockRect,
    pub primary: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowCoverageCandidate {
    pub hwnd: isize,
    pub visible: bool,
    pub minimized: bool,
    pub cloaked: bool,
    pub rect: DockRect,
    pub monitor: DockRect,
    pub work_area: DockRect,
}

#[cfg(windows)]
#[derive(Clone)]
pub struct ShellTaskbarWindow {
    pub hwnd: HWND,
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
    pub monitor: DockRect,
    pub key: String,
    pub primary: bool,
}

#[cfg(windows)]
pub fn hide_window(hwnd: HWND) -> anyhow::Result<()> {
    if hwnd.0.is_null() {
        return Err(anyhow::anyhow!("cannot hide a null HWND"));
    }

    unsafe {
        let _ = ShowWindow(hwnd, SW_HIDE);
    }
    Ok(())
}

#[cfg(windows)]
pub fn window_is_valid(hwnd: HWND) -> bool {
    !hwnd.0.is_null() && unsafe { IsWindow(Some(hwnd)).as_bool() }
}

#[cfg(windows)]
pub fn window_is_visible(hwnd: HWND) -> bool {
    window_is_valid(hwnd) && unsafe { IsWindowVisible(hwnd).as_bool() }
}

#[cfg(windows)]
pub fn pump_current_thread_messages() {
    let mut message = MSG::default();
    unsafe {
        while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
}

#[cfg(windows)]
pub fn set_window_tooltip(parent: HWND, value: &str) -> anyhow::Result<()> {
    if parent.0.is_null() {
        return Err(anyhow::anyhow!("cannot attach a tooltip to a null HWND"));
    }

    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(Some(0)).collect()
    }

    let key = parent.0 as isize;
    let current = NATIVE_TOOLTIPS
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .get(&key)
        .cloned();
    if let Some(current) = current {
        let tooltip = HWND(current.hwnd as *mut core::ffi::c_void);
        if current.owner_thread_id != unsafe { GetCurrentThreadId() } {
            return Err(anyhow::anyhow!(
                "native tooltip must be updated on its owner thread"
            ));
        }
        if unsafe { IsWindow(Some(tooltip)).as_bool() } {
            let next = Arc::new(wide(value));
            let info = tracking_tool_info(tooltip, &next);
            {
                let mut tooltips = NATIVE_TOOLTIPS
                    .lock()
                    .unwrap_or_else(|err| err.into_inner());
                if let Some(stored) = tooltips.get_mut(&key) {
                    stored.text = next.clone();
                }
            }
            unsafe {
                SendMessageW(
                    tooltip,
                    TTM_UPDATETIPTEXTW,
                    None,
                    Some(LPARAM((&info as *const TTTOOLINFOW) as isize)),
                );
            }
            return Ok(());
        }
    }
    NATIVE_TOOLTIPS
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .remove(&key);

    let tooltip = unsafe {
        CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_NOACTIVATE,
            TOOLTIPS_CLASSW,
            PCWSTR::null(),
            WINDOW_STYLE(WS_POPUP.0 | TTS_ALWAYSTIP | TTS_NOPREFIX),
            0,
            0,
            0,
            0,
            None,
            None,
            None,
            None,
        )?
    };
    let text = Arc::new(wide(value));
    let info = tracking_tool_info(tooltip, &text);
    let added = unsafe {
        SendMessageW(
            tooltip,
            TTM_ADDTOOLW,
            None,
            Some(LPARAM((&info as *const TTTOOLINFOW) as isize)),
        )
    };
    if added.0 == 0 {
        let _ = unsafe { DestroyWindow(tooltip) };
        return Err(anyhow::anyhow!("failed to register native taskbar tooltip"));
    }
    unsafe {
        SendMessageW(tooltip, TTM_SETMAXTIPWIDTH, None, Some(LPARAM(360)));
    }
    NATIVE_TOOLTIPS
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .insert(
            key,
            NativeTooltip {
                hwnd: tooltip.0 as isize,
                owner_thread_id: unsafe { GetCurrentThreadId() },
                text,
            },
        );
    Ok(())
}

#[cfg(windows)]
pub fn remove_window_tooltip(parent: HWND) -> anyhow::Result<bool> {
    let key = parent.0 as isize;
    let Some(current) = NATIVE_TOOLTIPS
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .get(&key)
        .cloned()
    else {
        return Ok(false);
    };
    if current.owner_thread_id != unsafe { GetCurrentThreadId() } {
        return Err(anyhow::anyhow!(
            "native tooltip must be removed on its owner thread"
        ));
    }

    let tooltip = HWND(current.hwnd as *mut core::ffi::c_void);
    let result = if unsafe { IsWindow(Some(tooltip)).as_bool() } {
        let info = tracking_tool_info(tooltip, &current.text);
        unsafe {
            SendMessageW(
                tooltip,
                TTM_TRACKACTIVATE,
                Some(windows::Win32::Foundation::WPARAM(0)),
                Some(LPARAM((&info as *const TTTOOLINFOW) as isize)),
            );
            let _ = ShowWindow(tooltip, SW_HIDE);
            SendMessageW(
                tooltip,
                TTM_DELTOOLW,
                None,
                Some(LPARAM((&info as *const TTTOOLINFOW) as isize)),
            );
            DestroyWindow(tooltip)
        }
    } else {
        Ok(())
    };

    NATIVE_TOOLTIPS
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .remove(&key);
    result?;
    Ok(true)
}

#[cfg(windows)]
pub fn clear_current_thread_tooltips() -> anyhow::Result<usize> {
    let owner_thread_id = unsafe { GetCurrentThreadId() };
    let parents = NATIVE_TOOLTIPS
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .iter()
        .filter_map(|(parent, tooltip)| {
            (tooltip.owner_thread_id == owner_thread_id).then_some(*parent)
        })
        .collect::<Vec<_>>();
    let mut removed = 0;
    let mut first_error = None;
    for parent in parents {
        let hwnd = HWND(parent as *mut core::ffi::c_void);
        match remove_window_tooltip(hwnd) {
            Ok(true) => removed += 1,
            Ok(false) => {}
            Err(err) if first_error.is_none() => first_error = Some(err),
            Err(_) => {}
        }
    }
    if let Some(err) = first_error {
        Err(err)
    } else {
        Ok(removed)
    }
}

#[cfg(windows)]
#[doc(hidden)]
pub fn native_tooltip_registry_count_for_test() -> usize {
    NATIVE_TOOLTIPS
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .len()
}

#[cfg(windows)]
fn tooltip_bubble_size(tooltip: HWND, info: &TTTOOLINFOW) -> anyhow::Result<(i32, i32)> {
    let packed = unsafe {
        SendMessageW(
            tooltip,
            TTM_GETBUBBLESIZE,
            None,
            Some(LPARAM((info as *const TTTOOLINFOW) as isize)),
        )
    }
    .0 as u32;
    let size = ((packed & 0xffff) as i32, ((packed >> 16) & 0xffff) as i32);
    if size.0 <= 0 || size.1 <= 0 {
        Err(anyhow::anyhow!(
            "native taskbar tooltip returned an invalid bubble size"
        ))
    } else {
        Ok(size)
    }
}

#[cfg(windows)]
pub fn show_window_tooltip(parent: HWND, visible: bool) -> anyhow::Result<()> {
    let key = parent.0 as isize;
    let current = NATIVE_TOOLTIPS
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .get(&key)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("native taskbar tooltip is not registered"))?;
    let tooltip = HWND(current.hwnd as *mut core::ffi::c_void);
    if current.owner_thread_id != unsafe { GetCurrentThreadId() } {
        return Err(anyhow::anyhow!(
            "native tooltip must be shown on its owner thread"
        ));
    }
    if !unsafe { IsWindow(Some(tooltip)).as_bool() } {
        NATIVE_TOOLTIPS
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .remove(&key);
        return Err(anyhow::anyhow!(
            "native taskbar tooltip window is unavailable"
        ));
    }

    let info = tracking_tool_info(tooltip, &current.text);
    if visible {
        let mut rect = RECT::default();
        unsafe { GetWindowRect(parent, &mut rect)? };
        let bar = rect_to_dock(rect)
            .ok_or_else(|| anyhow::anyhow!("invalid taskbar tooltip owner rectangle"))?;
        let monitor = unsafe { MonitorFromWindow(parent, MONITOR_DEFAULTTONEAREST) };
        let mut monitor_info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let work_area = if !monitor.0.is_null()
            && unsafe { GetMonitorInfoW(monitor, &mut monitor_info).as_bool() }
        {
            rect_to_dock(monitor_info.rcWork).unwrap_or(bar)
        } else {
            bar
        };
        let bubble_size = tooltip_bubble_size(tooltip, &info)?;
        let (x, y) = taskbar_tooltip_anchor(bar, work_area, bubble_size);
        let packed_position = ((y as i16 as u16 as u32) << 16) | x as i16 as u16 as u32;
        unsafe {
            SendMessageW(
                tooltip,
                TTM_TRACKPOSITION,
                None,
                Some(LPARAM(packed_position as isize)),
            );
        }
    }
    unsafe {
        SendMessageW(
            tooltip,
            TTM_TRACKACTIVATE,
            Some(windows::Win32::Foundation::WPARAM(visible as usize)),
            Some(LPARAM((&info as *const TTTOOLINFOW) as isize)),
        );
        let _ = ShowWindow(tooltip, if visible { SW_SHOWNOACTIVATE } else { SW_HIDE });
    }
    Ok(())
}

pub fn rect_covers_monitor(window: DockRect, monitor: DockRect) -> bool {
    const TOLERANCE_PX: i32 = 1;

    if window.width <= 0 || window.height <= 0 || monitor.width <= 0 || monitor.height <= 0 {
        return false;
    }

    let Some(window_right) = window.x.checked_add(window.width) else {
        return false;
    };
    let Some(window_bottom) = window.y.checked_add(window.height) else {
        return false;
    };
    let Some(monitor_right) = monitor.x.checked_add(monitor.width) else {
        return false;
    };
    let Some(monitor_bottom) = monitor.y.checked_add(monitor.height) else {
        return false;
    };

    window.x <= monitor.x + TOLERANCE_PX
        && window.y <= monitor.y + TOLERANCE_PX
        && window_right >= monitor_right - TOLERANCE_PX
        && window_bottom >= monitor_bottom - TOLERANCE_PX
}

pub fn rect_covers_work_area_without_covering_monitor(
    window: DockRect,
    monitor: DockRect,
    work_area: DockRect,
) -> bool {
    rect_covers_monitor(window, work_area) && !rect_covers_monitor(window, monitor)
}

pub fn window_coverage_is_ignored(class_name: &str, title: &str) -> bool {
    matches!(
        class_name,
        "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd"
    ) || (class_name == "CEF-OSC-WIDGET" && title == "NVIDIA GeForce Overlay")
}

pub fn visible_window_coverage(
    candidates: &[WindowCoverageCandidate],
    excluded: &[isize],
) -> (bool, bool) {
    visible_window_coverage_with_filter(candidates, excluded, |_| true)
}

pub fn visible_window_coverage_on_monitor(
    candidates: &[WindowCoverageCandidate],
    excluded: &[isize],
    target_monitor: DockRect,
) -> (bool, bool) {
    visible_window_coverage_with_filter(candidates, excluded, |candidate| {
        candidate.monitor == target_monitor
    })
}

fn visible_window_coverage_with_filter(
    candidates: &[WindowCoverageCandidate],
    excluded: &[isize],
    matches_target: impl Fn(&WindowCoverageCandidate) -> bool,
) -> (bool, bool) {
    let mut fullscreen = false;
    let mut maximized_like = false;

    for candidate in candidates {
        if excluded.contains(&candidate.hwnd)
            || !candidate.visible
            || candidate.minimized
            || candidate.cloaked
            || !matches_target(candidate)
        {
            continue;
        }

        fullscreen |= rect_covers_monitor(candidate.rect, candidate.monitor);
        maximized_like |= rect_covers_work_area_without_covering_monitor(
            candidate.rect,
            candidate.monitor,
            candidate.work_area,
        );
    }

    (fullscreen, maximized_like)
}

#[cfg(windows)]
fn rect_to_dock(rect: RECT) -> Option<DockRect> {
    Some(DockRect {
        x: rect.left,
        y: rect.top,
        width: rect.right.checked_sub(rect.left)?,
        height: rect.bottom.checked_sub(rect.top)?,
    })
}

#[cfg(windows)]
fn hwnd_id(hwnd: HWND) -> isize {
    hwnd.0 as isize
}

#[cfg(windows)]
fn window_process_id(hwnd: HWND) -> Option<u32> {
    unsafe {
        let mut process_id = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut process_id));
        (process_id != 0).then_some(process_id)
    }
}

#[cfg(windows)]
fn window_class_name(hwnd: HWND) -> String {
    let mut buffer = [0u16; 128];
    let len = unsafe { GetClassNameW(hwnd, &mut buffer) };
    if len <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buffer[..len as usize])
}

#[cfg(windows)]
fn window_title(hwnd: HWND) -> String {
    let mut buffer = [0u16; 512];
    let len = unsafe { GetWindowTextW(hwnd, &mut buffer) };
    if len <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buffer[..len as usize])
}

#[cfg(windows)]
fn is_ignored_coverage_window(hwnd: HWND) -> bool {
    let class_name = window_class_name(hwnd);
    let title = window_title(hwnd);
    window_coverage_is_ignored(&class_name, &title)
}

#[cfg(windows)]
fn window_is_cloaked(hwnd: HWND) -> bool {
    let mut cloaked = 0u32;
    unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            &mut cloaked as *mut _ as *mut std::ffi::c_void,
            std::mem::size_of::<u32>() as u32,
        )
        .is_ok()
            && cloaked != 0
    }
}

#[cfg(windows)]
fn coverage_candidate_for_window(hwnd: HWND) -> Option<WindowCoverageCandidate> {
    unsafe {
        if hwnd.0.is_null() {
            return None;
        }
        if !IsWindowVisible(hwnd).as_bool() {
            return None;
        }
        if IsIconic(hwnd).as_bool() {
            return None;
        }
        if window_is_cloaked(hwnd) {
            return None;
        }
        if window_process_id(hwnd) == Some(std::process::id()) || is_ignored_coverage_window(hwnd) {
            return None;
        }

        let mut window_rect = RECT::default();
        if GetWindowRect(hwnd, &mut window_rect).is_err() {
            return None;
        }

        let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        if monitor.0.is_null() {
            return None;
        }
        let mut monitor_info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if !GetMonitorInfoW(monitor, &mut monitor_info).as_bool() {
            return None;
        }

        Some(WindowCoverageCandidate {
            hwnd: hwnd_id(hwnd),
            visible: true,
            minimized: false,
            cloaked: false,
            rect: rect_to_dock(window_rect)?,
            monitor: rect_to_dock(monitor_info.rcMonitor)?,
            work_area: rect_to_dock(monitor_info.rcWork)?,
        })
    }
}

#[cfg(windows)]
unsafe extern "system" fn collect_window_coverage_candidate(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let candidates = &mut *(lparam.0 as *mut Vec<WindowCoverageCandidate>);
    if let Some(candidate) = coverage_candidate_for_window(hwnd) {
        candidates.push(candidate);
    }
    BOOL(1)
}

#[cfg(windows)]
pub fn visible_windows_coverage(excluded: &[HWND]) -> (bool, bool) {
    let mut candidates = Vec::new();
    unsafe {
        let _ = EnumWindows(
            Some(collect_window_coverage_candidate),
            LPARAM(&mut candidates as *mut _ as isize),
        );
    }
    let excluded_ids = excluded
        .iter()
        .map(|hwnd| hwnd_id(*hwnd))
        .collect::<Vec<_>>();
    visible_window_coverage(&candidates, &excluded_ids)
}

#[cfg(windows)]
pub fn visible_windows_coverage_on_monitor(
    excluded: &[HWND],
    target_monitor: DockRect,
) -> (bool, bool) {
    let mut candidates = Vec::new();
    unsafe {
        let _ = EnumWindows(
            Some(collect_window_coverage_candidate),
            LPARAM(&mut candidates as *mut _ as isize),
        );
    }
    let excluded_ids = excluded
        .iter()
        .map(|hwnd| hwnd_id(*hwnd))
        .collect::<Vec<_>>();
    visible_window_coverage_on_monitor(&candidates, &excluded_ids, target_monitor)
}

pub fn dock_rect_for_taskbar(
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
    desired_width: i32,
) -> Option<DockRect> {
    dock_rect_for_taskbar_at_offset(left, top, right, bottom, desired_width, 0.5)
}

pub fn dock_rect_for_taskbar_at_offset(
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
    desired_width: i32,
    offset_ratio: f32,
) -> Option<DockRect> {
    let taskbar_width = right.checked_sub(left)?;
    let taskbar_height = bottom.checked_sub(top)?;
    if taskbar_width <= 0 || taskbar_height <= 0 {
        return None;
    }

    let desired_length = desired_width.max(1);
    let offset_ratio = if offset_ratio.is_finite() {
        offset_ratio.clamp(0.0, 1.0)
    } else {
        0.5
    };

    if is_horizontal_taskbar(taskbar_width, taskbar_height) {
        let width = desired_length.min(taskbar_width);
        let max_offset = taskbar_width - width;
        let x = left + ((max_offset as f32) * offset_ratio).round() as i32;

        Some(DockRect {
            x,
            y: top,
            width,
            height: taskbar_height,
        })
    } else {
        let height = desired_length.min(taskbar_height);
        let max_offset = taskbar_height - height;
        let y = top + ((max_offset as f32) * offset_ratio).round() as i32;

        Some(DockRect {
            x: left,
            y,
            width: taskbar_width,
            height,
        })
    }
}

pub fn dock_rect_for_taskbar_drag(
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
    desired_width: i32,
    pointer_screen_x: i32,
    grab_offset_x: i32,
) -> Option<(DockRect, f32)> {
    dock_rect_for_taskbar_drag_at_point(
        DockRect {
            x: left,
            y: top,
            width: right.checked_sub(left)?,
            height: bottom.checked_sub(top)?,
        },
        desired_width,
        (pointer_screen_x, top),
        (grab_offset_x, 0),
    )
}

pub fn dock_rect_for_taskbar_drag_at_point(
    taskbar: DockRect,
    desired_width: i32,
    pointer: (i32, i32),
    grab_offset: (i32, i32),
) -> Option<(DockRect, f32)> {
    let taskbar_width = taskbar.width;
    let taskbar_height = taskbar.height;
    if taskbar_width <= 0 || taskbar_height <= 0 {
        return None;
    }

    let desired_length = desired_width.max(1);
    if is_horizontal_taskbar(taskbar_width, taskbar_height) {
        let width = desired_length.min(taskbar_width);
        let max_offset = taskbar_width - width;
        let desired_offset = pointer
            .0
            .checked_sub(grab_offset.0)?
            .checked_sub(taskbar.x)?
            .clamp(0, max_offset);
        let ratio = ratio_for_offset(desired_offset, max_offset);

        return Some((
            DockRect {
                x: taskbar.x + desired_offset,
                y: taskbar.y,
                width,
                height: taskbar_height,
            },
            ratio,
        ));
    }

    let height = desired_length.min(taskbar_height);
    let max_offset = taskbar_height - height;
    let desired_offset = pointer
        .1
        .checked_sub(grab_offset.1)?
        .checked_sub(taskbar.y)?
        .clamp(0, max_offset);
    let ratio = ratio_for_offset(desired_offset, max_offset);

    Some((
        DockRect {
            x: taskbar.x,
            y: taskbar.y + desired_offset,
            width: taskbar_width,
            height,
        },
        ratio,
    ))
}

fn ratio_for_offset(offset: i32, max_offset: i32) -> f32 {
    if max_offset == 0 {
        0.0
    } else {
        offset as f32 / max_offset as f32
    }
}

pub fn offset_ratio_for_taskbar_left(
    taskbar_left: i32,
    taskbar_right: i32,
    desired_width: i32,
    window_left: i32,
) -> Option<f32> {
    let taskbar_width = taskbar_right.checked_sub(taskbar_left)?;
    if taskbar_width <= 0 {
        return None;
    }

    let width = desired_width.max(1).min(taskbar_width);
    let max_offset = taskbar_width - width;
    let offset = window_left.checked_sub(taskbar_left)?.clamp(0, max_offset);
    Some(if max_offset == 0 {
        0.0
    } else {
        offset as f32 / max_offset as f32
    })
}

pub fn offset_ratio_for_taskbar_rect(taskbar: DockRect, window: DockRect) -> Option<f32> {
    let taskbar_width = taskbar.width;
    let taskbar_height = taskbar.height;
    if taskbar_width <= 0 || taskbar_height <= 0 {
        return None;
    }

    if is_horizontal_taskbar(taskbar_width, taskbar_height) {
        offset_ratio_for_axis(
            taskbar.x,
            taskbar.x.checked_add(taskbar.width)?,
            window.width,
            window.x,
        )
    } else {
        offset_ratio_for_axis(
            taskbar.y,
            taskbar.y.checked_add(taskbar.height)?,
            window.height,
            window.y,
        )
    }
}

fn offset_ratio_for_axis(
    taskbar_start: i32,
    taskbar_end: i32,
    desired_length: i32,
    window_start: i32,
) -> Option<f32> {
    let taskbar_length = taskbar_end.checked_sub(taskbar_start)?;
    if taskbar_length <= 0 {
        return None;
    }

    let length = desired_length.max(1).min(taskbar_length);
    let max_offset = taskbar_length - length;
    let offset = window_start
        .checked_sub(taskbar_start)?
        .clamp(0, max_offset);
    Some(ratio_for_offset(offset, max_offset))
}

pub fn taskbar_monitor_key(monitor: DockRect) -> String {
    format!(
        "monitor:{},{},{},{}",
        monitor.x, monitor.y, monitor.width, monitor.height
    )
}

pub fn dock_rect_for_taskbar_target(
    target: &TaskbarTarget,
    desired_width: i32,
    offset_ratio: f32,
) -> Option<DockRect> {
    dock_rect_for_taskbar_at_offset(
        target.rect.x,
        target.rect.y,
        target.rect.x.checked_add(target.rect.width)?,
        target.rect.y.checked_add(target.rect.height)?,
        desired_width,
        offset_ratio,
    )
}

pub fn taskbar_target_by_key_or_primary<'a>(
    targets: &'a [TaskbarTarget],
    preferred_key: &str,
) -> Option<&'a TaskbarTarget> {
    targets
        .iter()
        .find(|target| !preferred_key.is_empty() && target.key == preferred_key)
        .or_else(|| targets.iter().find(|target| target.primary))
        .or_else(|| targets.first())
}

pub fn taskbar_target_for_point_or_key<'a>(
    targets: &'a [TaskbarTarget],
    point: (i32, i32),
    preferred_key: &str,
) -> Option<&'a TaskbarTarget> {
    targets
        .iter()
        .find(|target| point_inside_dock_rect(target.monitor, point))
        .or_else(|| {
            targets
                .iter()
                .find(|target| point_inside_dock_rect(target.rect, point))
        })
        .or_else(|| taskbar_target_by_key_or_primary(targets, preferred_key))
}

fn point_inside_dock_rect(rect: DockRect, point: (i32, i32)) -> bool {
    point.0 >= rect.x
        && point.0 < rect.x.saturating_add(rect.width)
        && point.1 >= rect.y
        && point.1 < rect.y.saturating_add(rect.height)
}

fn is_horizontal_taskbar(taskbar_width: i32, taskbar_height: i32) -> bool {
    taskbar_width >= taskbar_height
}

#[cfg(windows)]
pub fn shell_taskbar_window() -> anyhow::Result<ShellTaskbarWindow> {
    unsafe {
        let hwnd = FindWindowW(w!("Shell_TrayWnd"), None)?;
        shell_taskbar_window_from_hwnd(hwnd, true)
    }
}

#[cfg(windows)]
fn shell_taskbar_window_from_hwnd(hwnd: HWND, primary: bool) -> anyhow::Result<ShellTaskbarWindow> {
    unsafe {
        let mut rect = RECT::default();
        GetWindowRect(hwnd, &mut rect)?;

        let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        if monitor.0.is_null() {
            return Err(anyhow::anyhow!("no monitor for Shell_TrayWnd"));
        }
        let mut monitor_info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if !GetMonitorInfoW(monitor, &mut monitor_info).as_bool() {
            return Err(anyhow::anyhow!("failed to read Shell_TrayWnd monitor"));
        }

        let monitor = rect_to_dock(monitor_info.rcMonitor)
            .ok_or_else(|| anyhow::anyhow!("invalid Shell_TrayWnd monitor rectangle"))?;
        Ok(ShellTaskbarWindow {
            hwnd,
            left: rect.left,
            top: rect.top,
            right: rect.right,
            bottom: rect.bottom,
            monitor,
            key: taskbar_monitor_key(monitor),
            primary,
        })
    }
}

#[cfg(windows)]
unsafe extern "system" fn collect_shell_taskbar_window(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let class_name = window_class_name(hwnd);
    if class_name != "Shell_TrayWnd" && class_name != "Shell_SecondaryTrayWnd" {
        return BOOL(1);
    }

    let taskbars = &mut *(lparam.0 as *mut Vec<ShellTaskbarWindow>);
    if taskbars.iter().any(|taskbar| taskbar.hwnd == hwnd) {
        return BOOL(1);
    }

    if let Ok(taskbar) = shell_taskbar_window_from_hwnd(hwnd, class_name == "Shell_TrayWnd") {
        taskbars.push(taskbar);
    }
    BOOL(1)
}

#[cfg(windows)]
pub fn shell_taskbar_windows() -> anyhow::Result<Vec<ShellTaskbarWindow>> {
    let mut taskbars = Vec::new();
    unsafe {
        let _ = EnumWindows(
            Some(collect_shell_taskbar_window),
            LPARAM(&mut taskbars as *mut _ as isize),
        );
    }

    if taskbars.is_empty() {
        taskbars.push(shell_taskbar_window()?);
    }
    taskbars.sort_by_key(|taskbar| (!taskbar.primary, taskbar.left, taskbar.top));
    Ok(taskbars)
}

#[cfg(windows)]
pub fn shell_taskbar_window_for_key(preferred_key: &str) -> anyhow::Result<ShellTaskbarWindow> {
    let taskbars = shell_taskbar_windows()?;
    taskbars
        .iter()
        .find(|taskbar| !preferred_key.is_empty() && taskbar.key == preferred_key)
        .or_else(|| taskbars.iter().find(|taskbar| taskbar.primary))
        .or_else(|| taskbars.first())
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("no shell taskbar windows found"))
}

#[cfg(windows)]
pub fn shell_taskbar_monitor_rect() -> anyhow::Result<DockRect> {
    Ok(shell_taskbar_window()?.monitor)
}

#[cfg(windows)]
fn shell_taskbar_bounds() -> anyhow::Result<(i32, i32, i32, i32)> {
    let taskbar = shell_taskbar_window()?;
    Ok((taskbar.left, taskbar.top, taskbar.right, taskbar.bottom))
}

#[cfg(windows)]
pub fn shell_taskbar_dock_rect(desired_width: i32, offset_ratio: f32) -> anyhow::Result<DockRect> {
    let (left, top, right, bottom) = shell_taskbar_bounds()?;
    dock_rect_for_taskbar_at_offset(left, top, right, bottom, desired_width, offset_ratio)
        .ok_or_else(|| anyhow::anyhow!("invalid Shell_TrayWnd rectangle"))
}

#[cfg(windows)]
pub fn shell_taskbar_drag_rect(
    desired_width: i32,
    pointer_screen_x: i32,
    grab_offset_x: i32,
) -> anyhow::Result<(DockRect, f32)> {
    let (left, top, right, bottom) = shell_taskbar_bounds()?;
    dock_rect_for_taskbar_drag(
        left,
        top,
        right,
        bottom,
        desired_width,
        pointer_screen_x,
        grab_offset_x,
    )
    .ok_or_else(|| anyhow::anyhow!("invalid Shell_TrayWnd rectangle"))
}

#[cfg(windows)]
pub fn shell_taskbar_drag_rect_at_point(
    desired_width: i32,
    pointer_screen_x: i32,
    pointer_screen_y: i32,
    grab_offset_x: i32,
    grab_offset_y: i32,
) -> anyhow::Result<(DockRect, f32)> {
    let (left, top, right, bottom) = shell_taskbar_bounds()?;
    dock_rect_for_taskbar_drag_at_point(
        DockRect {
            x: left,
            y: top,
            width: right
                .checked_sub(left)
                .ok_or_else(|| anyhow::anyhow!("invalid Shell_TrayWnd rectangle"))?,
            height: bottom
                .checked_sub(top)
                .ok_or_else(|| anyhow::anyhow!("invalid Shell_TrayWnd rectangle"))?,
        },
        desired_width,
        (pointer_screen_x, pointer_screen_y),
        (grab_offset_x, grab_offset_y),
    )
    .ok_or_else(|| anyhow::anyhow!("invalid Shell_TrayWnd rectangle"))
}

#[cfg(windows)]
pub fn shell_taskbar_drag_rect_at_point_for_key(
    logical_length: i32,
    pointer_screen_x: i32,
    pointer_screen_y: i32,
    grab_axis_ratio: f32,
    grab_cross_ratio: f32,
    preferred_key: &str,
) -> anyhow::Result<(ShellTaskbarWindow, DockRect, f32)> {
    let taskbars = shell_taskbar_windows()?;
    let targets = taskbars
        .iter()
        .filter_map(|taskbar| {
            Some(TaskbarTarget {
                key: taskbar.key.clone(),
                rect: DockRect {
                    x: taskbar.left,
                    y: taskbar.top,
                    width: taskbar.right.checked_sub(taskbar.left)?,
                    height: taskbar.bottom.checked_sub(taskbar.top)?,
                },
                monitor: taskbar.monitor,
                primary: taskbar.primary,
            })
        })
        .collect::<Vec<_>>();
    let target = taskbar_target_for_point_or_key(
        &targets,
        (pointer_screen_x, pointer_screen_y),
        preferred_key,
    )
    .ok_or_else(|| anyhow::anyhow!("no shell taskbar target found"))?;
    let taskbar = taskbars
        .iter()
        .find(|taskbar| taskbar.key == target.key)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("selected shell taskbar vanished"))?;
    let rect = DockRect {
        x: taskbar.left,
        y: taskbar.top,
        width: taskbar
            .right
            .checked_sub(taskbar.left)
            .ok_or_else(|| anyhow::anyhow!("invalid Shell taskbar rectangle"))?,
        height: taskbar
            .bottom
            .checked_sub(taskbar.top)
            .ok_or_else(|| anyhow::anyhow!("invalid Shell taskbar rectangle"))?,
    };
    let dpi = unsafe { windows::Win32::UI::HiDpi::GetDpiForWindow(taskbar.hwnd) };
    let (dock, ratio) = drag_rect_for_logical_length_at_dpi(
        rect,
        logical_length,
        dpi,
        (pointer_screen_x, pointer_screen_y),
        grab_axis_ratio,
        grab_cross_ratio,
    )
    .ok_or_else(|| anyhow::anyhow!("invalid Shell taskbar rectangle"))?;
    Ok((taskbar, dock, ratio))
}

pub fn drag_rect_for_logical_length_at_dpi(
    taskbar: DockRect,
    logical_length: i32,
    dpi: u32,
    pointer: (i32, i32),
    grab_axis_ratio: f32,
    grab_cross_ratio: f32,
) -> Option<(DockRect, f32)> {
    let dpi = if dpi == 0 { 96 } else { dpi };
    let physical_length =
        ((logical_length.max(1) as i64 * dpi as i64 + 48) / 96).clamp(1, i32::MAX as i64) as i32;
    let axis_ratio = grab_axis_ratio.clamp(0.0, 1.0);
    let cross_ratio = grab_cross_ratio.clamp(0.0, 1.0);
    let grab_offset = if is_horizontal_taskbar(taskbar.width, taskbar.height) {
        (
            (physical_length as f32 * axis_ratio).round() as i32,
            (taskbar.height as f32 * cross_ratio).round() as i32,
        )
    } else {
        (
            (taskbar.width as f32 * cross_ratio).round() as i32,
            (physical_length as f32 * axis_ratio).round() as i32,
        )
    };
    dock_rect_for_taskbar_drag_at_point(taskbar, physical_length, pointer, grab_offset)
}

#[cfg(all(test, windows))]
mod tooltip_tests {
    use super::*;

    #[test]
    fn tracking_tool_is_owned_by_the_tooltip_thread_not_the_bar_window() {
        let tooltip = HWND(0x2222usize as *mut core::ffi::c_void);
        let text = "Juice tooltip\0".encode_utf16().collect::<Vec<_>>();
        let info = tracking_tool_info(tooltip, &text);

        assert_eq!(info.hwnd, tooltip);
        assert_eq!(info.uId, 1);
        assert_eq!(info.uFlags, TTF_TRACK | TTF_ABSOLUTE);
    }

    #[test]
    fn tooltip_registry_removal_preserves_the_owner_thread_contract() {
        let parent = 0x1234_5678isize;
        let tooltip = 0x1234_5679isize;
        let baseline = native_tooltip_registry_count_for_test();
        NATIVE_TOOLTIPS
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .insert(
                parent,
                NativeTooltip {
                    hwnd: tooltip,
                    owner_thread_id: unsafe { GetCurrentThreadId() },
                    text: Arc::new(vec![0]),
                },
            );
        assert_eq!(native_tooltip_registry_count_for_test(), baseline + 1);

        let error = std::thread::spawn(move || {
            remove_window_tooltip(HWND(parent as *mut core::ffi::c_void)).unwrap_err()
        })
        .join()
        .unwrap();
        assert!(error.to_string().contains("owner thread"));
        assert_eq!(native_tooltip_registry_count_for_test(), baseline + 1);

        assert!(remove_window_tooltip(HWND(parent as *mut core::ffi::c_void)).unwrap());
        assert_eq!(native_tooltip_registry_count_for_test(), baseline);
    }
}
