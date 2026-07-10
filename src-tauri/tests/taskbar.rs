use agent_juice::taskbar::{
    dock_rect_for_taskbar, dock_rect_for_taskbar_at_offset, dock_rect_for_taskbar_drag_at_point,
    dock_rect_for_taskbar_target, offset_ratio_for_taskbar_left, offset_ratio_for_taskbar_rect,
    rect_covers_monitor, rect_covers_work_area_without_covering_monitor, taskbar_monitor_key,
    taskbar_target_by_key_or_primary, taskbar_target_for_point_or_key,
    visible_window_coverage_on_monitor, window_coverage_is_ignored, DockRect, TaskbarTarget,
    WindowCoverageCandidate,
};

#[test]
fn dock_rect_sits_inside_horizontal_taskbar_without_reserving_screen_space() {
    let dock = dock_rect_for_taskbar(0, 1040, 1920, 1080, 520).unwrap();

    assert_eq!(
        dock,
        DockRect {
            x: 700,
            y: 1040,
            width: 520,
            height: 40
        }
    );
}

#[test]
fn dock_rect_clamps_to_taskbar_width_for_narrow_screens() {
    let dock = dock_rect_for_taskbar(0, 728, 480, 768, 520).unwrap();

    assert_eq!(
        dock,
        DockRect {
            x: 0,
            y: 728,
            width: 480,
            height: 40
        }
    );
}

#[test]
fn dock_rect_rejects_invalid_taskbar_rectangles() {
    assert!(dock_rect_for_taskbar(100, 100, 100, 140, 520).is_none());
    assert!(dock_rect_for_taskbar(0, 100, 500, 100, 520).is_none());
}

#[test]
fn dock_rect_uses_offset_ratio_and_clamps_inside_taskbar() {
    let left = dock_rect_for_taskbar_at_offset(0, 1040, 1920, 1080, 520, -0.2).unwrap();
    assert_eq!(left.x, 0);

    let middle = dock_rect_for_taskbar_at_offset(0, 1040, 1920, 1080, 520, 0.5).unwrap();
    assert_eq!(middle.x, 700);

    let right = dock_rect_for_taskbar_at_offset(0, 1040, 1920, 1080, 520, 1.2).unwrap();
    assert_eq!(right.x, 1400);
}

#[test]
fn dock_rect_supports_top_bottom_left_and_right_taskbars() {
    let bottom = dock_rect_for_taskbar_at_offset(0, 1040, 1920, 1080, 520, 0.5).unwrap();
    assert_eq!(
        bottom,
        DockRect {
            x: 700,
            y: 1040,
            width: 520,
            height: 40
        }
    );

    let top = dock_rect_for_taskbar_at_offset(0, 0, 1920, 40, 520, 0.5).unwrap();
    assert_eq!(
        top,
        DockRect {
            x: 700,
            y: 0,
            width: 520,
            height: 40
        }
    );

    let left = dock_rect_for_taskbar_at_offset(0, 0, 48, 1080, 260, 0.5).unwrap();
    assert_eq!(
        left,
        DockRect {
            x: 0,
            y: 410,
            width: 48,
            height: 260
        }
    );

    let right = dock_rect_for_taskbar_at_offset(1872, 0, 1920, 1080, 260, 0.5).unwrap();
    assert_eq!(
        right,
        DockRect {
            x: 1872,
            y: 410,
            width: 48,
            height: 260
        }
    );
}

#[test]
fn drag_rect_uses_vertical_axis_for_side_taskbars() {
    let (dock, ratio) = dock_rect_for_taskbar_drag_at_point(
        DockRect {
            x: 0,
            y: 0,
            width: 48,
            height: 1080,
        },
        260,
        (24, 800),
        (12, 20),
    )
    .unwrap();

    assert_eq!(
        dock,
        DockRect {
            x: 0,
            y: 780,
            width: 48,
            height: 260
        }
    );
    assert!((ratio - (780.0 / 820.0)).abs() < 0.001);
}

