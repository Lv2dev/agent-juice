use agent_juice::taskbar::{
    dock_rect_for_taskbar, dock_rect_for_taskbar_at_offset, dock_rect_for_taskbar_drag_at_point,
    dock_rect_for_taskbar_target, drag_rect_for_logical_length_at_dpi,
    offset_ratio_for_taskbar_left, offset_ratio_for_taskbar_rect, rect_covers_monitor,
    rect_covers_work_area_without_covering_monitor, taskbar_monitor_device_key,
    taskbar_monitor_key, taskbar_monitor_path_key, taskbar_rect_for_monitor_work_area,
    taskbar_target_by_key_or_primary, taskbar_target_for_point_or_key, taskbar_tooltip_anchor,
    visible_window_coverage_on_monitor, window_coverage_is_ignored, DockRect, TaskbarTarget,
    WindowCoverageCandidate,
};

#[test]
fn taskbar_rect_uses_reserved_bottom_edge_on_a_mixed_resolution_monitor() {
    let monitor = DockRect {
        x: 1920,
        y: 0,
        width: 2560,
        height: 1440,
    };
    let work_area = DockRect {
        x: 1920,
        y: 0,
        width: 2560,
        height: 1392,
    };
    let transitional_window = DockRect {
        x: 1920,
        y: 1344,
        width: 2560,
        height: 48,
    };

    assert_eq!(
        taskbar_rect_for_monitor_work_area(transitional_window, monitor, work_area),
        DockRect {
            x: 1920,
            y: 1392,
            width: 2560,
            height: 48,
        }
    );
}

#[test]
fn taskbar_rect_handles_negative_coordinates_and_side_taskbars() {
    let monitor = DockRect {
        x: -1600,
        y: -200,
        width: 1600,
        height: 1200,
    };
    let work_area = DockRect {
        x: -1540,
        y: -200,
        width: 1540,
        height: 1200,
    };

    assert_eq!(
        taskbar_rect_for_monitor_work_area(
            DockRect {
                x: -1592,
                y: -200,
                width: 52,
                height: 1200,
            },
            monitor,
            work_area,
        ),
        DockRect {
            x: -1600,
            y: -200,
            width: 60,
            height: 1200,
        }
    );
}

#[test]
fn taskbar_rect_normalizes_reserved_top_and_right_edges() {
    let monitor = DockRect {
        x: 0,
        y: 0,
        width: 1920,
        height: 1080,
    };
    assert_eq!(
        taskbar_rect_for_monitor_work_area(
            DockRect {
                x: 0,
                y: 6,
                width: 1920,
                height: 40,
            },
            monitor,
            DockRect {
                x: 0,
                y: 48,
                width: 1920,
                height: 1032,
            },
        ),
        DockRect {
            x: 0,
            y: 0,
            width: 1920,
            height: 48,
        }
    );
    assert_eq!(
        taskbar_rect_for_monitor_work_area(
            DockRect {
                x: 1870,
                y: 0,
                width: 44,
                height: 1080,
            },
            monitor,
            DockRect {
                x: 0,
                y: 0,
                width: 1860,
                height: 1080,
            },
        ),
        DockRect {
            x: 1860,
            y: 0,
            width: 60,
            height: 1080,
        }
    );
}

#[test]
fn taskbar_rect_preserves_window_bounds_when_auto_hide_reserves_no_edge() {
    let monitor = DockRect {
        x: 0,
        y: 0,
        width: 1920,
        height: 1080,
    };
    let window = DockRect {
        x: 0,
        y: 1078,
        width: 1920,
        height: 2,
    };

    assert_eq!(
        taskbar_rect_for_monitor_work_area(window, monitor, monitor),
        window
    );
}

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
fn tooltip_anchor_stays_inside_the_desktop_for_each_taskbar_edge() {
    let work = DockRect {
        x: 48,
        y: 48,
        width: 1872,
        height: 1032,
    };
    let top = DockRect {
        x: 200,
        y: 0,
        width: 260,
        height: 48,
    };
    let bottom = DockRect {
        x: 200,
        y: 1080,
        width: 260,
        height: 48,
    };
    let left = DockRect {
        x: 0,
        y: 200,
        width: 48,
        height: 260,
    };
    let right = DockRect {
        x: 1920,
        y: 200,
        width: 48,
        height: 260,
    };

    let bubble_size = (212, 68);
    assert_eq!(taskbar_tooltip_anchor(top, work, bubble_size), (208, 56));
    assert_eq!(
        taskbar_tooltip_anchor(bottom, work, bubble_size),
        (208, 1004)
    );
    assert_eq!(taskbar_tooltip_anchor(left, work, bubble_size), (56, 208));
    assert_eq!(
        taskbar_tooltip_anchor(right, work, bubble_size),
        (1700, 208)
    );
}

