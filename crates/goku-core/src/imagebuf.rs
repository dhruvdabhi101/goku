use image::codecs::jpeg::JpegEncoder;
use image::{ImageBuffer, Rgba};

use crate::error::RecorderError;
use crate::types::{OnUnmappable, PixelPolicy, PixelRect, SecretHit};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawImage {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

impl RawImage {
    pub fn new(width: u32, height: u32, rgba: Vec<u8>) -> Result<Self, RecorderError> {
        let expected = (width as usize)
            .checked_mul(height as usize)
            .and_then(|n| n.checked_mul(4))
            .ok_or_else(|| RecorderError::capture("image dimensions overflow"))?;
        if rgba.len() != expected || width == 0 || height == 0 {
            return Err(RecorderError::capture(
                "rgba length does not match dimensions",
            ));
        }
        Ok(Self {
            rgba,
            width,
            height,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedactedImage {
    rgba: Vec<u8>,
    width: u32,
    height: u32,
}

impl RedactedImage {
    pub fn rgba(&self) -> &[u8] {
        &self.rgba
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PersistableImage {
    Redacted(RedactedImage),
    ExplicitlyUnredacted(RawImage),
}

impl PersistableImage {
    pub fn rgba(&self) -> &[u8] {
        match self {
            Self::Redacted(img) => img.rgba(),
            Self::ExplicitlyUnredacted(img) => &img.rgba,
        }
    }

    pub fn width(&self) -> u32 {
        match self {
            Self::Redacted(img) => img.width(),
            Self::ExplicitlyUnredacted(img) => img.width,
        }
    }

    pub fn height(&self) -> u32 {
        match self {
            Self::Redacted(img) => img.height(),
            Self::ExplicitlyUnredacted(img) => img.height,
        }
    }
}

pub fn prepare_image(
    raw: RawImage,
    hits: &[SecretHit],
    forced: &[PixelRect],
    policy: PixelPolicy,
) -> Result<PersistableImage, RecorderError> {
    match policy {
        PixelPolicy::Off => Ok(PersistableImage::ExplicitlyUnredacted(raw)),
        PixelPolicy::SecretRegions { on_unmappable } => {
            let unmapped = hits.iter().any(|hit| !hit.mapped());
            if unmapped && on_unmappable == OnUnmappable::WithholdImage {
                return Err(RecorderError::Message(
                    "secret could not be mapped to pixels".into(),
                ));
            }
            let mut rgba = raw.rgba;
            if unmapped && on_unmappable == OnUnmappable::RedactWholeImage {
                paint_all(&mut rgba);
            } else {
                for hit in hits {
                    if let Some(rect) = hit.region {
                        paint_rect(&mut rgba, raw.width, raw.height, rect);
                    }
                }
                for rect in forced {
                    paint_rect(&mut rgba, raw.width, raw.height, *rect);
                }
            }
            Ok(PersistableImage::Redacted(RedactedImage {
                rgba,
                width: raw.width,
                height: raw.height,
            }))
        }
    }
}

pub fn encode_jpeg(image: &PersistableImage, quality: u8) -> Result<Vec<u8>, RecorderError> {
    if !(1..=100).contains(&quality) {
        return Err(RecorderError::config("jpeg quality must be 1-100"));
    }
    let mut out = Vec::new();
    let pixels = ImageBuffer::<Rgba<u8>, _>::from_raw(image.width(), image.height(), image.rgba())
        .ok_or_else(|| RecorderError::capture("rgba length does not match dimensions"))?;
    let mut encoder = JpegEncoder::new_with_quality(&mut out, quality);
    encoder
        .encode_image(&pixels)
        .map_err(|e| RecorderError::capture(e.to_string()))?;
    Ok(out)
}

fn paint_all(rgba: &mut [u8]) {
    for px in rgba.chunks_exact_mut(4) {
        px[0] = 0;
        px[1] = 0;
        px[2] = 0;
        px[3] = 255;
    }
}

fn paint_rect(rgba: &mut [u8], width: u32, height: u32, rect: PixelRect) {
    if rect.is_empty() {
        return;
    }
    let x0 = rect.x.max(0) as u32;
    let y0 = rect.y.max(0) as u32;
    let x1 = ((rect.x as i64) + rect.width as i64).clamp(0, width as i64) as u32;
    let y1 = ((rect.y as i64) + rect.height as i64).clamp(0, height as i64) as u32;
    for y in y0..y1.min(height) {
        for x in x0..x1.min(width) {
            let i = ((y * width + x) * 4) as usize;
            rgba[i] = 0;
            rgba[i + 1] = 0;
            rgba[i + 2] = 0;
            rgba[i + 3] = 255;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: u32, h: u32, pixel: [u8; 4]) -> RawImage {
        let mut rgba = Vec::new();
        for _ in 0..w * h {
            rgba.extend_from_slice(&pixel);
        }
        RawImage::new(w, h, rgba).unwrap()
    }

    #[test]
    fn withhold_drops_the_image_when_a_secret_has_no_box() {
        let raw = solid(2, 2, [1, 2, 3, 255]);
        let hits = [SecretHit {
            label: "OPENAI_KEY".into(),
            start: 0,
            end: 4,
            region: None,
        }];
        let err = prepare_image(
            raw,
            &hits,
            &[],
            PixelPolicy::SecretRegions {
                on_unmappable: OnUnmappable::WithholdImage,
            },
        );
        assert!(err.is_err());
    }

    #[test]
    fn mapped_secret_is_painted_black_and_the_rest_stays() {
        let raw = solid(2, 2, [9, 9, 9, 255]);
        let hits = [SecretHit {
            label: "JWT".into(),
            start: 0,
            end: 3,
            region: Some(PixelRect {
                x: 0,
                y: 0,
                width: 1,
                height: 1,
            }),
        }];
        let img = prepare_image(
            raw,
            &hits,
            &[],
            PixelPolicy::SecretRegions {
                on_unmappable: OnUnmappable::WithholdImage,
            },
        )
        .unwrap();
        assert!(matches!(img, PersistableImage::Redacted(_)));
        assert_eq!(&img.rgba()[0..4], &[0, 0, 0, 255]);
        assert_eq!(&img.rgba()[4..8], &[9, 9, 9, 255]);
    }

    #[test]
    fn pixel_policy_off_is_explicit() {
        let raw = solid(1, 1, [4, 5, 6, 255]);
        let img = prepare_image(raw, &[], &[], PixelPolicy::Off).unwrap();
        assert!(matches!(img, PersistableImage::ExplicitlyUnredacted(_)));
    }

    #[test]
    fn borrowed_rgba_encoding_matches_rgb_for_color_alpha_and_odd_dimensions() {
        for (width, height) in [(1, 1), (17, 9), (64, 33)] {
            let rgba = (0..width * height)
                .flat_map(|i| [(i * 17) as u8, (i * 31) as u8, (i * 7) as u8, i as u8])
                .collect();
            let raw = RawImage::new(width, height, rgba).unwrap();
            let image = prepare_image(
                raw,
                &[],
                &[],
                PixelPolicy::SecretRegions {
                    on_unmappable: OnUnmappable::WithholdImage,
                },
            )
            .unwrap();
            let rgb: Vec<u8> = image
                .rgba()
                .chunks_exact(4)
                .flat_map(|px| px[..3].iter().copied())
                .collect();
            for quality in [1, 80, 100] {
                let mut expected = Vec::new();
                JpegEncoder::new_with_quality(&mut expected, quality)
                    .encode(&rgb, width, height, image::ExtendedColorType::Rgb8)
                    .unwrap();
                assert_eq!(encode_jpeg(&image, quality).unwrap(), expected);
            }
            assert!(encode_jpeg(&image, 0).is_err());
            assert!(encode_jpeg(&image, 101).is_err());
        }
    }
}
