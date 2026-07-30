import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { open as openDialog, confirm as confirmDialog } from "@tauri-apps/plugin-dialog";
import { check, type Update } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";
import * as api from "./api";
import "./App.css";

interface Filter {
  tag: string;
  neg: boolean;
}

/// What the grid is showing. One value instead of parallel view/folder/album
/// states that would have to be reset in lockstep.
type Scope =
  | { kind: "all" }
  | { kind: "favorites" }
  | { kind: "rejects" }
  | { kind: "folder"; id: number }
  | { kind: "album"; id: number };

interface SavedQuery {
  filters: Filter[];
  scope: Scope;
  sort: string;
}

interface NamePrompt {
  title: string;
  placeholder?: string;
  initial?: string;
  onSubmit: (value: string) => void;
}

interface Mods {
  shift: boolean;
  ctrl: boolean;
}

const PAGE = 200;

const scopeId = (s: Scope) => ("id" in s ? s.id : null);
const sameScope = (a: Scope, b: Scope) => a.kind === b.kind && scopeId(a) === scopeId(b);

export default function App() {
  // ── query state ──
  const [filters, setFilters] = useState<Filter[]>([]);
  const [scope, setScope] = useState<Scope>({ kind: "all" });
  const [sort, setSort] = useState("newest");
  // ── data state ──
  const [cards, setCards] = useState<api.ImageCard[]>([]);
  const [total, setTotal] = useState(0);
  const [folders, setFolders] = useState<api.FolderInfo[]>([]);
  const [albums, setAlbums] = useState<api.AlbumInfo[]>([]);
  const [searches, setSearches] = useState<api.SavedSearch[]>([]);
  const [tops, setTops] = useState<api.TagSuggestion[]>([]);
  const [stats, setStats] = useState<api.LibraryStats | null>(null);
  const [scanMsg, setScanMsg] = useState<string | null>(null);
  // ── selection / viewer ──
  const [sel, setSel] = useState<Set<number>>(new Set());
  const [primaryId, setPrimaryId] = useState<number | null>(null);
  const [detail, setDetail] = useState<api.ImageDetail | null>(null);
  const [viewerOpen, setViewerOpen] = useState(false);
  const [thumbWidth, setThumbWidth] = useState(210);
  const [namePrompt, setNamePrompt] = useState<NamePrompt | null>(null);
  const [update, setUpdate] = useState<Update | null>(null);
  const [updating, setUpdating] = useState(false);

  const anchorRef = useRef<number | null>(null);
  const queryRef = useRef(0);

  const selIds = useMemo(() => [...sel], [sel]);

  const buildQuery = useCallback(
    (offset: number): Partial<api.Query> => ({
      include_tags: filters.filter((f) => !f.neg).map((f) => f.tag),
      exclude_tags: filters.filter((f) => f.neg).map((f) => f.tag),
      favorite: scope.kind === "favorites" ? true : undefined,
      folder_id: scope.kind === "folder" ? scope.id : undefined,
      album_id: scope.kind === "album" ? scope.id : undefined,
      rejects: scope.kind === "rejects",
      sort,
      offset,
      limit: PAGE,
    }),
    [filters, scope, sort]
  );

  const refreshMeta = useCallback(() => {
    api.listFolders().then(setFolders).catch(() => {});
    api.listAlbums().then(setAlbums).catch(() => {});
    api.listSavedSearches().then(setSearches).catch(() => {});
    api.topTags().then(setTops).catch(() => {});
    api.getStats().then(setStats).catch(() => {});
  }, []);

  const runQuery = useCallback(() => {
    const token = ++queryRef.current;
    api.queryImages(buildQuery(0)).then((res) => {
      if (queryRef.current !== token) return;
      setCards(res.cards);
      setTotal(res.total);
    });
  }, [buildQuery]);

  const loadMore = useCallback(() => {
    if (cards.length >= total) return;
    const token = queryRef.current;
    api.queryImages(buildQuery(cards.length)).then((res) => {
      if (queryRef.current !== token) return;
      setCards((prev) => [...prev, ...res.cards]);
      setTotal(res.total);
    });
  }, [buildQuery, cards.length, total]);

  useEffect(runQuery, [runQuery]);
  useEffect(refreshMeta, [refreshMeta]);

  // a new result set invalidates the old selection
  const clearSel = useCallback(() => {
    setSel(new Set());
    setPrimaryId(null);
    anchorRef.current = null;
  }, []);
  useEffect(clearSel, [scope, filters, sort, clearSel]);

  // scan events
  useEffect(() => {
    const un1 = listen<{ done: number; total: number; folder: string }>(
      "scan:progress",
      (e) => setScanMsg(`scanning ${e.payload.done}/${e.payload.total}`)
    );
    const un2 = listen<number>("scan:done", () => {
      setScanMsg(null);
      runQuery();
      refreshMeta();
    });
    const un3 = listen<string>("scan:error", (e) => setScanMsg(`scan error: ${e.payload}`));
    return () => {
      un1.then((f) => f());
      un2.then((f) => f());
      un3.then((f) => f());
    };
  }, [runQuery, refreshMeta]);

  // update check on startup (no-op in dev builds / offline)
  useEffect(() => {
    check()
      .then((u) => u && setUpdate(u))
      .catch(() => {});
  }, []);

  const installUpdate = useCallback(async () => {
    if (!update || updating) return;
    setUpdating(true);
    try {
      await update.downloadAndInstall();
      await relaunch();
    } catch {
      setUpdating(false);
    }
  }, [update, updating]);

  // inspector detail follows the primary (last-clicked) image
  useEffect(() => {
    if (primaryId == null) {
      setDetail(null);
      return;
    }
    api.getImage(primaryId).then(setDetail).catch(() => setDetail(null));
  }, [primaryId]);

  // ── selection ──
  const selectCard = useCallback(
    (id: number, mods: Mods) => {
      if (mods.shift && anchorRef.current != null) {
        const a = cards.findIndex((c) => c.id === anchorRef.current);
        const b = cards.findIndex((c) => c.id === id);
        if (a >= 0 && b >= 0) {
          const [lo, hi] = a < b ? [a, b] : [b, a];
          setSel(new Set(cards.slice(lo, hi + 1).map((c) => c.id)));
          setPrimaryId(id);
          return;
        }
      }
      if (mods.ctrl) {
        setSel((prev) => {
          const next = new Set(prev);
          if (next.has(id)) next.delete(id);
          else next.add(id);
          return next;
        });
      } else {
        setSel(new Set([id]));
      }
      anchorRef.current = id;
      setPrimaryId(id);
    },
    [cards]
  );

  /// Drop rows that no longer belong to the current result set. Offset-based
  /// paging stays correct: the removed rows are gone from the query too, so
  /// the next page still starts at cards.length.
  const dropCards = useCallback(
    (ids: number[]) => {
      const gone = new Set(ids);
      // count against what's actually on screen — a background rescan can
      // replace the grid while a selection is still held
      const removed = cards.reduce((n, c) => (gone.has(c.id) ? n + 1 : n), 0);
      setCards((cs) => cs.filter((c) => !gone.has(c.id)));
      setTotal((t) => Math.max(0, t - removed));
      setPrimaryId((p) => (p != null && gone.has(p) ? null : p));
      setSel(new Set());
      anchorRef.current = null;
    },
    [cards]
  );

  const patchCards = useCallback((ids: number[], patch: Partial<api.ImageCard>) => {
    const hit = new Set(ids);
    setCards((cs) => cs.map((c) => (hit.has(c.id) ? { ...c, ...patch } : c)));
    setDetail((d) => (d && hit.has(d.id) ? { ...d, ...patch } : d));
  }, []);

  // ── actions ──
  const addFilter = useCallback((tag: string, neg: boolean) => {
    setFilters((f) =>
      f.some((x) => x.tag === tag && x.neg === neg) ? f : [...f, { tag, neg }]
    );
  }, []);
  const removeFilter = (i: number) => setFilters((f) => f.filter((_, j) => j !== i));

  const favorite = useCallback(
    async (ids: number[], value: boolean) => {
      if (!ids.length) return;
      await api.setFavoriteBulk(ids, value);
      // un-favoriting inside the Favorites view removes those images from it
      if (scope.kind === "favorites" && !value) dropCards(ids);
      else patchCards(ids, { favorite: value });
      api.getStats().then(setStats).catch(() => {});
    },
    [scope, dropCards, patchCards]
  );

  const rate = useCallback(
    async (ids: number[], rating: number) => {
      if (!ids.length) return;
      await api.setRatingBulk(ids, rating);
      patchCards(ids, { rating });
    },
    [patchCards]
  );

  const setHidden = useCallback(
    async (ids: number[], hidden: boolean) => {
      if (!ids.length) return;
      await api.setHiddenBulk(ids, hidden);
      dropCards(ids); // leaves the current view either way (reject or restore)
      api.getStats().then(setStats).catch(() => {});
    },
    [dropCards]
  );

  const trashSelection = useCallback(async () => {
    const ids = selIds;
    if (!ids.length) return;
    const ok = await confirmDialog(
      `Move ${ids.length} file${ids.length === 1 ? "" : "s"} to the Recycle Bin?\n\n` +
        `They leave the library and can be restored from the Windows Recycle Bin.`,
      { title: "Move to Recycle Bin", kind: "warning", okLabel: "Move to Recycle Bin" }
    );
    if (!ok) return;
    await api.trashImages(ids);
    dropCards(ids);
    refreshMeta();
  }, [selIds, dropCards, refreshMeta]);

  const addSelToAlbum = useCallback(
    async (albumId: number) => {
      if (!selIds.length) return;
      await api.addToAlbum(albumId, selIds);
      api.listAlbums().then(setAlbums).catch(() => {});
      if (scope.kind === "album" && scope.id === albumId) runQuery();
    },
    [selIds, scope, runQuery]
  );

  const removeSelFromAlbum = useCallback(async () => {
    if (scope.kind !== "album" || !selIds.length) return;
    await api.removeFromAlbum(scope.id, selIds);
    dropCards(selIds);
    api.listAlbums().then(setAlbums).catch(() => {});
  }, [scope, selIds, dropCards]);

  const promptNewAlbum = useCallback(
    (withSelection: boolean) => {
      const ids = withSelection ? selIds : [];
      setNamePrompt({
        title: ids.length ? `New album with ${ids.length} image${ids.length === 1 ? "" : "s"}` : "New album",
        placeholder: "Album name",
        onSubmit: async (name) => {
          const id = await api.createAlbum(name);
          if (ids.length) await api.addToAlbum(id, ids);
          api.listAlbums().then(setAlbums).catch(() => {});
        },
      });
    },
    [selIds]
  );

  const deleteAlbum = useCallback(
    async (a: api.AlbumInfo) => {
      const ok = await confirmDialog(
        `Delete the album “${a.name}”?\n\nThe ${a.image_count} image${
          a.image_count === 1 ? "" : "s"
        } stay in your library — only the grouping is removed.`,
        { title: "Delete album", kind: "warning", okLabel: "Delete album" }
      );
      if (!ok) return;
      await api.deleteAlbum(a.id);
      setScope((s) => (s.kind === "album" && s.id === a.id ? { kind: "all" } : s));
      api.listAlbums().then(setAlbums).catch(() => {});
    },
    []
  );

  const saveCurrentSearch = useCallback(() => {
    const payload: SavedQuery = { filters, scope, sort };
    setNamePrompt({
      title: "Save current search",
      placeholder: "Name",
      initial: filters.map((f) => (f.neg ? "-" : "") + f.tag).join(" "),
      onSubmit: async (name) => {
        await api.createSavedSearch(name, JSON.stringify(payload));
        api.listSavedSearches().then(setSearches).catch(() => {});
      },
    });
  }, [filters, scope, sort]);

  const applySearch = useCallback((s: api.SavedSearch) => {
    let q: Partial<SavedQuery>;
    try {
      q = JSON.parse(s.query_json);
    } catch {
      return; // unreadable entry — leave the current view alone
    }
    setFilters(Array.isArray(q.filters) ? q.filters : []);
    setScope(q.scope && typeof q.scope.kind === "string" ? q.scope : { kind: "all" });
    setSort(typeof q.sort === "string" ? q.sort : "newest");
  }, []);

  const pickFolder = async () => {
    const dir = await openDialog({ directory: true, multiple: false });
    if (typeof dir === "string") {
      await api.addFolder(dir);
      refreshMeta();
    }
  };

  // ── keyboard ──
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const tag = (e.target as HTMLElement | null)?.tagName;
      if (tag === "INPUT" || tag === "TEXTAREA") return;
      if (namePrompt) return;

      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "a") {
        e.preventDefault();
        setSel(new Set(cards.map((c) => c.id)));
        return;
      }
      // targets: the whole selection when there is one, else the primary
      const targets = sel.size > 0 ? [...sel] : primaryId != null ? [primaryId] : [];
      const idx = cards.findIndex((c) => c.id === primaryId);

      if (e.key === "ArrowRight" || e.key === "ArrowLeft") {
        e.preventDefault();
        const next = idx + (e.key === "ArrowRight" ? 1 : -1);
        if (next >= 0 && next < cards.length) {
          const id = cards[next].id;
          setPrimaryId(id);
          if (e.shiftKey && anchorRef.current != null) {
            const a = cards.findIndex((c) => c.id === anchorRef.current);
            const [lo, hi] = a < next ? [a, next] : [next, a];
            setSel(new Set(cards.slice(lo, hi + 1).map((c) => c.id)));
          } else {
            setSel(new Set([id]));
            anchorRef.current = id;
          }
        }
        if (next >= cards.length - 10) loadMore();
      } else if (e.key === "Escape") {
        if (viewerOpen) setViewerOpen(false);
        else clearSel();
      } else if (e.key === "Enter" && primaryId != null) {
        setViewerOpen(true);
      } else if (e.key.toLowerCase() === "f" && targets.length && detail) {
        // the primary's current state decides the direction for the whole batch
        favorite(targets, !detail.favorite);
      } else if (/^[0-5]$/.test(e.key) && targets.length) {
        rate(targets, Number(e.key));
      } else if (e.key === "Delete" && targets.length && scope.kind !== "rejects") {
        // rejecting is reversible; leaving Rejects requires the explicit buttons
        e.preventDefault();
        setHidden(targets, true);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [
    cards,
    sel,
    primaryId,
    detail,
    viewerOpen,
    namePrompt,
    scope,
    loadMore,
    favorite,
    rate,
    setHidden,
    clearSel,
  ]);

  return (
    <div className="app">
      <TopBar
        filters={filters}
        onAdd={addFilter}
        onRemove={removeFilter}
        sort={sort}
        setSort={setSort}
        thumbWidth={thumbWidth}
        setThumbWidth={setThumbWidth}
      />
      <Sidebar
        scope={scope}
        setScope={setScope}
        stats={stats}
        folders={folders}
        albums={albums}
        searches={searches}
        tops={tops}
        onTag={(t) => addFilter(t, false)}
        onAddFolder={pickFolder}
        onRemoveFolder={async (id) => {
          await api.removeFolder(id);
          setScope((s) => (s.kind === "folder" && s.id === id ? { kind: "all" } : s));
          refreshMeta();
          runQuery();
        }}
        onNewAlbum={() => promptNewAlbum(false)}
        onDeleteAlbum={deleteAlbum}
        onSaveSearch={saveCurrentSearch}
        onApplySearch={applySearch}
        onDeleteSearch={async (id) => {
          await api.deleteSavedSearch(id);
          api.listSavedSearches().then(setSearches).catch(() => {});
        }}
      />
      <Grid
        cards={cards}
        total={total}
        filters={filters}
        scope={scope}
        sel={sel}
        primaryId={primaryId}
        onSelect={selectCard}
        onOpen={(id) => {
          setSel(new Set([id]));
          setPrimaryId(id);
          anchorRef.current = id;
          setViewerOpen(true);
        }}
        onFav={(id, current) => favorite([id], !current)}
        onLoadMore={loadMore}
        thumbWidth={thumbWidth}
      />
      <Inspector
        detail={detail}
        onTag={(t) => addFilter(t, false)}
        onFav={(id, current) => favorite([id], !current)}
        onRate={(id, n) => rate([id], n)}
      />
      <StatusBar
        stats={stats}
        scanMsg={scanMsg}
        onRescan={() => api.rescanAll()}
        update={update}
        updating={updating}
        onInstallUpdate={installUpdate}
      />
      {sel.size > 0 && (
        <BulkBar
          count={sel.size}
          scope={scope}
          albums={albums}
          onClear={clearSel}
          onFav={(v) => favorite(selIds, v)}
          onRate={(n) => rate(selIds, n)}
          onReject={() => setHidden(selIds, true)}
          onRestore={() => setHidden(selIds, false)}
          onTrash={trashSelection}
          onAddToAlbum={addSelToAlbum}
          onNewAlbum={() => promptNewAlbum(true)}
          onRemoveFromAlbum={removeSelFromAlbum}
        />
      )}
      {viewerOpen && detail && (
        <Viewer
          detail={detail}
          onClose={() => setViewerOpen(false)}
          onNav={(dir) => {
            const idx = cards.findIndex((c) => c.id === primaryId);
            const next = idx + dir;
            if (next >= 0 && next < cards.length) {
              const id = cards[next].id;
              setPrimaryId(id);
              setSel(new Set([id]));
              anchorRef.current = id;
            }
            if (next >= cards.length - 10) loadMore();
          }}
          onFav={(id, current) => favorite([id], !current)}
          onRate={(id, n) => rate([id], n)}
        />
      )}
      {namePrompt && (
        <NameModal prompt={namePrompt} onClose={() => setNamePrompt(null)} />
      )}
    </div>
  );
}

