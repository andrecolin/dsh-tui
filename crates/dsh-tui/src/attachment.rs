//! Image attachments: what the terminal can show, and what the host will accept.
//!
//! **The terminal-image decision.** A browser renders any image inline. A terminal can only
//! do so through a protocol its emulator implements — kitty's graphics protocol, iTerm2's
//! inline images, or sixel — and most terminals implement none. Rather than pretending, the
//! capability is detected once and an image that cannot be drawn gets a descriptive
//! placeholder naming the file and why. A blank space would read as a broken attachment.
//!
//! Admission is checked **before** sending, against the `imageLimits` projection. The host
//! enforces these anyway; checking first turns a rejected turn into an immediate, specific
//! message about the file the user just chose.

use serde::Deserialize;

/// The host's image-intake limits.
#[derive(Debug, Clone, Deserialize)]
pub struct Limits {
    #[serde(default, rename = "maxImageBytes")]
    pub max_image_bytes: u64,
    #[serde(default, rename = "maxImagesPerMessage")]
    pub max_images_per_message: usize,
    #[serde(default, rename = "maxMessageImageBytes")]
    pub max_message_image_bytes: u64,
    #[serde(default, rename = "maxImagePixels")]
    pub max_image_pixels: u64,
    /// Maximum intrinsic width and maximum intrinsic height for one image.
    #[serde(default, rename = "maxImageDimension")]
    pub max_image_dimension: u32,
    #[serde(default, rename = "mediaTypes")]
    pub media_types: Vec<String>,
}

impl Limits {
    /// Read the limits from the projection map, if the capability is composed.
    pub fn read(projections: Option<&serde_json::Value>) -> Option<Self> {
        let value = projections?.get("imageLimits")?;
        serde_json::from_value(value.clone()).ok()
    }
}

/// One image staged for the next message.
///
/// Carries its bytes: they are needed both to draw the image inline and to send it, since
/// the host takes a base64 `EncodedImageAttachment` rather than a path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Draft {
    pub name: String,
    pub media_type: String,
    pub data: Vec<u8>,
    /// Intrinsic dimensions, when they could be read.
    pub dimensions: Option<(u32, u32)>,
}

impl Draft {
    pub fn bytes(&self) -> u64 {
        self.data.len() as u64
    }
}

/// How this terminal can show an image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Graphics {
    /// kitty's graphics protocol.
    Kitty,
    /// iTerm2 inline images.
    Iterm2,
    /// sixel.
    Sixel,
    /// No inline image support; attachments render as placeholders.
    None,
}

impl Graphics {
    /// Detect from the environment.
    ///
    /// Detection is best-effort and deliberately conservative: claiming a protocol the
    /// emulator lacks prints escape-sequence garbage across the transcript, which is worse
    /// than a placeholder.
    pub fn detect(
        term: Option<&str>,
        term_program: Option<&str>,
        kitty_window_id: Option<&str>,
    ) -> Self {
        if kitty_window_id.is_some() || term.is_some_and(|term| term.contains("kitty")) {
            return Graphics::Kitty;
        }
        if term_program.is_some_and(|program| program.eq_ignore_ascii_case("iTerm.app")) {
            return Graphics::Iterm2;
        }
        if term.is_some_and(|term| term.contains("sixel") || term == "mlterm") {
            return Graphics::Sixel;
        }
        Graphics::None
    }

    pub fn from_env() -> Self {
        Self::detect(
            std::env::var("TERM").ok().as_deref(),
            std::env::var("TERM_PROGRAM").ok().as_deref(),
            std::env::var("KITTY_WINDOW_ID").ok().as_deref(),
        )
    }

    pub fn can_draw(self) -> bool {
        !matches!(self, Graphics::None)
    }
}

/// What to show for one attachment in a terminal that cannot draw it.
pub fn placeholder(draft: &Draft) -> String {
    let size = format_bytes(draft.bytes());
    match draft.dimensions {
        Some((width, height)) => format!("[image] {} · {width}×{height} · {size}", draft.name),
        None => format!("[image] {} · {size}", draft.name),
    }
}

fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.0} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

