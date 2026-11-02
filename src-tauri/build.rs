fn main() {
    // `scripts/release.ps1` and `scripts/live-test.ps1` set a fresh WREN_BUILD_ID for every build and
    // then look for it inside the exe they copy or install, so they can never pick up a wren.exe that
    // another build wrote to the shared target folder in the meantime.
    println!("cargo:rerun-if-env-changed=WREN_BUILD_ID");
    let id = std::env::var("WREN_BUILD_ID").unwrap_or_else(|_| "dev".into());
    println!("cargo:rustc-env=WREN_BUILD_ID={id}");
    tauri_build::build()
}
