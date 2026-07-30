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
  album_id?: number;
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
