//! The version, in one place, and a test that keeps the copies honest.
//!
//! It appears in files that cannot all read each other at build time:
//! `Cargo.toml`, the installer package, the Windows resource script, and the
//! Settings card. `env!("CARGO_PKG_VERSION")` covers the two manifests; the
//! test below reads the other two and proves they agree. A version that drifts
//! is worse than none at all, because it makes a bug report impossible to place.

/// The version the manifests declare.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The interface file, which carries the string the Settings card shows.
#[cfg(test)]
const INTERFACE: &str = include_str!("../ui/version.slint");

/// The version the interface declares, read back out of `version.slint`.
#[cfg(test)]
fn ui_version() -> &'static str {
    const KEY: &str = "out property <string> value: \"";
    let start = INTERFACE
        .find(KEY)
        .expect("version.slint must declare Version")
        + KEY.len();
    let rest = &INTERFACE[start..];
    let end = rest.find('"').expect("the version literal must be closed");
    &rest[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Windows resource carries the version three times: two human strings
    /// and the numeric `FILEVERSION` quadruple that Explorer reads. All three
    /// are checked, because the numeric one drifted silently once already.
    #[test]
    fn every_version_agrees() {
        let ui = ui_version();
        let parts: Vec<u32> = ui
            .split('.')
            .map(|part| part.parse().expect("version parts are numbers"))
            .collect();
        assert_eq!(parts.len(), 3, "UI version must be major.minor.patch");
        let (major, minor, patch) = (parts[0], parts[1], parts[2]);

        let manifest: Vec<u32> = VERSION
            .split('.')
            .map(|part| part.parse().expect("version parts are numbers"))
            .collect();
        assert_eq!(parts, manifest, "Cargo.toml and the interface disagree");

        let rc = include_str!("../assets/windows.rc");
        let quoted = format!("\"{ui}\"");
        assert!(
            rc.contains(&format!("FileVersion\", {quoted}")),
            "FileVersion"
        );
        assert!(
            rc.contains(&format!("ProductVersion\", {quoted}")),
            "ProductVersion"
        );
        let numeric = format!("FILEVERSION {major},{minor},{patch},0");
        assert!(rc.contains(&numeric), "windows.rc numeric {numeric}");

        let installer = include_str!("../installer/Cargo.toml");
        assert!(
            installer.contains(&format!("version = \"{ui}\"")),
            "the installer package must ship the same version"
        );
        // The installer window used to carry its own literal, which is how it
        // kept advertising an old version after everything else had moved on.
        let setup = include_str!("../ui/installer/setup.slint");
        assert!(
            !setup.contains(&format!("\"v{ui}\"")),
            "the installer window must not hardcode a version"
        );
    }
}
