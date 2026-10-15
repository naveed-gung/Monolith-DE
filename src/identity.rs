use crate::identity_data as data;
use crate::library::Result;
use crate::seal::decode;
use std::{io::Read, path::Path, process::Command};

// Only a public product mark is embedded. Personal portraits stay on disk.
const AVATAR: &[u8] = include_bytes!("../assets/monolith-mark-72.png");

/// Decoded developer identity, resolved once at startup.
pub struct Identity {
    pub name: String,
    pub role: String,
    pub github: String,
    pub portfolio: String,
    pub github_url: String,
    pub portfolio_url: String,
    /// Where the crash reporter sends a report. Sealed like the rest, because it
    /// carries the same handle: a literal here puts it back in the binary.
    pub issue_repo: String,
}

impl Default for Identity {
    fn default() -> Self {
        Self {
            name: decode(&data::NAME, 0),
            role: decode(&data::ROLE, 1),
            github: decode(&data::GITHUB, 2),
            portfolio: decode(&data::PORTFOLIO, 3),
            github_url: decode(&data::URL_GITHUB, 5),
            portfolio_url: decode(&data::URL_PORTFOLIO, 6),
            issue_repo: decode(&data::ISSUE_REPO, 8),
        }
    }
}

/// Public product avatar, with no personal portrait in the source or package.
pub fn avatar() -> Option<slint::Image> {
    decode_avatar(AVATAR)
}

/// Optional local portrait; never required by builds or embedded in packages.
/// Read before constructing the UI. Missing/invalid files keep the product mark.
pub fn avatar_from(data: &Path) -> Option<slint::Image> {
    let local = (|| {
        let mut bytes = Vec::new();
        std::fs::File::open(data.join("developer-photo.png"))
            .ok()?
            .take(256 * 1024 + 1)
            .read_to_end(&mut bytes)
            .ok()?;
        (bytes.len() <= 256 * 1024)
            .then(|| decode_avatar(&bytes))
            .flatten()
    })();
    local.or_else(avatar)
}

fn decode_avatar(bytes: &[u8]) -> Option<slint::Image> {
    let mut reader =
        image::ImageReader::with_format(std::io::Cursor::new(bytes), image::ImageFormat::Png);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(512);
    limits.max_image_height = Some(512);
    limits.max_alloc = Some(4 * 1024 * 1024);
    reader.limits(limits);
    let decoded = reader.decode().ok()?;
    let rgba = decoded.to_rgba8();
    let (width, height) = rgba.dimensions();
    Some(slint::Image::from_rgba8(
        slint::SharedPixelBuffer::clone_from_slice(&rgba.into_raw(), width, height),
    ))
}

/// Opens a decoded link in the user's browser.
///
/// Only `http` and `https` are accepted, so a corrupted or edited payload
/// cannot turn the card into a `file://` or custom-scheme launcher. The
/// handler is invoked with an argument array, never a shell string, so a link
/// containing shell metacharacters stays a single inert argument.
pub fn open(link: &str) -> Result<()> {
    if !(link.starts_with("https://") || link.starts_with("http://")) {
        return Err("refusing to open a non-web link".into());
    }
    let mut handler = Command::new(browser_handler());
    #[cfg(windows)]
    handler.args(["/C", "start", "", link]);
    #[cfg(not(windows))]
    handler.arg(link);
    handler
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("could not open the browser: {error}").into())
}

#[cfg(windows)]
fn browser_handler() -> &'static str {
    // `start` is a cmd builtin, so cmd.exe has to host it. Empty title keeps a
    // quoted URL from being read as the window title.
    "cmd"
}

#[cfg(not(windows))]
fn browser_handler() -> &'static str {
    "xdg-open"
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::seal::encode;

    /// Guards the generator and the decoder against drifting apart. Encoding a
    /// decoded value must reproduce the stored bytes exactly.
    #[test]
    fn every_field_round_trips_through_the_keystream() {
        let stored: [(&[u8], u8); 7] = [
            (&data::NAME, 0),
            (&data::ROLE, 1),
            (&data::GITHUB, 2),
            (&data::PORTFOLIO, 3),
            (&data::URL_GITHUB, 5),
            (&data::URL_PORTFOLIO, 6),
            (&data::ISSUE_REPO, 8),
        ];
        for (bytes, field) in stored {
            assert_eq!(encode(&decode(bytes, field), field), bytes);
        }
    }

    /// A field that decoded to the wrong stream would still be valid UTF-8, so
    /// check the shape instead of a literal. No identity text lives in source.
    #[test]
    fn decoded_fields_are_printable_and_non_empty() {
        let identity = Identity::default();
        for value in [
            &identity.name,
            &identity.role,
            &identity.github,
            &identity.portfolio,
            &identity.github_url,
            &identity.portfolio_url,
        ] {
            assert!(!value.is_empty());
            assert!(value.chars().all(|c| !c.is_control()));
        }
    }

    /// Links must stay links, and must be web links, or `open` would refuse them.
    #[test]
    fn links_are_web_urls() {
        let identity = Identity::default();
        for url in [&identity.github_url, &identity.portfolio_url] {
            assert!(url.starts_with("https://"), "{url} is not https");
            assert!(url.len() > "https://".len() + 4, "{url} has no host");
        }
    }

    #[test]
    fn public_product_avatar_decodes() {
        let image = avatar().expect("the product mark must decode");
        assert!(image.size().width > 0 && image.size().height > 0);
    }

    #[test]
    fn local_avatar_is_optional_and_bounded() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("developer-photo.png");
        let fallback = avatar().unwrap().size();
        assert_eq!(avatar_from(directory.path()).unwrap().size(), fallback);
        // Use a generic fixture, never a personal portrait in tests/source.
        let image = image::RgbaImage::from_pixel(96, 96, image::Rgba([80, 80, 80, 255]));
        image.save(&path).unwrap();
        assert_eq!(avatar_from(directory.path()).unwrap().size().width, 96);
        for invalid in [b"invalid png".as_slice(), &vec![0; 256 * 1024 + 1]] {
            std::fs::write(&path, invalid).unwrap();
            assert_eq!(avatar_from(directory.path()).unwrap().size(), fallback);
        }
        image::RgbaImage::new(513, 1).save(&path).unwrap();
        assert_eq!(avatar_from(directory.path()).unwrap().size(), fallback);
    }

    #[test]
    fn only_web_links_are_opened() {
        for hostile in ["file:///C:/Windows/System32", "javascript:alert(1)"] {
            assert!(open(hostile).is_err(), "{hostile} must be rejected");
        }
    }
}
