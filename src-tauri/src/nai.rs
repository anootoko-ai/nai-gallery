//! NovelAI PNG metadata extraction and danbooru-style tag normalization.

use regex::Regex;
use serde::Serialize;
use std::sync::OnceLock;

#[derive(Debug, Clone, Serialize)]
pub struct Tag {
    pub name: String,
    pub category: String, // "general" | "artist" | "char"
    pub source: String,   // "base" | "char" | "negative"
}

#[derive(Debug, Default, Serialize)]
pub struct NaiMetadata {
    pub is_novelai: bool,
    pub width: u32,
    pub height: u32,
    pub model: Option<String>,
    pub seed: Option<i64>,
    pub sampler: Option<String>,
    pub steps: Option<i64>,
    pub scale: Option<f64>,
    pub raw_prompt: Option<String>,
    pub raw_negative: Option<String>,
    pub comment_json: Option<String>,
    pub tags: Vec<Tag>,
}

/// Walk PNG chunks without decoding pixel data.
/// Returns (width, height, IHDR color type, tEXt map).
fn read_png_chunks(data: &[u8]) -> Option<(u32, u32, u8, Vec<(String, String)>)> {
    const SIG: &[u8] = b"\x89PNG\r\n\x1a\n";
    if data.len() < 8 || &data[..8] != SIG {
        return None;
    }
    let (mut w, mut h) = (0u32, 0u32);
    let mut color_type = 0u8;
    let mut texts = Vec::new();
    let mut pos = 8usize;
    while pos + 8 <= data.len() {
        let len = u32::from_be_bytes(data[pos..pos + 4].try_into().ok()?) as usize;
        let ctype = &data[pos + 4..pos + 8];
        let body_start = pos + 8;
        let body_end = body_start.checked_add(len)?;
        if body_end + 4 > data.len() {
            break;
        }
        let body = &data[body_start..body_end];
        match ctype {
            b"IHDR" if len >= 10 => {
                w = u32::from_be_bytes(body[0..4].try_into().ok()?);
                h = u32::from_be_bytes(body[4..8].try_into().ok()?);
                color_type = body[9];
            }
            b"tEXt" => {
                if let Some(nul) = body.iter().position(|&b| b == 0) {
                    // tEXt is Latin-1; NAI payloads are ASCII/UTF-8-safe in practice,
                    // but decode lossily rather than reject
                    let key = String::from_utf8_lossy(&body[..nul]).into_owned();
                    let val = String::from_utf8_lossy(&body[nul + 1..]).into_owned();
                    texts.push((key, val));
                }
            }
            b"iTXt" => {
                // keyword\0 compressed\0 method\0 lang\0 translated\0 text
                if let Some(nul) = body.iter().position(|&b| b == 0) {
                    let key = String::from_utf8_lossy(&body[..nul]).into_owned();
                    let rest = &body[nul + 1..];
                    if rest.len() >= 2 && rest[0] == 0 {
                        // uncompressed only
                        let mut nuls = rest[2..].iter().enumerate().filter(|(_, &b)| b == 0);
                        if let (Some((l1, _)), Some((l2, _))) = (nuls.next(), nuls.next()) {
                            let text = &rest[2 + l2 + 1..];
                            let _ = l1;
                            texts.push((key, String::from_utf8_lossy(text).into_owned()));
                        }
                    }
                }
            }
            b"IEND" => break,
            _ => {}
        }
        pos = body_end + 4; // skip CRC
    }
    Some((w, h, color_type, texts))
}

fn weight_syntax_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\d+(?:\.\d+)?::").unwrap())
}

/// Normalize a NovelAI prompt fragment into danbooru-style tags.
/// Strips group-weight syntax (`0.5::tag a, tag b::`), splits on commas,
/// lowercases, and pulls `artist:` prefixes into the artist category.
pub fn normalize_tags(text: &str, source: &str) -> Vec<Tag> {
    let stripped = weight_syntax_re().replace_all(text, "");
    let stripped = stripped.replace("::", "");
    let mut out: Vec<Tag> = Vec::new();
    for token in stripped.split(',') {
        let token = token.split_whitespace().collect::<Vec<_>>().join(" ");
        let token = token.to_lowercase();
        if token.is_empty() {
            continue;
        }
        let (name, category) = if let Some(rest) = token.strip_prefix("artist:") {
            (rest.trim().to_string(), "artist")
        } else if source == "char" {
            (token, "char")
        } else {
            (token, "general")
        };
        if name.is_empty() || out.iter().any(|t| t.name == name && t.source == source) {
            continue;
        }
        out.push(Tag {
            name,
            category: category.to_string(),
            source: source.to_string(),
        });
    }
    out
}