#[test]
fn offset_ratio_for_taskbar_left_matches_dock_offsets() {
    assert_eq!(
        offset_ratio_for_taskbar_left(0, 1920, 520, -10).unwrap(),
        0.0
    );
    assert!((offset_ratio_for_taskbar_left(0, 1920, 520, 700).unwrap() - 0.5).abs() < 0.001);
    assert_eq!(
        offset_ratio_for_taskbar_left(0, 1920, 520, 2000).unwrap(),
        1.0
    );
}

#[test]
fn offset_ratio_for_taskbar_rect_uses_matching_orientation_axis() {
    assert!(
        (offset_ratio_for_taskbar_rect(
            DockRect {
                x: 0,
                y: 1040,
                width: 1920,
                height: 40,
            },
            DockRect {
                x: 700,
                y: 1040,
                width: 520,
                height: 40,
            },
        )
        .unwrap()
            - 0.5)
            .abs()
            < 0.001
    );
    assert!(
        (offset_ratio_for_taskbar_rect(
            DockRect {
                x: 0,
                y: 0,
                width: 48,
                height: 1080,
            },
            DockRect {
                x: 0,
                y: 410,
                width: 48,
                height: 260,
            },
        )
        .unwrap()
            - 0.5)
            .abs()
            < 0.001
    );
}

#[test]
fn fullscreen_rect_detection_requires_covering_the_whole_monitor() {
    let monitor = DockRect {
        x: 0,
        y: 0,
        width: 1920,
        height: 1080,
    };

    assert!(rect_covers_monitor(monitor, monitor));
    assert!(rect_covers_monitor(
        DockRect {
            x: -1,
            y: -1,
            width: 1922,
            height: 1082,
        },
        monitor,
    ));
    assert!(!rect_covers_monitor(
        DockRect {
            x: 0,
            y: 0,
            width: 1920,
            height: 1040,
        },
        monitor,
    ));
    assert!(!rect_covers_monitor(
        DockRect {
            x: 100,
            y: 100,
            width: 1200,
            height: 700,
        },
        monitor,
    ));
}

#[test]
fn maximized_rect_detection_covers_work_area_but_not_whole_monitor() {
    let monitor = DockRect {
        x: 0,
        y: 0,
        width: 1920,
        height: 1080,
    };
    let work_area = DockRect {
        x: 0,
        y: 0,
        width: 1920,
        height: 1032,
    };

    assert!(rect_covers_work_area_without_covering_monitor(
        work_area, monitor, work_area,
    ));
    assert!(!rect_covers_work_area_without_covering_monitor(
        monitor, monitor, work_area,
    ));
    assert!(!rect_covers_work_area_without_covering_monitor(
        DockRect {
            x: 100,
            y: 100,
            width: 1200,
            height: 700,
        },
        monitor,
        work_area,
    ));
}

#[test]
fn visible_window_coverage_scans_past_excluded_foreground_windows() {
    let monitor = DockRect {
        x: 0,
        y: 0,
        width: 1920,
        height: 1080,
    };
    let work_area = DockRect {
        x: 0,
        y: 0,
        width: 1920,
        height: 1032,
    };

    let candidates = [
        WindowCoverageCandidate {
            hwnd: 1,
            visible: true,
            minimized: false,
            cloaked: false,
            rect: DockRect {
                x: 400,
                y: 200,
                width: 600,
                height: 500,
            },
            monitor,
            work_area,
        },
        WindowCoverageCandidate {
            hwnd: 2,
            visible: true,
            minimized: false,
            cloaked: false,
            rect: monitor,
            monitor,
            work_area,
        },
    ];

    assert_eq!(
        visible_window_coverage_on_monitor(&candidates, &[1], monitor),
        (true, false)
    );
}

#[test]
fn visible_window_coverage_ignores_other_monitor_maximized_windows() {
    let primary_monitor = DockRect {
        x: 0,
        y: 0,
        width: 1920,
        height: 1080,
    };
    let primary_work_area = DockRect {
        x: 0,
        y: 0,
        width: 1920,
        height: 1032,
    };
    let secondary_monitor = DockRect {
        x: 1920,
        y: 0,
        width: 2560,
        height: 1440,
    };
    let secondary_work_area = DockRect {
        x: 1920,
        y: 0,
        width: 2560,
        height: 1392,
    };

    let candidates = [
        WindowCoverageCandidate {
            hwnd: 1,
            visible: true,
            minimized: false,
            cloaked: false,
            rect: secondary_work_area,
            monitor: secondary_monitor,
            work_area: secondary_work_area,
        },
        WindowCoverageCandidate {
            hwnd: 2,
            visible: true,
            minimized: false,
            cloaked: false,
            rect: DockRect {
                x: 300,
                y: 300,
                width: 600,
                height: 400,
            },
            monitor: primary_monitor,
            work_area: primary_work_area,
        },
    ];

    assert_eq!(
        visible_window_coverage_on_monitor(&candidates, &[], primary_monitor),
        (false, false)
    );
}