// ── TopBar ───────────────────────────────────────────────────

function TopBar(props: {
  filters: Filter[];
  onAdd: (tag: string, neg: boolean) => void;
  onRemove: (i: number) => void;
  sort: string;
  setSort: (s: string) => void;
  thumbWidth: number;
  setThumbWidth: (n: number) => void;
}) {
  const [q, setQ] = useState("");
  const [sugs, setSugs] = useState<api.TagSuggestion[]>([]);
  const [open, setOpen] = useState(false);

  useEffect(() => {
    const v = q.trim().replace(/^-/, "");
    if (!v) {
      setSugs([]);
      setOpen(false);
      return;
    }
    const t = setTimeout(() => {
      api.suggestTags(v).then((s) => {
        setSugs(s);
        setOpen(s.length > 0);
      });
    }, 120);
    return () => clearTimeout(t);
  }, [q]);

  const neg = q.trim().startsWith("-");
  const accept = (name: string) => {
    props.onAdd(name, neg);
    setQ("");
    setOpen(false);
  };

  return (
    <div className="topbar">
      <div className="logo">
        <b>
          nai<em>·</em>gallery
        </b>
      </div>
      <div className="search">
        <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.4">
          <circle cx="11" cy="11" r="7" />
          <path d="M20 20l-4-4" />
        </svg>
        {props.filters.map((f, i) => (
          <span key={f.tag + f.neg} className={`chip ${f.neg ? "neg" : ""}`} onClick={() => props.onRemove(i)}>
            {f.neg ? "−" : ""}
            {f.tag} <span className="x">✕</span>
          </span>
        ))}
        <input
          value={q}
          onChange={(e) => setQ(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && sugs.length) accept(sugs[0].name);
            else if (e.key === "Backspace" && !q && props.filters.length)
              props.onRemove(props.filters.length - 1);
            else if (e.key === "Escape") setOpen(false);
          }}
          onBlur={() => setTimeout(() => setOpen(false), 150)}
          placeholder="Search tags — prefix with - to exclude"
        />
        {open && (
          <div className="dropdown open">
            {sugs.map((s) => (
              <div key={s.name} className="dd-row" onMouseDown={() => accept(s.name)}>
                <span className={`t-${s.category}`}>
                  {neg ? "−" : ""}
                  {s.name}
                </span>
                <span className="cnt">{s.count}</span>
              </div>
            ))}
          </div>
        )}
      </div>
      <div className="top-controls">
        <select className="select" value={props.sort} onChange={(e) => props.setSort(e.target.value)}>
          <option value="newest">Newest first</option>
          <option value="oldest">Oldest first</option>
          <option value="rating">By rating</option>
        </select>
        <div className="zoom">
          ▦
          <input
            type="range"
            min={130}
            max={340}
            value={props.thumbWidth}
            onChange={(e) => props.setThumbWidth(Number(e.target.value))}
          />
        </div>
      </div>
    </div>
  );
}

