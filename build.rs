fn main() {
    let config = slint_build::CompilerConfiguration::new().with_style("fluent".into());
    slint_build::compile_with_config("ui/app.slint", config)
        .expect("Could not compile Monolith's interface");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        use std::{path::PathBuf, process::Command};
        println!("cargo:rerun-if-changed=assets/windows.rc");
        println!("cargo:rerun-if-changed=assets/monolith.ico");
        let compiler = std::env::var_os("RC")
            .map(PathBuf::from)
            .or_else(|| {
                let root = PathBuf::from(std::env::var_os("ProgramFiles(x86)")?)
                    .join("Windows Kits/10/bin");
                let mut versions = std::fs::read_dir(root)
                    .ok()?
                    .filter_map(|p| p.ok().map(|p| p.path().join("x64/rc.exe")))
                    .filter(|p| p.is_file())
                    .collect::<Vec<_>>();
                versions.sort();
                versions.pop()
            })
            .unwrap_or_else(|| "rc.exe".into());
        let output = PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("monolith.res");
        let status = Command::new(compiler)
            .arg("/nologo")
            .arg(format!("/fo{}", output.display()))
            .arg("assets/windows.rc")
            .status()
            .expect("Windows SDK resource compiler required");
        assert!(
            status.success(),
            "Could not compile the Monolith executable icon"
        );
        println!(
            "cargo:rustc-link-arg-bin=monolith-desktop={}",
            output.display()
        );
    }
}
