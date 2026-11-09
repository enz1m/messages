use enzui_iced_core::decode_limits::DecodeLimits;
use iced::widget::image::Handle;

#[derive(Clone, Debug)]
pub struct Thumbnail {
    pub handle: Handle,
    pub rgba_bytes: u64,
}

pub fn decode(bytes: &[u8], cancelled: impl Fn() -> bool) -> Result<Thumbnail, String> {
    let limits = DecodeLimits {
        max_encoded_bytes: 32 * 1024 * 1024,
        max_pixels: 24 * 1024 * 1024,
        max_rgba_bytes: 96 * 1024 * 1024,
        ..DecodeLimits::DEFAULT
    };
    let asset =
        enzui_iced_resources::first_frame::decode_resized(bytes, &limits, 480, 480, cancelled)
            .map_err(|error| error.to_string())?;
    Ok(Thumbnail {
        handle: asset.handle,
        rgba_bytes: asset.rgba_bytes,
    })
}

pub fn decode_avatar(bytes: &[u8], cancelled: impl Fn() -> bool) -> Result<Thumbnail, String> {
    let limits = DecodeLimits {
        max_encoded_bytes: 262144,
        max_pixels: 4 * 1024 * 1024,
        max_rgba_bytes: 16 * 1024 * 1024,
        ..DecodeLimits::DEFAULT
    };
    let asset =
        enzui_iced_resources::first_frame::decode_resized(bytes, &limits, 128, 128, cancelled)
            .map_err(|_| "Profile photo unavailable".to_owned())?;
    Ok(Thumbnail {
        handle: asset.handle,
        rgba_bytes: asset.rgba_bytes,
    })
}
