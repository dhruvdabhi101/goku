use std::error::Error;
use std::hint::black_box;
use std::time::Instant;

use goku_core::{
    encode_jpeg, prepare_image, redact_text, OnUnmappable, PersistableImage, PixelPolicy,
    PixelRect, RawImage,
};
use image::codecs::jpeg::JpegEncoder;
use image::{ExtendedColorType, GenericImageView};

fn copied_rgb(image: &PersistableImage) -> Result<Vec<u8>, Box<dyn Error>> {
    let rgb: Vec<u8> = image
        .rgba()
        .chunks_exact(4)
        .flat_map(|px| px[..3].iter().copied())
        .collect();
    let mut out = Vec::new();
    JpegEncoder::new_with_quality(&mut out, 80).encode(
        &rgb,
        image.width(),
        image.height(),
        ExtendedColorType::Rgb8,
    )?;
    Ok(out)
}

fn pixels(width: u32, height: u32) -> Result<RawImage, Box<dyn Error>> {
    let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
    for y in 0..height {
        for x in 0..width {
            let text = (x / 7 + y / 16) % 9 == 0 && y % 16 < 9;
            let value = if text {
                32
            } else {
                224 + ((x / 64 + y / 64) % 24) as u8
            };
            rgba.extend_from_slice(&[
                value,
                value.saturating_sub((x % 17) as u8),
                value.saturating_sub((y % 13) as u8),
                255,
            ]);
        }
    }
    Ok(RawImage::new(width, height, rgba)?)
}

fn main() -> Result<(), Box<dyn Error>> {
    let rounds: usize = std::env::args()
        .skip(1)
        .filter(|arg| arg != "--bench")
        .next()
        .as_deref()
        .unwrap_or("6")
        .parse()?;
    if rounds < 5 {
        return Err("at least five rounds are required".into());
    }
    println!(
        "variant,width,height,round,frames,total_ms,redact_ms,prepare_ms,jpeg_ms,jpeg_bytes,errors"
    );
    let text = "Synthetic document. token=sk-abcdefghijklmnopqrstuvwxyz1234567890\n".repeat(64);
    let rect = PixelRect {
        x: 0,
        y: 0,
        width: 64,
        height: 32,
    };
    let policy = PixelPolicy::SecretRegions {
        on_unmappable: OnUnmappable::WithholdImage,
    };
    for (width, height) in [(1280, 720), (1920, 1080), (3840, 2160)] {
        let raw = pixels(width, height)?;
        let mut redaction = redact_text(&text);
        for hit in &mut redaction.hits {
            hit.region = Some(rect);
        }
        assert!(!redaction.hits.is_empty());
        assert!(!redaction.text.contains("sk-abcdefghijklmnopqrstuvwxyz"));
        let image = prepare_image(raw.clone(), &redaction.hits, &[], policy)?;
        let expected = copied_rgb(&image)?;
        let actual = encode_jpeg(&image, 80)?;
        assert_eq!(actual, expected, "JPEG output must stay byte-identical");
        let decoded = image::load_from_memory(&actual)?;
        assert_eq!(decoded.dimensions(), (width, height));
        assert!(decoded.get_pixel(8, 8).0[..3].iter().all(|&v| v < 8));
        for round in 0..rounds {
            let order = if round % 2 == 0 {
                ["copied_rgb", "current"]
            } else {
                ["current", "copied_rgb"]
            };
            for variant in order {
                let mut redact_ms = 0.0;
                let mut prepare_ms = 0.0;
                let mut jpeg_ms = 0.0;
                let mut bytes = 0;
                let start = Instant::now();
                for _ in 0..3 {
                    let t = Instant::now();
                    let mut redaction = redact_text(black_box(&text));
                    for hit in &mut redaction.hits {
                        hit.region = Some(rect);
                    }
                    redact_ms += t.elapsed().as_secs_f64() * 1000.0;
                    let t = Instant::now();
                    let image = prepare_image(raw.clone(), &redaction.hits, &[], policy)?;
                    prepare_ms += t.elapsed().as_secs_f64() * 1000.0;
                    let t = Instant::now();
                    let jpeg = if variant == "current" {
                        encode_jpeg(&image, 80)?
                    } else {
                        copied_rgb(&image)?
                    };
                    jpeg_ms += t.elapsed().as_secs_f64() * 1000.0;
                    assert_eq!(jpeg.len(), expected.len());
                    bytes += black_box(jpeg.len());
                    black_box(jpeg);
                }
                println!("{variant},{width},{height},{round},3,{:.3},{redact_ms:.3},{prepare_ms:.3},{jpeg_ms:.3},{bytes},0", start.elapsed().as_secs_f64() * 1000.0);
            }
        }
    }
    Ok(())
}
