use crate::identity_data as data;
use crate::library::Result;
use crate::seal::{Keystream, decode};
use std::process::Command;

/// Portrait, sealed. The PNG is unreadable without the keystream.
const PORTRAIT: &[u8] = include_bytes!("../assets/profile-card.enc");

/// Decoded developer identity, resolved once at startup.
pub struct Identity {
    pub name: String,
    pub role: String,
    pub github: String,
    pub portfolio: String,
    pub instagram: String,
    pub github_url: String,
    pub portfolio_url: String,
    pub instagram_url: String,
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
            instagram: decode(&data::INSTAGRAM, 4),
            github_url: decode(&data::URL_GITHUB, 5),
            portfolio_url: decode(&data::URL_PORTFOLIO, 6),
            instagram_url: decode(&data::URL_INSTAGRAM, 7),
            issue_repo: decode(&data::ISSUE_REPO, 8),
        }
    }
}

/// Unseals the portrait into an RGBA image for Slint.
///
/// Returns `None` rather than a broken image if the PNG header does not come
/// back, which is the cheapest proof the keystream still lines up.
pub fn portrait() -> Option<slint::Image> {
    let mut key = Keystream::new(data::PROFILE_FIELD);
    let png: Vec<u8> = PORTRAIT.iter().map(|byte| byte ^ key.next()).collect();
    if !png.starts_with(b"\x89PNG\r\n\x1a\n") {
        return None;
    }
    let decoded = image::load_from_memory_with_format(&png, image::ImageFormat::Png).ok()?;
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
        let stored: [(&[u8], u8); 9] = [
            (&data::NAME, 0),
            (&data::ROLE, 1),
            (&data::GITHUB, 2),
            (&data::PORTFOLIO, 3),
            (&data::INSTAGRAM, 4),
            (&data::URL_GITHUB, 5),
            (&data::URL_PORTFOLIO, 6),
            (&data::URL_INSTAGRAM, 7),
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
            &identity.instagram,
            &identity.github_url,
            &identity.portfolio_url,
            &identity.instagram_url,
        ] {
            assert!(!value.is_empty());
            assert!(value.chars().all(|c| !c.is_control()));
        }
    }

    /// Links must stay links, and must be web links, or `open` would refuse them.
    #[test]
    fn links_are_web_urls() {
        let identity = Identity::default();
        for url in [
            &identity.github_url,
            &identity.portfolio_url,
            &identity.instagram_url,
        ] {
            assert!(url.starts_with("https://"), "{url} is not https");
            assert!(url.len() > "https://".len() + 4, "{url} has no host");
        }
    }

    #[test]
    fn the_sealed_portrait_is_not_a_png_until_unsealed() {
        assert!(!PORTRAIT.starts_with(b"\x89PNG"));
        let image = portrait().expect("the portrait must unseal");
        assert!(image.size().width > 0 && image.size().height > 0);
    }

    #[test]
    fn only_web_links_are_opened() {
        for hostile in ["file:///C:/Windows/System32", "javascript:alert(1)"] {
            assert!(open(hostile).is_err(), "{hostile} must be rejected");
        }
    }
}
