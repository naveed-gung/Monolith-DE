#[path = "assets/compile-windows-resource.rs"]
mod windows_resource;

fn main() {
    let config = slint_build::CompilerConfiguration::new().with_style("fluent".into());
    slint_build::compile_with_config("ui/app.slint", config)
        .expect("Could not compile Monolith's interface");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rerun-if-changed=assets/windows.rc");
        println!("cargo:rerun-if-changed=assets/monolith.ico");
        let output = windows_resource::compile(
            std::path::Path::new("."),
            "assets/windows.rc",
            "monolith.res",
        );
        println!(
            "cargo:rustc-link-arg-bin=monolith-desktop={}",
            output.display()
        );
    }
}