// ── Sidebar ──────────────────────────────────────────────────

function Sidebar(props: {
  scope: Scope;
  setScope: (s: Scope) => void;
  stats: api.LibraryStats | null;
  folders: api.FolderInfo[];
  albums: api.AlbumInfo[];
  searches: api.SavedSearch[];
  tops: api.TagSuggestion[];
  onTag: (t: string) => void;
  onAddFolder: () => void;
  onRemoveFolder: (id: number) => void;
  onNewAlbum: () => void;
  onDeleteAlbum: (a: api.AlbumInfo) => void;
  onSaveSearch: () => void;
  onApplySearch: (s: api.SavedSearch) => void;
  onDeleteSearch: (id: number) => void;
}) {
  const is = (s: Scope) => sameScope(props.scope, s);

  return (
    <div className="sidebar">
      <div className="side-h">Library</div>
      <div className={`side-row ${is({ kind: "all" }) ? "active" : ""}`} onClick={() => props.setScope({ kind: "all" })}>
        <span className="ico">◈</span>
        <span className="name">All images</span>
        <span className="n">{props.stats?.total ?? ""}</span>
      </div>
      <div
        className={`side-row ${is({ kind: "favorites" }) ? "active" : ""}`}
        onClick={() => props.setScope({ kind: "favorites" })}
      >
        <span className="ico">♥</span>
        <span className="name">Favorites</span>
        <span className="n">{props.stats?.favorites ?? ""}</span>
      </div>
      <div
        className={`side-row ${is({ kind: "rejects" }) ? "active" : ""}`}
        onClick={() => props.setScope({ kind: "rejects" })}
        title="Images you've rejected — still on disk, hidden from every other view"
      >
        <span className="ico">⊘</span>
        <span className="name">Rejects</span>
        <span className="n">{props.stats?.rejects ?? ""}</span>
      </div>

      <div className="side-h">
        Albums
        <span className="h-act" title="New album" onClick={props.onNewAlbum}>
          ＋
        </span>
      </div>
      {props.albums.map((a) => (
        <div
          key={a.id}
          className={`side-row ${is({ kind: "album", id: a.id }) ? "active" : ""}`}
          onClick={() => props.setScope({ kind: "album", id: a.id })}
        >
          <span className="ico">▤</span>
          <span className="name ltr">{a.name}</span>
          <span className="n">{a.image_count}</span>
          <span
            className="rm"
            title="Delete album (images stay in the library)"
            onClick={(e) => {
              e.stopPropagation();
              props.onDeleteAlbum(a);
            }}
          >
            ✕
          </span>
        </div>
      ))}
      {props.albums.length === 0 && <div className="side-empty">No albums yet</div>}

      <div className="side-h">
        Saved searches
        <span className="h-act" title="Save the current search" onClick={props.onSaveSearch}>
          ＋
        </span>
      </div>
      {props.searches.map((s) => (
        <div key={s.id} className="side-row" onClick={() => props.onApplySearch(s)}>
          <span className="ico">⌕</span>
          <span className="name ltr">{s.name}</span>
          <span
            className="rm"
            title="Delete saved search"
            onClick={(e) => {
              e.stopPropagation();
              props.onDeleteSearch(s.id);
            }}
          >
            ✕
          </span>
        </div>
      ))}
      {props.searches.length === 0 && <div className="side-empty">Search, then save it here</div>}

      <div className="side-h">Folders</div>
      {props.folders.map((f) => (
        <div
          key={f.id}
          className={`side-row ${is({ kind: "folder", id: f.id }) ? "active" : ""}`}
          onClick={() =>
            props.setScope(is({ kind: "folder", id: f.id }) ? { kind: "all" } : { kind: "folder", id: f.id })
          }
          title={f.path}
        >
          <span className="ico">▸</span>
          <span className="name">{f.path}</span>
          <span className="n">{f.image_count}</span>
          <span
            className="rm"
            title="Remove from library (files stay on disk)"
            onClick={(e) => {
              e.stopPropagation();
              props.onRemoveFolder(f.id);
            }}
          >
            ✕
          </span>
        </div>
      ))}
      <div className="side-row add" onClick={props.onAddFolder}>
        <span className="ico">＋</span>
        <span className="name">Add folder…</span>
      </div>

      <div className="side-h">Top tags</div>
      {props.tops.map((t) => (
        <div key={t.name} className={`side-row tag-row t-${t.category}`} onClick={() => props.onTag(t.name)}>
          <span className="name">{t.name}</span>
          <span className="n">{t.count}</span>
        </div>
      ))}
    </div>
  );
}