#[test]
fn visible_window_coverage_ignores_cloaked_monitor_cover_windows() {
    let primary_monitor = DockRect {
        x: 0,
        y: 0,
        width: 3440,
        height: 1440,
    };
    let primary_work_area = DockRect {
        x: 0,
        y: 0,
        width: 3440,
        height: 1392,
    };

    let candidates = [
        WindowCoverageCandidate {
            hwnd: 1,
            visible: true,
            minimized: false,
            cloaked: true,
            rect: primary_monitor,
            monitor: primary_monitor,
            work_area: primary_work_area,
        },
        WindowCoverageCandidate {
            hwnd: 2,
            visible: true,
            minimized: false,
            cloaked: false,
            rect: DockRect {
                x: 300,
                y: 300,
                width: 600,
                height: 400,
            },
            monitor: primary_monitor,
            work_area: primary_work_area,
        },
    ];

    assert_eq!(
        visible_window_coverage_on_monitor(&candidates, &[], primary_monitor),
        (false, false)
    );
}

#[test]
fn coverage_ignores_nvidia_geforce_overlay_without_ignoring_real_fullscreen_windows() {
    assert!(window_coverage_is_ignored(
        "CEF-OSC-WIDGET",
        "NVIDIA GeForce Overlay"
    ));
    assert!(!window_coverage_is_ignored(
        "BlackDesertWindowClass",
        "검은사막 - 524872"
    ));
    assert!(!window_coverage_is_ignored("Chrome_WidgetWin_1", "Codex"));
}

#[test]
fn taskbar_target_selection_prefers_pointer_monitor_then_saved_key_then_primary() {
    let primary_monitor = DockRect {
        x: 0,
        y: 0,
        width: 1920,
        height: 1080,
    };
    let secondary_monitor = DockRect {
        x: 1920,
        y: 0,
        width: 2560,
        height: 1440,
    };
    let primary = TaskbarTarget {
        key: taskbar_monitor_key(primary_monitor),
        rect: DockRect {
            x: 0,
            y: 1040,
            width: 1920,
            height: 40,
        },
        monitor: primary_monitor,
        primary: true,
    };
    let secondary = TaskbarTarget {
        key: taskbar_monitor_key(secondary_monitor),
        rect: DockRect {
            x: 1920,
            y: 1392,
            width: 2560,
            height: 48,
        },
        monitor: secondary_monitor,
        primary: false,
    };
    let targets = [primary.clone(), secondary.clone()];

    assert_eq!(
        taskbar_target_for_point_or_key(&targets, (2200, 700), &primary.key)
            .unwrap()
            .key,
        secondary.key
    );
    assert_eq!(
        taskbar_target_for_point_or_key(&targets, (-100, -100), &secondary.key)
            .unwrap()
            .key,
        secondary.key
    );
    assert_eq!(
        taskbar_target_by_key_or_primary(&targets, "missing")
            .unwrap()
            .key,
        primary.key
    );
}

#[test]
fn dock_rect_for_taskbar_target_uses_target_rect_not_primary_coordinates() {
    let secondary = TaskbarTarget {
        key: "monitor:1920,0,2560,1440".into(),
        rect: DockRect {
            x: 1920,
            y: 1392,
            width: 2560,
            height: 48,
        },
        monitor: DockRect {
            x: 1920,
            y: 0,
            width: 2560,
            height: 1440,
        },
        primary: false,
    };

    let rect = dock_rect_for_taskbar_target(&secondary, 260, 0.5).unwrap();

    assert_eq!(
        rect,
        DockRect {
            x: 3070,
            y: 1392,
            width: 260,
            height: 48,
        }
    );
}