#[test]
fn tooltip_anchor_clamps_both_axes_at_work_area_trailing_edges() {
    let work = DockRect {
        x: 48,
        y: 48,
        width: 1872,
        height: 1032,
    };
    let bottom_right = DockRect {
        x: 1880,
        y: 1080,
        width: 40,
        height: 48,
    };
    let right_bottom = DockRect {
        x: 1920,
        y: 1060,
        width: 48,
        height: 20,
    };

    assert_eq!(
        taskbar_tooltip_anchor(bottom_right, work, (212, 68)),
        (1708, 1004)
    );
    assert_eq!(
        taskbar_tooltip_anchor(right_bottom, work, (212, 68)),
        (1700, 1012)
    );
}

#[test]
fn tooltip_anchor_clamps_mixed_dpi_physical_size_on_negative_monitor() {
    let work = DockRect {
        x: -2560,
        y: 48,
        width: 2560,
        height: 1392,
    };
    let top_trailing = DockRect {
        x: -180,
        y: 0,
        width: 180,
        height: 48,
    };

    assert_eq!(
        taskbar_tooltip_anchor(top_trailing, work, (420, 120)),
        (-420, 56)
    );
}

#[test]
fn tooltip_anchor_pins_oversized_bubble_to_work_area_origin() {
    let work = DockRect {
        x: 100,
        y: 200,
        width: 300,
        height: 180,
    };
    let bottom = DockRect {
        x: 100,
        y: 380,
        width: 300,
        height: 40,
    };

    assert_eq!(taskbar_tooltip_anchor(bottom, work, (500, 240)), (100, 200));
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
fn drag_geometry_rescales_logical_length_for_target_dpi() {
    let horizontal = DockRect {
        x: 0,
        y: 1040,
        width: 1920,
        height: 40,
    };
    let (at_96, _) =
        drag_rect_for_logical_length_at_dpi(horizontal, 64, 96, (500, 1060), 0.5, 0.5).unwrap();
    let (at_144, _) =
        drag_rect_for_logical_length_at_dpi(horizontal, 64, 144, (500, 1060), 0.5, 0.5).unwrap();
    assert_eq!(at_96.width, 64);
    assert_eq!(at_144.width, 96);
    assert_eq!(at_96.x, 468);
    assert_eq!(at_144.x, 452);

    let vertical = DockRect {
        x: -60,
        y: 0,
        width: 60,
        height: 1080,
    };
    let (side, _) =
        drag_rect_for_logical_length_at_dpi(vertical, 64, 144, (-30, 500), 0.25, 0.5).unwrap();
    assert_eq!(side.height, 96);
    assert_eq!(side.y, 476);
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
            maximized: false,
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
            maximized: false,
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
fn iszoomed_window_is_maximized_even_when_its_frame_covers_the_monitor() {
    let monitor = DockRect {
        x: 0,
        y: 0,
        width: 1920,
        height: 1080,
    };
    let candidate = WindowCoverageCandidate {
        hwnd: 1,
        visible: true,
        minimized: false,
        cloaked: false,
        maximized: true,
        rect: monitor,
        monitor,
        work_area: DockRect {
            x: 0,
            y: 0,
            width: 1920,
            height: 1032,
        },
    };

    assert_eq!(
        visible_window_coverage_on_monitor(&[candidate], &[], monitor),
        (false, true)
    );
}

#[test]
fn monitor_device_key_is_stable_across_case_and_surrounding_space() {
    assert_eq!(
        taskbar_monitor_device_key(r"  \\.\DISPLAY2  "),
        r"device:\\.\display2"
    );
    assert_eq!(
        taskbar_monitor_path_key("  MONITOR\\ACME123\\{ABC}  "),
        "monitor-path:monitor\\acme123\\{abc}"
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
            maximized: true,
            rect: secondary_work_area,
            monitor: secondary_monitor,
            work_area: secondary_work_area,
        },
        WindowCoverageCandidate {
            hwnd: 2,
            visible: true,
            minimized: false,
            cloaked: false,
            maximized: false,
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
            maximized: false,
            rect: primary_monitor,
            monitor: primary_monitor,
            work_area: primary_work_area,
        },
        WindowCoverageCandidate {
            hwnd: 2,
            visible: true,
            minimized: false,
            cloaked: false,
            maximized: false,
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
