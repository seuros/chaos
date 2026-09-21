use std::io::{self, Cursor};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader, Limits};

pub const MAX_IMAGE_BYTES: usize = 4 * 1024 * 1024;
const MAX_IMAGE_SIDE: u32 = 4096;
const MAX_IMAGE_PIXELS: u64 = 8 * 1024 * 1024;

pub fn decode_base64(blob: &str, formats: Option<&[ImageFormat]>) -> io::Result<DynamicImage> {
    if blob.len() > MAX_IMAGE_BYTES.div_ceil(3) * 4 {
        return Err(io::Error::other("image exceeds the 4 MiB preview limit"));
    }
    let bytes = STANDARD.decode(blob).map_err(io::Error::other)?;
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err(io::Error::other("image exceeds the 4 MiB preview limit"));
    }
    let mut reader = ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
    if let Some(formats) = formats
        && reader
            .format()
            .is_none_or(|format| !formats.contains(&format))
    {
        return Err(io::Error::other("unsupported preview image format"));
    }
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_SIDE);
    limits.max_image_height = Some(MAX_IMAGE_SIDE);
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits);
    let decoder = reader.into_decoder().map_err(io::Error::other)?;
    let (width, height) = decoder.dimensions();
    if width == 0 || height == 0 || u64::from(width) * u64::from(height) > MAX_IMAGE_PIXELS {
        return Err(io::Error::other(
            "image exceeds the 8 megapixel preview limit",
        ));
    }
    DynamicImage::from_decoder(decoder).map_err(io::Error::other)
}
