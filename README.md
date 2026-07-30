# NAI Gallery

A local Windows desktop app for browsing, searching, and curating large collections of
NovelAI-generated images using the generation metadata already embedded in the PNGs.

Point it at the folders where your images land and it builds a searchable index —
**your files are never moved, renamed, or modified**.

## Features

- **Index in place** — watches your existing folders (e.g. date-organized downloads);
  new images appear automatically
- **Real danbooru-style tag search** — NovelAI prompts are normalized into a tag
  database (weight syntax like `0.7::artist: name::` stripped), with autocomplete,
  AND filtering, and `-tag` exclusion — not just full-text prompt matching
- **NovelAI v1 → v4.5 metadata** — including v4's structured per-character
  `char_captions`, shown as a separate tag group per image
- **Fast masonry grid** with infinite scroll and adjustable thumbnail size
- **Fullscreen viewer** — arrow-key navigation, `i` for metadata overlay
- **Curation** — favorites (`f`), 0–5 star ratings (number keys), copy prompt/seed,
  show in Explorer
- **Auto-updates** via signed GitHub releases

## Install

Download the latest installer from [Releases](../../releases/latest).
The app checks for updates on startup and offers one-click install.

## Development

Prerequisites: Node 20+, Rust stable (MSVC).

```
npm install
npm run tauri dev
```

The library database and thumbnail cache live in `%APPDATA%\com.pc.nai-gallery`.

## How it reads metadata

NovelAI PNGs carry tEXt chunks (`Software`, `Source`, `Description`, and a `Comment`
JSON payload with prompt, negative prompt, seed, sampler, steps, and — for v4+ —
structured `v4_prompt` captions). The indexer walks PNG chunks without decoding
pixels, parses whichever era's format it finds, and normalizes prompts into
categorized tags (artist / character / general).

## Roadmap

- Albums, saved searches, reject-review workflow
- Near-duplicate detection (perceptual hash)
- Stealth alpha-channel metadata recovery for stripped files
- Tag analytics; A1111 / ComfyUI metadata parsers

## License

MIT