/// Parse NovelAI metadata from raw PNG bytes. Non-NovelAI PNGs return
/// `is_novelai: false` with dimensions only, so they can still be indexed.
pub fn parse(data: &[u8]) -> Option<NaiMetadata> {
    let (width, height, color_type, texts) = read_png_chunks(data)?;
    let meta = from_texts(width, height, &texts);
    // tEXt chunks stripped (image editors, some transfer paths) but pixels
    // intact: NovelAI hides a copy of the metadata in the alpha LSBs.
    // IHDR color type 4/6 = has alpha; anything else can't carry it.
    if !meta.is_novelai && matches!(color_type, 4 | 6) {
        if let Some(stealth) = parse_stealth(data, width, height) {
            return Some(stealth);
        }
    }
    Some(meta)
}

/// Build metadata from a PNG's tEXt key/value pairs (or the equivalent
/// JSON object recovered from the stealth alpha channel).
fn from_texts(width: u32, height: u32, texts: &[(String, String)]) -> NaiMetadata {
    let get = |k: &str| texts.iter().find(|(key, _)| key == k).map(|(_, v)| v.clone());

    let mut meta = NaiMetadata {
        width,
        height,
        is_novelai: get("Software").as_deref() == Some("NovelAI"),
        model: get("Source"),
        ..Default::default()
    };
    if !meta.is_novelai {
        return meta;
    }

    let description = get("Description");
    if let Some(comment) = get("Comment") {
        if let Ok(c) = serde_json::from_str::<serde_json::Value>(&comment) {
            meta.seed = c.get("seed").and_then(|v| v.as_i64());
            meta.sampler = c.get("sampler").and_then(|v| v.as_str()).map(String::from);
            meta.steps = c.get("steps").and_then(|v| v.as_i64());
            meta.scale = c.get("scale").and_then(|v| v.as_f64());

            // v4+: structured prompt with per-character captions
            let v4 = c.pointer("/v4_prompt/caption");
            let base_prompt = v4
                .and_then(|cap| cap.get("base_caption"))
                .and_then(|v| v.as_str())
                .map(String::from)
                .or_else(|| c.get("prompt").and_then(|v| v.as_str()).map(String::from))
                .or_else(|| description.clone());

            if let Some(ref p) = base_prompt {
                meta.tags.extend(normalize_tags(p, "base"));
            }
            if let Some(chars) = v4.and_then(|cap| cap.get("char_captions")).and_then(|v| v.as_array()) {
                for cc in chars {
                    if let Some(cap) = cc.get("char_caption").and_then(|v| v.as_str()) {
                        meta.tags.extend(normalize_tags(cap, "char"));
                    }
                }
            }

            let negative = c
                .pointer("/v4_negative_prompt/caption/base_caption")
                .and_then(|v| v.as_str())
                .map(String::from)
                .or_else(|| c.get("uc").and_then(|v| v.as_str()).map(String::from))
                .or_else(|| c.get("negative_prompt").and_then(|v| v.as_str()).map(String::from));

            meta.raw_prompt = base_prompt;
            meta.raw_negative = negative;
            meta.comment_json = Some(comment);
        }
    } else if let Some(ref d) = description {
        // v1-era files: prompt only lives in Description
        meta.tags.extend(normalize_tags(d, "base"));
        meta.raw_prompt = description.clone();
    }
    meta
}

// ── stealth alpha-channel metadata (phase 3) ─────────────────
//
// Format (NovelAI/novelai-image-metadata, `LSBExtractor`): the LSB of each
// alpha byte, iterated column-major (all of column x=0 top to bottom, then
// x=1, …), packed into bytes MSB-first. Stream layout:
//   15-byte ASCII magic  — "stealth_pngcomp" (gzip) / "stealth_pnginfo" (raw)
//   32-bit BE payload length in BITS
//   payload — JSON object mirroring the tEXt chunks
//     ("Software", "Source", "Description", "Comment", …)

/// MSB-first byte reader over the alpha-channel LSB bit stream.
struct AlphaLsbReader<'a> {
    img: &'a image::RgbaImage,
    bit: u64,
}

impl AlphaLsbReader<'_> {
    fn read_bytes(&mut self, n: usize) -> Option<Vec<u8>> {
        let (w, h) = self.img.dimensions();
        let total_bits = w as u64 * h as u64;
        if self.bit + n as u64 * 8 > total_bits {
            return None;
        }
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            let mut byte = 0u8;
            for _ in 0..8 {
                // column-major: bit index = x * height + y
                let x = (self.bit / h as u64) as u32;
                let y = (self.bit % h as u64) as u32;
                byte = (byte << 1) | (self.img.get_pixel(x, y)[3] & 1);
                self.bit += 1;
            }
            out.push(byte);
        }
        Some(out)
    }
}

