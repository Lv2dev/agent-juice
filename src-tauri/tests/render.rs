use agent_juice::render::*;

#[test]
fn worst_and_color_and_svg() {
    assert_eq!(worst(Some(88.0), Some(41.0)), Some(88.0));
    assert_eq!(worst(None, Some(41.0)), Some(41.0));
    assert_eq!(worst(None, None), None);

    assert_eq!(color_for(50.0, 70.0, 90.0, Palette::Traffic), "#22c55e");
    assert_eq!(color_for(80.0, 70.0, 90.0, Palette::Traffic), "#f59e0b");
    assert_eq!(color_for(95.0, 70.0, 90.0, Palette::Traffic), "#ef4444");
    assert_eq!(color_for(95.0, 70.0, 90.0, Palette::Cvd), "#cc79a7");
    assert_eq!(color_for(50.0, 70.0, 90.0, Palette::Ocean), "#0f9fb5");
    assert_eq!(color_for(80.0, 70.0, 90.0, Palette::Forest), "#b18432");
    assert_eq!(color_for(95.0, 70.0, 90.0, Palette::Sunset), "#9658b3");
    assert_eq!(
        color_for(50.0, 70.0, 90.0, Palette::Mono([0x34, 0x56, 0x78])),
        "#345678"
    );
    assert_eq!(
        color_for(95.0, 70.0, 90.0, Palette::Mono([0x34, 0x56, 0x78])),
        "#ef4444"
    );
    assert_eq!(
        color_for(
            95.0,
            70.0,
            90.0,
            Palette::Custom([1, 2, 3], [4, 5, 6], [7, 8, 9])
        ),
        "#070809"
    );

    let svg = ring_svg(Some(88.0), Some(41.0), Some(88.0), "#f59e0b", "#22c55e");
    assert!(svg.contains("<svg") && svg.contains("88"));
    let png = svg_to_png(&svg, 32).unwrap();
    assert!(png.len() > 8 && &png[1..4] == b"PNG");
}
