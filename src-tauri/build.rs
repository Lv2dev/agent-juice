fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_resource::compile_for_tests("windows/test-common-controls.rc", embed_resource::NONE)
            .manifest_required()
            .unwrap();
        println!(
            "cargo:rustc-link-search=native={}",
            std::env::var("OUT_DIR").unwrap()
        );
    }
    tauri_build::build()
}
