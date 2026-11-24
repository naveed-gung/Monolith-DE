use std::{
    path::{Path, PathBuf},
    process::Command,
};

pub fn compile(root: &Path, resource: &str, output_name: &str) -> PathBuf {
    println!("cargo:rerun-if-env-changed=RC");
    let compiler = std::env::var_os("RC")
        .map(PathBuf::from)
        .or_else(|| {
            let root =
                PathBuf::from(std::env::var_os("ProgramFiles(x86)")?).join("Windows Kits/10/bin");
            let mut versions = std::fs::read_dir(root)
                .ok()?
                .filter_map(|p| p.ok().map(|p| p.path().join("x64/rc.exe")))
                .filter(|p| p.is_file())
                .collect::<Vec<_>>();
            versions.sort();
            versions.pop()
        })
        .unwrap_or_else(|| "rc.exe".into());
    let output = PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join(output_name);
    let status = Command::new(compiler)
        .arg("/nologo")
        .arg("/fo")
        .arg(&output)
        .arg(resource)
        .current_dir(root)
        .status()
        .expect("Windows SDK resource compiler required");
    assert!(
        status.success(),
        "Could not compile Monolith Windows resources"
    );
    output
}