// ── BulkBar ──────────────────────────────────────────────────

function BulkBar(props: {
  count: number;
  scope: Scope;
  albums: api.AlbumInfo[];
  onClear: () => void;
  onFav: (v: boolean) => void;
  onRate: (n: number) => void;
  onReject: () => void;
  onRestore: () => void;
  onTrash: () => void;
  onAddToAlbum: (id: number) => void;
  onNewAlbum: () => void;
  onRemoveFromAlbum: () => void;
}) {
  const [albumOpen, setAlbumOpen] = useState(false);
  const inRejects = props.scope.kind === "rejects";

  return (
    <div className="bulkbar">
      <span className="bulk-count">
        <b>{props.count}</b> selected
      </span>

      <div className="bulk-stars" title="Rate the selection">
        {[1, 2, 3, 4, 5].map((i) => (
          <span key={i} onClick={() => props.onRate(i)}>
            ★
          </span>
        ))}
        <span className="clear-stars" onClick={() => props.onRate(0)} title="Clear rating">
          ✕
        </span>
      </div>

      <button className="bulk-btn" onClick={() => props.onFav(true)}>
        ♥ Favorite
      </button>
      <button className="bulk-btn" onClick={() => props.onFav(false)}>
        ♡ Unfavorite
      </button>

      <div className="bulk-album">
        <button className="bulk-btn" onClick={() => setAlbumOpen((o) => !o)}>
          ▤ Add to album ▾
        </button>
        {albumOpen && (
          <>
            <div className="pop-back" onClick={() => setAlbumOpen(false)} />
            <div className="pop">
              {props.albums.map((a) => (
                <div
                  key={a.id}
                  className="dd-row"
                  onClick={() => {
                    props.onAddToAlbum(a.id);
                    setAlbumOpen(false);
                  }}
                >
                  <span>{a.name}</span>
                  <span className="cnt">{a.image_count}</span>
                </div>
              ))}
              {props.albums.length === 0 && <div className="pop-empty">No albums yet</div>}
              <div
                className="dd-row new"
                onClick={() => {
                  props.onNewAlbum();
                  setAlbumOpen(false);
                }}
              >
                ＋ New album…
              </div>
            </div>
          </>
        )}
      </div>

      {props.scope.kind === "album" && (
        <button className="bulk-btn" onClick={props.onRemoveFromAlbum}>
          ⊖ Remove from album
        </button>
      )}

      {inRejects ? (
        <>
          <button className="bulk-btn" onClick={props.onRestore}>
            ↩ Restore
          </button>
          <button className="bulk-btn danger" onClick={props.onTrash}>
            🗑 Recycle Bin…
          </button>
        </>
      ) : (
        <button className="bulk-btn" onClick={props.onReject} title="Hide from every view except Rejects (Del)">
          ⊘ Reject
        </button>
      )}

      <button className="bulk-btn ghost" onClick={props.onClear}>
        Clear
      </button>
    </div>
  );
}

