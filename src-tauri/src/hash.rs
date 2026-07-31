//! Perceptual hashing for near-duplicate detection.
//!
//! dHash (difference hash) over a 9x8 grayscale downsample: each bit
//! records whether a pixel is brighter than its right neighbor. Chosen
//! over DCT pHash because it is a fraction of the code, insensitive to
//! re-encoding/resizing (our duplicate sources: re-downloads, format
//! shuffling, minor edits), and the extra DCT robustness only matters
//! for crops/rotations, which NAI gen batches don't produce.

/// 64-bit difference hash. Input can be any size; typically fed the
/// cached 512px thumbnail since the 9x8 downsample discards detail anyway.
pub fn dhash(img: &image::DynamicImage) -> i64 {
    let small = img
        .resize_exact(9, 8, image::imageops::FilterType::Triangle)
        .into_luma8();
    let mut bits: u64 = 0;
    for y in 0..8 {
        for x in 0..8 {
            bits <<= 1;
            if small.get_pixel(x, y)[0] < small.get_pixel(x + 1, y)[0] {
                bits |= 1;
            }
        }
    }
    bits as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pattern(w: u32, h: u32, f: impl Fn(u32, u32) -> u8) -> image::DynamicImage {
        image::DynamicImage::ImageLuma8(image::GrayImage::from_fn(w, h, |x, y| {
            image::Luma([f(x, y)])
        }))
    }

    fn distance(a: i64, b: i64) -> u32 {
        (a ^ b).count_ones()
    }

    #[test]
    fn dhash_survives_resize_and_reencode() {
        let base = pattern(256, 256, |x, y| ((x * 13 + y * 7) % 256) as u8);
        let hash = dhash(&base);

        // same content at thumbnail size hashes identically or nearly so
        let small = base.resize_exact(128, 128, image::imageops::FilterType::Triangle);
        assert!(distance(hash, dhash(&small)) <= 4);

        // jpeg round-trip (thumbnail cache) stays close
        let mut buf = std::io::Cursor::new(Vec::new());
        base.to_rgb8()
            .write_to(&mut buf, image::ImageFormat::Jpeg)
            .unwrap();
        let rejpg = image::load_from_memory(&buf.into_inner()).unwrap();
        assert!(distance(hash, dhash(&rejpg)) <= 4);

        // unrelated pattern lands far away
        let other = pattern(256, 256, |x, y| ((x * 5 + y * 27 + 91) % 256) as u8);
        assert!(distance(hash, dhash(&other)) > 12);
    }
}
