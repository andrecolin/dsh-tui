//! Inline image transmission for terminals that support it.
//!
//! ratatui paints a grid of cells and knows nothing about images, so drawing one means
//! reserving blank cells in the layout and then writing the terminal's own escape sequence
//! positioned over them, after the frame is drawn. That ordering matters: writing before
//! the draw would have the buffer paint over the image.
//!
//! **What each protocol actually accepts** decides where a placeholder is still the honest
//! answer:
//!
//! - **kitty** takes PNG directly (`f=100`); other formats need a decode-and-reencode this
//!   build does not do, so a JPEG falls back to a placeholder rather than being sent as
//!   bytes kitty will reject.
//! - **iTerm2** hands the data to the OS image decoder, so PNG and JPEG both work.
//! - **sixel** requires rasterizing and quantizing the image to a palette — a real image
//!   pipeline. Detecting sixel and then emitting nothing but a placeholder is honest;
//!   emitting malformed sixel would corrupt the screen.

use base64::Engine as _;

use crate::attachment::Graphics;

/// Where an image should appear, in terminal cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellRect {
    pub x: u16,
    pub y: u16,
    pub cols: u16,
    pub rows: u16,
}

/// One image ready to be written to the terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placement {
    pub rect: CellRect,
    /// The escape sequence, cursor positioning included.
    pub payload: String,
}

/// Why an image could not be drawn inline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unsupported {
    /// The terminal reported no inline-image protocol.
    NoProtocol,
    /// The protocol is present but cannot take this media type as-is.
    MediaType,
    /// The protocol needs a raster pipeline this build does not have.
    NeedsRasterizer,
}

/// Build the escape sequence that draws `bytes` at `rect`.
///
/// Returns the reason instead when the image cannot be drawn, so the caller can render a
/// placeholder that says which case it hit.
pub fn placement(
    graphics: Graphics,
    media_type: &str,
    bytes: &[u8],
    rect: CellRect,
) -> Result<Placement, Unsupported> {
    let payload = match graphics {
        Graphics::None => return Err(Unsupported::NoProtocol),
        // Sixel needs the image rasterized and quantized; emitting anything else would
        // corrupt the screen rather than degrade.
        Graphics::Sixel => return Err(Unsupported::NeedsRasterizer),
        Graphics::Kitty => {
            if media_type != "image/png" {
                // kitty's `f=100` is PNG; sending a JPEG under it would be rejected.
                return Err(Unsupported::MediaType);
            }
            kitty(bytes, rect)
        }
        Graphics::Iterm2 => {
            if !matches!(media_type, "image/png" | "image/jpeg" | "image/gif") {
                return Err(Unsupported::MediaType);
            }
            iterm2(bytes, rect)
        }
    };
    Ok(Placement {
        rect,
        payload: format!("{}{payload}", cursor_to(rect.x, rect.y)),
    })
}

/// Move the cursor to a cell, 1-based as the terminal counts.
fn cursor_to(x: u16, y: u16) -> String {
    format!("\x1b[{};{}H", y + 1, x + 1)
}

/// Maximum base64 payload per kitty escape, from its protocol documentation.
const KITTY_CHUNK: usize = 4096;

/// kitty graphics protocol: transmit and display, chunked.
fn kitty(bytes: &[u8], rect: CellRect) -> String {
    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
    let mut out = String::new();
    let chunks: Vec<&str> = encoded
        .as_bytes()
        .chunks(KITTY_CHUNK)
        .map(|chunk| std::str::from_utf8(chunk).expect("base64 is ASCII"))
        .collect();

    for (index, chunk) in chunks.iter().enumerate() {
        // `m=1` means more chunks follow; the final one carries `m=0`.
        let more = u8::from(index + 1 < chunks.len());
        if index == 0 {
            // The first escape carries the format and the cell box to scale into.
            out.push_str(&format!(
                "\x1b_Ga=T,f=100,c={},r={},m={more};{chunk}\x1b\\",
                rect.cols, rect.rows
            ));
        } else {
            out.push_str(&format!("\x1b_Gm={more};{chunk}\x1b\\"));
        }
    }
    out
}

/// iTerm2 inline image protocol.
fn iterm2(bytes: &[u8], rect: CellRect) -> String {
    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
    format!(
        "\x1b]1337;File=inline=1;size={};width={};height={};preserveAspectRatio=1:{encoded}\x07",
        bytes.len(),
        rect.cols,
        rect.rows
    )
}