// ── Grid ─────────────────────────────────────────────────────

function Grid(props: {
  cards: api.ImageCard[];
  total: number;
  filters: Filter[];
  scope: Scope;
  sel: Set<number>;
  primaryId: number | null;
  onSelect: (id: number, mods: Mods) => void;
  onOpen: (id: number) => void;
  onFav: (id: number, current: boolean) => void;
  onLoadMore: () => void;
  thumbWidth: number;
}) {
  const wrapRef = useRef<HTMLDivElement>(null);
  const sentinelRef = useRef<HTMLDivElement>(null);
  const [cols, setCols] = useState(4);
  const { onLoadMore } = props;

  useEffect(() => {
    const el = wrapRef.current;
    if (!el) return;
    const ro = new ResizeObserver(() => {
      setCols(Math.max(1, Math.floor(el.clientWidth / (props.thumbWidth + 10))));
    });
    ro.observe(el);
    return () => ro.disconnect();
  }, [props.thumbWidth]);

  useEffect(() => {
    const s = sentinelRef.current;
    if (!s) return;
    const io = new IntersectionObserver((entries) => {
      if (entries[0].isIntersecting) onLoadMore();
    });
    io.observe(s);
    return () => io.disconnect();
  }, [onLoadMore]);

  // distribute into columns by running height (stable under append)
  const columns = useMemo(() => {
    const heights = Array(cols).fill(0);
    const buckets: api.ImageCard[][] = Array.from({ length: cols }, () => []);
    for (const c of props.cards) {
      const aspect = c.height && c.width ? c.height / c.width : 1;
      let best = 0;
      for (let i = 1; i < cols; i++) if (heights[i] < heights[best]) best = i;
      buckets[best].push(c);
      heights[best] += aspect;
    }
    return buckets;
  }, [props.cards, cols]);

  const multi = props.sel.size > 1;

  return (
    <div className="gridwrap" ref={wrapRef}>
      <div className="result-line">
        <b>{props.total}</b> image{props.total === 1 ? "" : "s"}
        {props.scope.kind === "rejects" && <> in Rejects</>}
        {props.filters.length > 0 && (
          <> matching {props.filters.map((f) => (f.neg ? "−" : "") + f.tag).join(" · ")}</>
        )}
        {props.sel.size > 0 && <span> · {props.sel.size} selected</span>}
        {props.total === 0 && props.scope.kind === "rejects" && (
          <> — nothing rejected yet. Select images and press Del to reject them.</>
        )}
      </div>
      <div className="masonry">
        {columns.map((bucket, i) => (
          <div className="mcol" key={i}>
            {bucket.map((c) => {
              const selected = props.sel.has(c.id);
              return (
                <div
                  key={c.id}
                  className={`card ${selected ? "sel" : ""} ${c.id === props.primaryId ? "primary" : ""}`}
                  onClick={(e) =>
                    props.onSelect(c.id, { shift: e.shiftKey, ctrl: e.ctrlKey || e.metaKey })
                  }
                  onDoubleClick={() => props.onOpen(c.id)}
                >
                  <img
                    src={api.thumbUrl(c.id)}
                    loading="lazy"
                    style={{ aspectRatio: `${c.width || 1} / ${c.height || 1}` }}
                    alt=""
                  />
                  {multi && selected && <div className="selmark">✓</div>}
                  <div
                    className={`fav ${c.favorite ? "on" : ""}`}
                    onClick={(e) => {
                      e.stopPropagation();
                      props.onFav(c.id, c.favorite);
                    }}
                  >
                    {c.favorite ? "♥" : "♡"}
                  </div>
                  <div className="ov">
                    <span>
                      {c.width}×{c.height}
                    </span>
                    <span className="stars-mini">{"★".repeat(c.rating)}</span>
                  </div>
                </div>
              );
            })}
          </div>
        ))}
      </div>
      <div ref={sentinelRef} style={{ height: 1 }} />
      {props.cards.length < props.total && <div className="loading-more">loading…</div>}
    </div>
  );
}

