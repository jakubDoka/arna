fn main() {
    if std::env::var_os("CARGO_FEATURE_EXAMPLE_LINK_RAYLIB").is_some() {
        println!("cargo:rustc-link-lib=dylib=raylib");

        #[cfg(target_os = "linux")]
        println!("cargo:rustc-link-lib=X11");
    }
}
