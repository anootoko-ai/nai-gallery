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

/// Walk PNG chunks without decoding pixel data. Returns (width, height, tEXt map).
fn read_png_chunks(data: &[u8]) -> Option<(u32, u32, Vec<(String, String)>)> {
    const SIG: &[u8] = b"\x89PNG\r\n\x1a\n";
    if data.len() < 8 || &data[..8] != SIG {
        return None;
    }
    let (mut w, mut h) = (0u32, 0u32);
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
            b"IHDR" if len >= 8 => {
                w = u32::from_be_bytes(body[0..4].try_into().ok()?);
                h = u32::from_be_bytes(body[4..8].try_into().ok()?);
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
    Some((w, h, texts))
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
    let (width, height, texts) = read_png_chunks(data)?;
    let get = |k: &str| texts.iter().find(|(key, _)| key == k).map(|(_, v)| v.clone());

    let mut meta = NaiMetadata {
        width,
        height,
        is_novelai: get("Software").as_deref() == Some("NovelAI"),
        model: get("Source"),
        ..Default::default()
    };
    if !meta.is_novelai {
        return Some(meta);
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
    Some(meta)
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
}
