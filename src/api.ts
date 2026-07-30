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
}

export interface Query {
  include_tags: string[];
  exclude_tags: string[];
  text?: string;
  favorite?: boolean;
  min_rating?: number;
  folder_id?: number;
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
