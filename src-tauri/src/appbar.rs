use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::UI::Shell::{
    SHAppBarMessage, ABE_BOTTOM, ABM_NEW, ABM_QUERYPOS, ABM_REMOVE, ABM_SETPOS, APPBARDATA,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppBarError {
    NewFailed,
    QueryPositionFailed,
    SetPositionFailed,
}

pub struct AppBarReservation {
    pub rect: RECT,
    pub guard: AppBarGuard,
}

pub struct AppBarGuard {
    hwnd: isize,
    active: bool,
}

impl AppBarGuard {
    fn new(hwnd: HWND) -> Self {
        Self {
            hwnd: hwnd.0 as isize,
            active: true,
        }
    }

    fn hwnd(&self) -> HWND {
        HWND(self.hwnd as *mut core::ffi::c_void)
    }

    pub fn release(&mut self) {
        if !self.active {
            return;
        }

        unsafe {
            let mut data = APPBARDATA {
                cbSize: std::mem::size_of::<APPBARDATA>() as u32,
                hWnd: self.hwnd(),
                ..Default::default()
            };
            let _ = SHAppBarMessage(ABM_REMOVE, &mut data);
        }
        self.active = false;
    }
}

impl Drop for AppBarGuard {
    fn drop(&mut self) {
        self.release();
    }
}

pub fn physical_bar_height(logical_px: u32, scale_factor: f64) -> i32 {
    let scale = if scale_factor.is_finite() && scale_factor > 0.0 {
        scale_factor
    } else {
        1.0
    };
    ((logical_px as f64) * scale).round().max(1.0) as i32
}

pub fn reserve_bottom(
    hwnd: HWND,
    height: i32,
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
) -> Result<AppBarReservation, AppBarError> {
    unsafe {
        let mut data = APPBARDATA {
            cbSize: std::mem::size_of::<APPBARDATA>() as u32,
            hWnd: hwnd,
            uEdge: ABE_BOTTOM,
            ..Default::default()
        };

        if SHAppBarMessage(ABM_NEW, &mut data) == 0 {
            return Err(AppBarError::NewFailed);
        }
        let guard = AppBarGuard::new(hwnd);

        data.rc = RECT {
            left,
            top: (bottom - height).max(top),
            right,
            bottom,
        };
        if SHAppBarMessage(ABM_QUERYPOS, &mut data) == 0 {
            return Err(AppBarError::QueryPositionFailed);
        }
        data.rc.top = data.rc.bottom - height;

        if SHAppBarMessage(ABM_SETPOS, &mut data) == 0 {
            return Err(AppBarError::SetPositionFailed);
        }

        Ok(AppBarReservation {
            rect: data.rc,
            guard,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn physical_bar_height_scales_logical_pixels() {
        assert_eq!(physical_bar_height(40, 1.5), 60);
        assert_eq!(physical_bar_height(40, 0.0), 40);
    }

    #[test]
    fn error_surface_includes_query_failure() {
        assert_eq!(
            format!("{:?}", AppBarError::QueryPositionFailed),
            "QueryPositionFailed"
        );
    }
}