/// Whether one more image may join the draft.
///
/// Every rejection names the limit it hit, so the message says what to do next rather than
/// only that something was refused.
pub fn admit(staged: &[Draft], candidate: &Draft, limits: &Limits) -> Result<(), String> {
    if !limits.media_types.is_empty()
        && !limits
            .media_types
            .iter()
            .any(|allowed| allowed == &candidate.media_type)
    {
        return Err(format!(
            "{} is not an accepted image type ({})",
            candidate.media_type,
            limits.media_types.join(", ")
        ));
    }
    if limits.max_image_bytes > 0 && candidate.bytes() > limits.max_image_bytes {
        return Err(format!(
            "{} is {} — the limit is {} per image",
            candidate.name,
            format_bytes(candidate.bytes()),
            format_bytes(limits.max_image_bytes)
        ));
    }
    if limits.max_images_per_message > 0 && staged.len() >= limits.max_images_per_message {
        return Err(format!(
            "a message takes at most {} image{}",
            limits.max_images_per_message,
            if limits.max_images_per_message == 1 { "" } else { "s" }
        ));
    }
    let total: u64 = staged.iter().map(Draft::bytes).sum::<u64>() + candidate.bytes();
    if limits.max_message_image_bytes > 0 && total > limits.max_message_image_bytes {
        return Err(format!(
            "the message's images would total {} — the limit is {}",
            format_bytes(total),
            format_bytes(limits.max_message_image_bytes)
        ));
    }
    if let Some((width, height)) = candidate.dimensions {
        if limits.max_image_dimension > 0
            && (width > limits.max_image_dimension || height > limits.max_image_dimension)
        {
            return Err(format!(
                "{width}×{height} exceeds the {}px maximum side",
                limits.max_image_dimension
            ));
        }
        let pixels = u64::from(width) * u64::from(height);
        if limits.max_image_pixels > 0 && pixels > limits.max_image_pixels {
            return Err(format!(
                "{width}×{height} is {pixels} pixels — the limit is {}",
                limits.max_image_pixels
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits() -> Limits {
        serde_json::from_value(serde_json::json!({
            "maxImageBytes": 1_048_576,
            "maxImagesPerMessage": 3,
            "maxMessageImageBytes": 2_097_152,
            "maxImagePixels": 3_000_000,
            "maxImageDimension": 2000,
            "mediaTypes": ["image/png", "image/jpeg"]
        }))
        .expect("limits")
    }

    fn draft(name: &str, bytes: u64) -> Draft {
        Draft {
            name: name.into(),
            media_type: "image/png".into(),
            data: vec![0u8; bytes as usize],
            dimensions: Some((800, 600)),
        }
    }

    #[test]
    fn a_conforming_image_is_admitted() {
        assert!(admit(&[], &draft("shot.png", 200_000), &limits()).is_ok());
    }

    #[test]
    fn each_rejection_names_the_limit_it_hit() {
        let limits = limits();

        let wrong_type = Draft { media_type: "image/gif".into(), ..draft("a.gif", 10) };
        let error = admit(&[], &wrong_type, &limits).unwrap_err();
        assert!(error.contains("not an accepted image type"));
        assert!(error.contains("image/png"));

        let too_big = draft("huge.png", 2_000_000);
        assert!(admit(&[], &too_big, &limits).unwrap_err().contains("per image"));

        let staged = vec![draft("a.png", 10), draft("b.png", 10), draft("c.png", 10)];
        assert!(admit(&staged, &draft("d.png", 10), &limits)
            .unwrap_err()
            .contains("at most 3 images"));

        let heavy = vec![draft("a.png", 1_000_000), draft("b.png", 1_000_000)];
        assert!(admit(&heavy, &draft("c.png", 500_000), &limits)
            .unwrap_err()
            .contains("would total"));
    }

    #[test]
    fn dimension_and_pixel_limits_are_separate_checks() {
        let limits = limits();
        let wide = Draft { dimensions: Some((3000, 100)), ..draft("wide.png", 10) };
        assert!(admit(&[], &wide, &limits).unwrap_err().contains("maximum side"));

        // Within the 2000px side limit but over the 3M pixel budget: 2000 × 1900.
        let dense = Draft { dimensions: Some((2000, 1900)), ..draft("dense.png", 10) };
        let error = admit(&[], &dense, &limits).unwrap_err();
        assert!(error.contains("pixels"));
    }

    #[test]
    fn unknown_dimensions_skip_the_geometry_checks() {
        // A file whose header could not be read is still admissible on its other limits;
        // the host re-checks during admission.
        let unknown = Draft { dimensions: None, ..draft("a.png", 10) };
        assert!(admit(&[], &unknown, &limits()).is_ok());
    }

    #[test]
    fn a_zeroed_limit_is_treated_as_unset() {
        let none = serde_json::from_value::<Limits>(serde_json::json!({})).unwrap();
        let huge = Draft { dimensions: Some((99_999, 99_999)), ..draft("x.png", 4096) };
        assert!(admit(&[], &huge, &none).is_ok());
    }

    #[test]
    fn graphics_detection_is_conservative() {
        assert_eq!(Graphics::detect(Some("xterm-kitty"), None, None), Graphics::Kitty);
        assert_eq!(Graphics::detect(None, None, Some("1")), Graphics::Kitty);
        assert_eq!(Graphics::detect(Some("xterm-256color"), Some("iTerm.app"), None), Graphics::Iterm2);
        assert_eq!(Graphics::detect(Some("mlterm"), None, None), Graphics::Sixel);
        // Claiming a protocol the emulator lacks prints garbage across the transcript.
        assert_eq!(Graphics::detect(Some("xterm-256color"), None, None), Graphics::None);
        assert_eq!(Graphics::detect(None, None, None), Graphics::None);
        assert!(!Graphics::None.can_draw());
    }

    #[test]
    fn a_placeholder_describes_what_cannot_be_drawn() {
        let text = placeholder(&draft("screenshot.png", 204_800));
        assert!(text.contains("screenshot.png"));
        assert!(text.contains("800×600"));
        assert!(text.contains("200 KB"));

        let unknown = Draft { dimensions: None, ..draft("a.png", 900) };
        assert_eq!(placeholder(&unknown), "[image] a.png · 900 B");
    }
}
