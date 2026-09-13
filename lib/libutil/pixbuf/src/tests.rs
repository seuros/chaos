use super::*;
use image::GenericImageView;
use image::ImageBuffer;
use image::Rgba;
use tempfile::NamedTempFile;

#[tokio::test(flavor = "multi_thread")]
async fn returns_original_image_when_within_bounds() {
    for (format, mime) in [
        (ImageFormat::Png, "image/png"),
        (ImageFormat::WebP, "image/webp"),
    ] {
        let temp_file = NamedTempFile::new().expect("temp file");
        let image = ImageBuffer::from_pixel(64, 32, Rgba([10u8, 20, 30, 255]));
        image
            .save_with_format(temp_file.path(), format)
            .expect("write image to temp file");

        let original_bytes = std::fs::read(temp_file.path()).expect("read written image");
        let encoded = load_and_resize_to_fit(temp_file.path()).expect("process image");

        assert_eq!(encoded.width, 64);
        assert_eq!(encoded.height, 32);
        assert_eq!(encoded.mime, mime);
        assert_eq!(encoded.bytes, original_bytes);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn downscales_large_image() {
    for (format, mime) in [
        (ImageFormat::Png, "image/png"),
        (ImageFormat::WebP, "image/webp"),
    ] {
        let temp_file = NamedTempFile::new().expect("temp file");
        let image = ImageBuffer::from_pixel(4096, 2048, Rgba([200u8, 10, 10, 255]));
        image
            .save_with_format(temp_file.path(), format)
            .expect("write image to temp file");

        let processed = load_and_resize_to_fit(temp_file.path()).expect("process image");

        assert!(processed.width <= MAX_WIDTH);
        assert!(processed.height <= MAX_HEIGHT);
        assert_eq!(processed.mime, mime);

        let detected_format =
            image::guess_format(&processed.bytes).expect("detect resized output format");
        assert_eq!(detected_format, format);

        let loaded =
            image::load_from_memory(&processed.bytes).expect("read resized bytes back into image");
        assert_eq!(loaded.dimensions(), (processed.width, processed.height));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn preserves_large_image_in_original_mode() {
    let temp_file = NamedTempFile::new().expect("temp file");
    let image = ImageBuffer::from_pixel(4096, 2048, Rgba([180u8, 30, 30, 255]));
    image
        .save_with_format(temp_file.path(), ImageFormat::Png)
        .expect("write png to temp file");

    let original_bytes = std::fs::read(temp_file.path()).expect("read written image");
    let processed =
        load_for_prompt(temp_file.path(), PromptImageMode::Original).expect("process image");

    assert_eq!(processed.width, 4096);
    assert_eq!(processed.height, 2048);
    assert_eq!(processed.mime, "image/png");
    assert_eq!(processed.bytes, original_bytes);
}

#[tokio::test(flavor = "multi_thread")]
async fn fails_cleanly_for_invalid_images() {
    let temp_file = NamedTempFile::new().expect("temp file");
    std::fs::write(temp_file.path(), b"not an image").expect("write bytes");

    let err = load_and_resize_to_fit(temp_file.path()).expect_err("invalid image should fail");
    match err {
        ImageProcessingError::Decode { .. } => {}
        _ => panic!("unexpected error variant"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn reprocesses_updated_file_contents() {
    {
        IMAGE_CACHE.clear();
    }

    let temp_file = NamedTempFile::new().expect("temp file");
    let first_image = ImageBuffer::from_pixel(32, 16, Rgba([20u8, 120, 220, 255]));
    first_image
        .save_with_format(temp_file.path(), ImageFormat::Png)
        .expect("write initial image");

    let first = load_and_resize_to_fit(temp_file.path()).expect("process first image");

    let second_image = ImageBuffer::from_pixel(96, 48, Rgba([50u8, 60, 70, 255]));
    second_image
        .save_with_format(temp_file.path(), ImageFormat::Png)
        .expect("write updated image");

    let second = load_and_resize_to_fit(temp_file.path()).expect("process updated image");

    assert_eq!(first.width, 32);
    assert_eq!(first.height, 16);
    assert_eq!(second.width, 96);
    assert_eq!(second.height, 48);
    assert_ne!(second.bytes, first.bytes);
}