/// A short reason for the placeholder line.
pub fn reason(unsupported: Unsupported) -> &'static str {
    match unsupported {
        Unsupported::NoProtocol => "this terminal cannot show images inline",
        Unsupported::MediaType => "this terminal's image protocol cannot take this format",
        Unsupported::NeedsRasterizer => "sixel output is not implemented",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect() -> CellRect {
        CellRect { x: 4, y: 2, cols: 20, rows: 10 }
    }

    fn png() -> Vec<u8> {
        // A PNG signature is enough: nothing here decodes the image.
        let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        bytes.extend(std::iter::repeat_n(0u8, 100));
        bytes
    }

    #[test]
    fn a_terminal_without_a_protocol_draws_nothing() {
        let error = placement(Graphics::None, "image/png", &png(), rect()).unwrap_err();
        assert_eq!(error, Unsupported::NoProtocol);
        assert!(reason(error).contains("cannot show images"));
    }

    #[test]
    fn sixel_is_detected_but_not_emitted() {
        // Emitting malformed sixel would corrupt the screen; a placeholder degrades.
        let error = placement(Graphics::Sixel, "image/png", &png(), rect()).unwrap_err();
        assert_eq!(error, Unsupported::NeedsRasterizer);
    }

    #[test]
    fn kitty_takes_png_and_refuses_jpeg() {
        assert!(placement(Graphics::Kitty, "image/png", &png(), rect()).is_ok());
        // `f=100` is PNG; a JPEG under it would be rejected by the terminal.
        assert_eq!(
            placement(Graphics::Kitty, "image/jpeg", &png(), rect()).unwrap_err(),
            Unsupported::MediaType
        );
    }

    #[test]
    fn iterm2_takes_what_the_os_decoder_takes() {
        for media in ["image/png", "image/jpeg", "image/gif"] {
            assert!(
                placement(Graphics::Iterm2, media, &png(), rect()).is_ok(),
                "{media}"
            );
        }
        assert_eq!(
            placement(Graphics::Iterm2, "image/webp", &png(), rect()).unwrap_err(),
            Unsupported::MediaType
        );
    }

    #[test]
    fn a_kitty_payload_carries_the_cell_box_and_terminates() {
        let placement = placement(Graphics::Kitty, "image/png", &png(), rect()).unwrap();
        assert!(placement.payload.starts_with("\x1b[3;5H"), "cursor first");
        assert!(placement.payload.contains("\x1b_Ga=T,f=100,c=20,r=10,m=0;"));
        assert!(placement.payload.ends_with("\x1b\\"));
    }

    #[test]
    fn a_large_image_is_chunked_with_only_the_last_marked_final() {
        // Three chunks' worth of base64, so continuation marking is exercised.
        let big = vec![7u8; KITTY_CHUNK * 2];
        let placement = placement(Graphics::Kitty, "image/png", &big, rect()).unwrap();
        let more = placement.payload.matches("m=1;").count();
        let last = placement.payload.matches("m=0;").count();
        assert!(more >= 2, "expected continuation chunks, got {more}");
        assert_eq!(last, 1, "exactly one chunk ends the transmission");
        // Only the first escape carries the format and geometry.
        assert_eq!(placement.payload.matches("a=T,f=100").count(), 1);
    }

    #[test]
    fn an_iterm2_payload_declares_its_byte_size() {
        let bytes = png();
        let placement = placement(Graphics::Iterm2, "image/png", &bytes, rect()).unwrap();
        assert!(placement.payload.contains(&format!("size={}", bytes.len())));
        assert!(placement.payload.contains("width=20;height=10"));
        assert!(placement.payload.contains("preserveAspectRatio=1"));
        assert!(placement.payload.ends_with('\x07'));
    }

    #[test]
    fn the_payload_is_positioned_at_the_reserved_cells() {
        // Cells are zero-based here and one-based on the wire.
        let placement = placement(
            Graphics::Iterm2,
            "image/png",
            &png(),
            CellRect { x: 0, y: 0, cols: 4, rows: 2 },
        )
        .unwrap();
        assert!(placement.payload.starts_with("\x1b[1;1H"));
    }

    #[test]
    fn the_encoded_bytes_round_trip() {
        let bytes = png();
        let placement = placement(Graphics::Iterm2, "image/png", &bytes, rect()).unwrap();
        let encoded = placement
            .payload
            .rsplit_once(':')
            .expect("payload separator")
            .1
            .trim_end_matches('\x07');
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .expect("valid base64");
        assert_eq!(decoded, bytes);
    }
}