/// Recover metadata from the alpha-channel LSBs of a PNG whose tEXt chunks
/// were stripped. Full pixel decode — only worth calling when the cheap
/// tEXt path found nothing and the IHDR says there is an alpha channel.
fn parse_stealth(data: &[u8], width: u32, height: u32) -> Option<NaiMetadata> {
    use std::io::Read;

    let img = match image::load_from_memory(data).ok()? {
        // 16-bit images get truncated to 8 here, which would destroy the
        // LSB stream anyway; NAI only ever writes 8-bit RGBA
        image::DynamicImage::ImageRgba8(img) => img,
        _ => return None,
    };
    let mut reader = AlphaLsbReader { img: &img, bit: 0 };
    let compressed = match reader.read_bytes(15)?.as_slice() {
        b"stealth_pngcomp" => true,
        b"stealth_pnginfo" => false,
        _ => return None,
    };
    let bit_len = u32::from_be_bytes(reader.read_bytes(4)?.try_into().ok()?) as u64;
    if bit_len == 0 || bit_len % 8 != 0 {
        return None;
    }
    let payload = reader.read_bytes((bit_len / 8) as usize)?;
    let json_text = if compressed {
        let mut out = String::new();
        flate2::read::GzDecoder::new(payload.as_slice())
            .read_to_string(&mut out)
            .ok()?;
        out
    } else {
        String::from_utf8(payload).ok()?
    };

    // payload mirrors the tEXt chunks as a JSON object; reuse the tEXt path
    let value: serde_json::Value = serde_json::from_str(&json_text).ok()?;
    let texts: Vec<(String, String)> = value
        .as_object()?
        .iter()
        .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_owned())))
        .collect();
    let meta = from_texts(width, height, &texts);
    meta.is_novelai.then_some(meta)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_group_weights() {
        let tags = normalize_tags(
            "1girl, 1::artist: alpha painter, 0.5::artist: beta_sketch,:: 0.8::artist: gamma 9x,::artist: delta_box,0.7::artist: epsilon::, year 2025,::     1::expert shading::,  upper torso",
            "base",
        );
        let names: Vec<&str> = tags.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "1girl", "alpha painter", "beta_sketch", "gamma 9x", "delta_box",
                "epsilon", "year 2025", "expert shading", "upper torso"
            ]
        );
        assert_eq!(tags[1].category, "artist");
        assert_eq!(tags[6].category, "general");
    }

    #[test]
    fn char_tags_categorized() {
        let tags = normalize_tags("girl, female knight, blonde, armor", "char");
        assert!(tags.iter().all(|t| t.category == "char"));
        assert_eq!(tags.len(), 4);
    }

    /// Embed a stealth stream exactly the way NovelAI's reference encoder
    /// does (column-major alpha LSBs, MSB-first bytes), then check that a
    /// PNG with no tEXt chunks at all still yields full metadata.
    #[test]
    fn stealth_metadata_roundtrip() {
        use std::io::Write;

        let payload = serde_json::json!({
            "Software": "NovelAI",
            "Source": "Stable Diffusion XL C1E1DE52",
            "Description": "1girl, smile",
            "Comment": r#"{"prompt":"1girl, smile","uc":"lowres, bad hands","seed":42,"sampler":"k_euler","steps":28,"scale":5.0}"#,
        })
        .to_string();
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(payload.as_bytes()).unwrap();
        let gz = gz.finish().unwrap();

        let mut stream = Vec::new();
        stream.extend_from_slice(b"stealth_pngcomp");
        stream.extend_from_slice(&((gz.len() as u32) * 8).to_be_bytes());
        stream.extend_from_slice(&gz);

        let mut img = image::RgbaImage::from_pixel(64, 64, image::Rgba([120, 130, 140, 255]));
        let height = img.height();
        assert!((stream.len() as u32) * 8 <= img.width() * height, "test image too small");
        for (i, byte) in stream.iter().enumerate() {
            for bit in 0..8 {
                let idx = (i * 8 + bit) as u32;
                let (x, y) = (idx / height, idx % height);
                let px = img.get_pixel_mut(x, y);
                px[3] = (px[3] & !1) | ((byte >> (7 - bit)) & 1);
            }
        }
        let mut buf = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut buf, image::ImageFormat::Png)
            .unwrap();

        let meta = parse(&buf.into_inner()).expect("valid png");
        assert!(meta.is_novelai);
        assert_eq!(meta.seed, Some(42));
        assert_eq!(meta.raw_prompt.as_deref(), Some("1girl, smile"));
        assert_eq!(meta.raw_negative.as_deref(), Some("lowres, bad hands"));
        assert_eq!(meta.sampler.as_deref(), Some("k_euler"));
        assert!(meta.tags.iter().any(|t| t.name == "smile"));
    }

    #[test]
    fn plain_rgba_png_without_stealth_stays_non_nai() {
        let img = image::RgbaImage::from_pixel(16, 16, image::Rgba([1, 2, 3, 255]));
        let mut buf = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut buf, image::ImageFormat::Png)
            .unwrap();
        let meta = parse(&buf.into_inner()).expect("valid png");
        assert!(!meta.is_novelai);
        assert_eq!((meta.width, meta.height), (16, 16));
    }
}
