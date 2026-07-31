import { invoke, convertFileSrc } from "@tauri-apps/api/core";

export interface ImageCard {
  id: number;
  width: number;
  height: number;
  seed: number | null;
  rating: number;
  favorite: boolean;
}

export interface QueryResult {
  total: number;
  cards: ImageCard[];
}

export interface TagSuggestion {
  name: string;
  category: string;
  count: number;
}

export interface TagDetail {
  name: string;
  category: string;
  source: string;
  hidden: boolean;
}

export interface ImageDetail {
  id: number;
  path: string;
  file_name: string;
  width: number;
  height: number;
  file_mtime: number;
  is_novelai: boolean;
  model: string | null;
  seed: number | null;
  sampler: string | null;
  steps: number | null;
  scale: number | null;
  raw_prompt: string | null;
  raw_negative: string | null;
  rating: number;
  favorite: boolean;
  tags: TagDetail[];
}

export interface FolderInfo {
  id: number;
  path: string;
  image_count: number;
}

export interface LibraryStats {
  total: number;
  novelai: number;
  favorites: number;
  rejects: number;
}

export interface AlbumInfo {
  id: number;
  name: string;
  image_count: number;
}

export interface SavedSearch {
  id: number;
  name: string;
  query_json: string;
}

export interface Query {
  include_tags: string[];
  exclude_tags: string[];
  text?: string;
  favorite?: boolean;
  min_rating?: number;
  folder_id?: number;
  /** absolute directory prefix INCLUDING trailing separator, e.g. `D:\gallery\2026-07\` */
  path_prefix?: string;
  album_id?: number;
  /** true = only images in at least one album; false = only images in no album */
  in_album?: boolean;
  rejects?: boolean;
  sort: string;
  offset: number;
  limit: number;
}

export const thumbUrl = (id: number) => convertFileSrc(String(id), "thumb");
export const origUrl = (id: number) => convertFileSrc(String(id), "orig");

export const queryImages = (query: Partial<Query>) =>
  invoke<QueryResult>("query_images", { query });
export const suggestTags = (prefix: string) =>
  invoke<TagSuggestion[]>("suggest_tags", { prefix });
export const topTags = () => invoke<TagSuggestion[]>("top_tags");
export const getImage = (id: number) => invoke<ImageDetail>("get_image", { id });
export const setRating = (id: number, rating: number) =>
  invoke<void>("set_rating", { id, rating });
export const setFavorite = (id: number, favorite: boolean) =>
  invoke<void>("set_favorite", { id, favorite });
export const listFolders = () => invoke<FolderInfo[]>("list_folders");
export const getStats = () => invoke<LibraryStats>("stats");
export const addFolder = (path: string) => invoke<number>("add_folder", { path });
export const removeFolder = (id: number) => invoke<void>("remove_folder", { id });
export const rescanAll = () => invoke<void>("rescan_all");
export const openInExplorer = (path: string) =>
  invoke<void>("open_in_explorer", { path });

// ── phase 2 ──
export const setHiddenBulk = (ids: number[], hidden: boolean) =>
  invoke<void>("set_hidden_bulk", { ids, hidden });
export const setFavoriteBulk = (ids: number[], favorite: boolean) =>
  invoke<void>("set_favorite_bulk", { ids, favorite });
export const setRatingBulk = (ids: number[], rating: number) =>
  invoke<void>("set_rating_bulk", { ids, rating });
export const listAlbums = () => invoke<AlbumInfo[]>("list_albums");
export const createAlbum = (name: string) => invoke<number>("create_album", { name });
export const deleteAlbum = (id: number) => invoke<void>("delete_album", { id });
export const addToAlbum = (albumId: number, ids: number[]) =>
  invoke<void>("add_to_album", { albumId, ids });
export const removeFromAlbum = (albumId: number, ids: number[]) =>
  invoke<void>("remove_from_album", { albumId, ids });
export const listSavedSearches = () => invoke<SavedSearch[]>("list_saved_searches");
export const createSavedSearch = (name: string, queryJson: string) =>
  invoke<number>("create_saved_search", { name, queryJson });
export const deleteSavedSearch = (id: number) =>
  invoke<void>("delete_saved_search", { id });
export const trashImages = (ids: number[]) => invoke<number>("trash_images", { ids });

// ── phase 2.5 ──
export interface DirEntry {
  folder_id: number;
  /** directory relative to the watched root, "" for the root itself */
  rel_dir: string;
  /** images directly in this directory (not descendants) */
  count: number;
}

/** Adds tag(s) with source='user'; input is comma-splittable and normalized
 *  like a prompt. Returns the normalized tag names actually added. */
export const addUserTag = (ids: number[], name: string) =>
  invoke<string[]>("add_user_tag", { ids, name });
export const removeUserTag = (ids: number[], name: string) =>
  invoke<void>("remove_user_tag", { ids, name });
export const setTagHidden = (name: string, hidden: boolean) =>
  invoke<void>("set_tag_hidden", { name, hidden });
export const listHiddenTags = () => invoke<TagSuggestion[]>("list_hidden_tags");
export const folderTree = () => invoke<DirEntry[]>("folder_tree");

// ── phase 3 ──
export interface DupImage {
  id: number;
  path: string;
  file_name: string;
  width: number;
  height: number;
  file_size: number;
  file_mtime: number;
  seed: number | null;
  rating: number;
  favorite: boolean;
  /** hamming distance to the closest other member of the group (0 = visual twin) */
  distance: number;
}

export interface DupResult {
  /** visible images not hashed yet (thumbnails still generating); nonzero
   *  means groups may be incomplete — offer a refresh */
  unhashed: number;
  /** each group sorted likely-keeper-first (favorite, rating, file size);
   *  groups sorted newest first */
  groups: DupImage[][];
}

/** maxDistance: 0 = exact/re-encoded twins, ~6 = near-duplicates (default
 *  suggestion), 10+ = loose. Emits no events; safe to re-run freely. */
export const findDuplicates = (maxDistance: number) =>
  invoke<DupResult>("find_duplicates", { maxDistance });

export interface TagPair {
  a: string;
  b: string;
  /** visible images carrying both tags */
  count: number;
}

export interface DayCount {
  /** local calendar day, ISO `YYYY-MM-DD` */
  day: string;
  count: number;
}

/** Top tags by visible-image count; hidden tags and negative-prompt rows
 *  are excluded, as in autocomplete. */
export const tagFrequency = (limit: number) =>
  invoke<TagSuggestion[]>("tag_frequency", { limit });
/** Top tag pairs sharing an image, computed over the most-used tags only. */
export const tagCooccurrence = (limit: number) =>
  invoke<TagPair[]>("tag_cooccurrence", { limit });
/** Visible images per local calendar day; only days that have images come
 *  back — the caller fills the gaps. */
export const imagesPerDay = (days: number) =>
  invoke<DayCount[]>("images_per_day", { days });