// ── Inspector ────────────────────────────────────────────────

function Stars(props: { rating: number; onRate: (n: number) => void }) {
  return (
    <div className="stars">
      {[1, 2, 3, 4, 5].map((i) => (
        <span
          key={i}
          className={i <= props.rating ? "on" : ""}
          onClick={() => props.onRate(props.rating === i ? 0 : i)}
        >
          ★
        </span>
      ))}
    </div>
  );
}

function Inspector(props: {
  detail: api.ImageDetail | null;
  onTag: (t: string) => void;
  onFav: (id: number, current: boolean) => void;
  onRate: (id: number, n: number) => void;
}) {
  const [showNeg, setShowNeg] = useState(false);
  const d = props.detail;
  useEffect(() => setShowNeg(false), [d?.id]);
  if (!d)
    return (
      <div className="inspector">
        <div className="empty-insp">Select an image to inspect its generation metadata.</div>
      </div>
    );

  const baseTags = d.tags.filter((t) => t.source === "base");
  const charTags = d.tags.filter((t) => t.source === "char");
  const date = new Date(d.file_mtime * 1000).toLocaleString();

  return (
    <div className="inspector">
      <div className="insp-preview">
        <img src={api.thumbUrl(d.id)} alt="" />
      </div>
      <div className="insp-body">
        <div className="insp-file">
          <b title={`${d.path} — click to show in Explorer`} onClick={() => api.openInExplorer(d.path)} className="linkish">
            {d.file_name}
          </b>
          <br />
          {d.width} × {d.height} px · {date}
        </div>
        <div className="rating-row">
          <Stars rating={d.rating} onRate={(n) => props.onRate(d.id, n)} />
          <button className={`favbtn ${d.favorite ? "on" : ""}`} onClick={() => props.onFav(d.id, d.favorite)}>
            {d.favorite ? "♥ favorited" : "♡ favorite"}
          </button>
        </div>
        {d.is_novelai ? (
          <>
            <div className="insp-h">Generation</div>
            <dl className="kv">
              <dt>Model</dt>
              <dd>{d.model ?? "—"}</dd>
              <dt>Seed</dt>
              <dd
                className="copy"
                title="Click to copy"
                onClick={() => d.seed != null && navigator.clipboard.writeText(String(d.seed))}
              >
                {d.seed ?? "—"} ⧉
              </dd>
              <dt>Sampler</dt>
              <dd>{d.sampler ?? "—"}</dd>
              <dt>Steps</dt>
              <dd>
                {d.steps ?? "—"} · guidance {d.scale ?? "—"}
              </dd>
            </dl>
            {baseTags.length > 0 && (
              <>
                <div className="insp-h">Prompt tags</div>
                <div className="tagcloud">
                  {baseTags.map((t) => (
                    <span key={t.name} className={`tag t-${t.category}`} onClick={() => props.onTag(t.name)}>
                      {t.name}
                    </span>
                  ))}
                </div>
              </>
            )}
            {charTags.length > 0 && (
              <>
                <div className="insp-h">Character</div>
                <div className="tagcloud">
                  {charTags.map((t) => (
                    <span key={t.name} className={`tag t-${t.category}`} onClick={() => props.onTag(t.name)}>
                      {t.name}
                    </span>
                  ))}
                </div>
              </>
            )}
            {d.raw_negative && (
              <>
                <div className="insp-h">Negative prompt</div>
                <div className="negline">
                  {showNeg ? (
                    d.raw_negative
                  ) : (
                    <span className="neg-toggle" onClick={() => setShowNeg(true)}>
                      show ▾
                    </span>
                  )}
                </div>
              </>
            )}
            {d.raw_prompt && (
              <button className="copybtn" onClick={() => navigator.clipboard.writeText(d.raw_prompt!)}>
                ⧉ Copy full prompt
              </button>
            )}
          </>
        ) : (
          <div className="insp-h">No NovelAI metadata</div>
        )}
      </div>
    </div>
  );
}

