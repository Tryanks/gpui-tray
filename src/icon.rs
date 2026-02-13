use anyhow::{Context as _, Result};
use std::sync::{Arc, OnceLock};

/// Decode a `gpui::Image` into BGRA8 pixels (little-endian byte order).
///
/// This leverages GPUI's own decoding path, avoiding a direct dependency on `image` in this crate.
pub(crate) fn decode_gpui_image_to_bgra32(image: &gpui::Image) -> Result<(u32, u32, Vec<u8>)> {
    // `SvgRenderer` is only needed to satisfy the API; it is only used for SVG images.
    // For non-SVG formats, `gpui::Image::to_image_data` ignores the renderer.
    static RENDERER: OnceLock<gpui::SvgRenderer> = OnceLock::new();
    let renderer = RENDERER
        .get_or_init(|| gpui::SvgRenderer::new(Arc::new(())))
        .clone();

    let render = image
        .to_image_data(renderer)
        .context("failed to decode gpui::Image")?;

    let size = render.size(0);
    let bytes = render.as_bytes(0).context("render image frame 0 missing")?;

    Ok((size.width.0 as u32, size.height.0 as u32, bytes.to_vec()))
}
