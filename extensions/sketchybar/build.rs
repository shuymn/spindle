fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        cc::Build::new()
            .file("c/sketchybar_mach.c")
            .warnings(true)
            .compile("sketchybar_mach");
    }
}