// ── Viewer ───────────────────────────────────────────────────

function Viewer(props: {
  detail: api.ImageDetail;
  onClose: () => void;
  onNav: (dir: 1 | -1) => void;
  onFav: (id: number, current: boolean) => void;
  onRate: (id: number, n: number) => void;
}) {
  const d = props.detail;
  const [showInfo, setShowInfo] = useState(false);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key.toLowerCase() === "i") setShowInfo((s) => !s);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  return (
    <div className="viewer" onClick={props.onClose}>
      <img src={api.origUrl(d.id)} alt="" onClick={(e) => e.stopPropagation()} />
      <button className="v-btn v-close" onClick={props.onClose}>
        ✕
      </button>
      <button
        className="v-btn v-prev"
        onClick={(e) => {
          e.stopPropagation();
          props.onNav(-1);
        }}
      >
        ‹
      </button>
      <button
        className="v-btn v-next"
        onClick={(e) => {
          e.stopPropagation();
          props.onNav(1);
        }}
      >
        ›
      </button>
      <div className="v-bar" onClick={(e) => e.stopPropagation()}>
        <span className="v-name" title={d.path}>
          {d.file_name}
        </span>
        <Stars rating={d.rating} onRate={(n) => props.onRate(d.id, n)} />
        <button className={`favbtn ${d.favorite ? "on" : ""}`} onClick={() => props.onFav(d.id, d.favorite)}>
          {d.favorite ? "♥" : "♡"}
        </button>
        <button className="favbtn" onClick={() => setShowInfo((s) => !s)}>
          info (i)
        </button>
      </div>
      {showInfo && (
        <div className="v-info" onClick={(e) => e.stopPropagation()}>
          <div className="kv-mini">
            <span>{d.model}</span>
            <span>seed {d.seed}</span>
            <span>
              {d.sampler} · {d.steps} steps · guidance {d.scale}
            </span>
          </div>
          <div className="v-prompt">{d.raw_prompt}</div>
        </div>
      )}
    </div>
  );
}

