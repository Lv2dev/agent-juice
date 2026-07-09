#[cfg(windows)]
use windows::{
    core::{w, BOOL},
    Win32::{
        Foundation::{HWND, LPARAM, RECT},
        Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED},
        Graphics::Gdi::{
            GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
        },
        UI::WindowsAndMessaging::{
            EnumWindows, FindWindowW, GetClassNameW, GetWindowRect, GetWindowTextW,
            GetWindowThreadProcessId, IsIconic, IsWindowVisible, ShowWindow, SW_HIDE,
        },
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DockRect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
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
pub struct ShellTaskbarWindow {
    pub hwnd: HWND,
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
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
    matches!(class_name, "Progman" | "WorkerW" | "Shell_TrayWnd")
        || (class_name == "CEF-OSC-WIDGET" && title == "NVIDIA GeForce Overlay")
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

fn is_horizontal_taskbar(taskbar_width: i32, taskbar_height: i32) -> bool {
    taskbar_width >= taskbar_height
}

#[cfg(windows)]
pub fn shell_taskbar_window() -> anyhow::Result<ShellTaskbarWindow> {
    unsafe {
        let hwnd = FindWindowW(w!("Shell_TrayWnd"), None)?;
        let mut rect = RECT::default();
        GetWindowRect(hwnd, &mut rect)?;
        Ok(ShellTaskbarWindow {
            hwnd,
            left: rect.left,
            top: rect.top,
            right: rect.right,
            bottom: rect.bottom,
        })
    }
}

#[cfg(windows)]
pub fn shell_taskbar_monitor_rect() -> anyhow::Result<DockRect> {
    unsafe {
        let taskbar = shell_taskbar_window()?;
        let monitor = MonitorFromWindow(taskbar.hwnd, MONITOR_DEFAULTTONEAREST);
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

        rect_to_dock(monitor_info.rcMonitor)
            .ok_or_else(|| anyhow::anyhow!("invalid Shell_TrayWnd monitor rectangle"))
    }
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