// ── NameModal ────────────────────────────────────────────────

function NameModal(props: { prompt: NamePrompt; onClose: () => void }) {
  const [v, setV] = useState(props.prompt.initial ?? "");
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    inputRef.current?.focus();
    inputRef.current?.select();
  }, []);

  const submit = () => {
    const value = v.trim();
    if (!value) return;
    props.prompt.onSubmit(value);
    props.onClose();
  };

  return (
    <div className="modal-back" onClick={props.onClose}>
      <div className="modal" onClick={(e) => e.stopPropagation()}>
        <div className="modal-title">{props.prompt.title}</div>
        <input
          ref={inputRef}
          value={v}
          placeholder={props.prompt.placeholder}
          onChange={(e) => setV(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") submit();
            else if (e.key === "Escape") props.onClose();
          }}
        />
        <div className="modal-actions">
          <button className="copybtn" onClick={props.onClose}>
            Cancel
          </button>
          <button className="copybtn accent" onClick={submit} disabled={!v.trim()}>
            OK
          </button>
        </div>
      </div>
    </div>
  );
}

// ── StatusBar ────────────────────────────────────────────────

function StatusBar(props: {
  stats: api.LibraryStats | null;
  scanMsg: string | null;
  onRescan: () => void;
  update: Update | null;
  updating: boolean;
  onInstallUpdate: () => void;
}) {
  return (
    <div className="status">
      <span className={`dot ${props.scanMsg ? "busy" : ""}`} />
      <span>
        {props.stats && props.stats.total > 0
          ? `${props.stats.total} images indexed · ${props.stats.novelai} with NovelAI metadata`
          : "empty library — add a folder to begin"}
      </span>
      {props.scanMsg && <span className="scanmsg">{props.scanMsg}</span>}
      {props.update && (
        <span className="update-banner linkish" onClick={props.onInstallUpdate}>
          {props.updating
            ? "downloading update…"
            : `⬆ update ${props.update.version} available — install & restart`}
        </span>
      )}
      <span className="right linkish" onClick={props.onRescan}>
        ⟳ rescan
      </span>
    </div>
  );
}
