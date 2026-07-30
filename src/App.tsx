import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { check, type Update } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";
import * as api from "./api";
import "./App.css";

interface Filter {
  tag: string;
  neg: boolean;
}

type View = "all" | "favorites";

const PAGE = 200;

export default function App() {
  // ── query state ──
  const [filters, setFilters] = useState<Filter[]>([]);
  const [view, setView] = useState<View>("all");
  const [folderId, setFolderId] = useState<number | null>(null);
  const [sort, setSort] = useState("newest");
  // ── data state ──
  const [cards, setCards] = useState<api.ImageCard[]>([]);
  const [total, setTotal] = useState(0);
  const [folders, setFolders] = useState<api.FolderInfo[]>([]);
  const [tops, setTops] = useState<api.TagSuggestion[]>([]);
  const [stats, setStats] = useState<api.LibraryStats | null>(null);
  const [scanMsg, setScanMsg] = useState<string | null>(null);
  // ── selection / viewer ──
  const [selectedId, setSelectedId] = useState<number | null>(null);
  const [detail, setDetail] = useState<api.ImageDetail | null>(null);
  const [viewerOpen, setViewerOpen] = useState(false);
  const [thumbWidth, setThumbWidth] = useState(210);
  const [update, setUpdate] = useState<Update | null>(null);
  const [updating, setUpdating] = useState(false);

  const queryRef = useRef(0);

  const buildQuery = useCallback(
    (offset: number): Partial<api.Query> => ({
      include_tags: filters.filter((f) => !f.neg).map((f) => f.tag),
      exclude_tags: filters.filter((f) => f.neg).map((f) => f.tag),
      favorite: view === "favorites" ? true : undefined,
      folder_id: folderId ?? undefined,
      sort,
      offset,
      limit: PAGE,
    }),
    [filters, view, folderId, sort]
  );

  const refreshMeta = useCallback(() => {
    api.listFolders().then(setFolders).catch(() => {});
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

  // selection detail
  useEffect(() => {
    if (selectedId == null) {
      setDetail(null);
      return;
    }
    api.getImage(selectedId).then(setDetail).catch(() => setDetail(null));
  }, [selectedId]);

  // ── actions ──
  const addFilter = useCallback((tag: string, neg: boolean) => {
    setFilters((f) =>
      f.some((x) => x.tag === tag && x.neg === neg) ? f : [...f, { tag, neg }]
    );
  }, []);
  const removeFilter = (i: number) => setFilters((f) => f.filter((_, j) => j !== i));

  const patchCard = useCallback((id: number, patch: Partial<api.ImageCard>) => {
    setCards((cs) => cs.map((c) => (c.id === id ? { ...c, ...patch } : c)));
  }, []);

  const toggleFav = useCallback(
    (id: number, current: boolean) => {
      api.setFavorite(id, !current).then(() => {
        patchCard(id, { favorite: !current });
        setDetail((d) => (d && d.id === id ? { ...d, favorite: !current } : d));
        api.getStats().then(setStats).catch(() => {});
      });
    },
    [patchCard]
  );

  const rate = useCallback(
    (id: number, rating: number) => {
      api.setRating(id, rating).then(() => {
        patchCard(id, { rating });
        setDetail((d) => (d && d.id === id ? { ...d, rating } : d));
      });
    },
    [patchCard]
  );

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
      if ((e.target as HTMLElement)?.tagName === "INPUT") return;
      const idx = cards.findIndex((c) => c.id === selectedId);
      if (e.key === "ArrowRight" || e.key === "ArrowLeft") {
        e.preventDefault();
        const next = e.key === "ArrowRight" ? idx + 1 : idx - 1;
        if (next >= 0 && next < cards.length) setSelectedId(cards[next].id);
        if (next >= cards.length - 10) loadMore();
      } else if (e.key === "Escape") {
        setViewerOpen(false);
      } else if (e.key === "Enter" && selectedId != null) {
        setViewerOpen(true);
      } else if (e.key.toLowerCase() === "f" && selectedId != null && detail) {
        toggleFav(selectedId, detail.favorite);
      } else if (/^[0-5]$/.test(e.key) && selectedId != null) {
        rate(selectedId, Number(e.key));
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [cards, selectedId, detail, loadMore, toggleFav, rate]);

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
        view={view}
        setView={(v) => {
          setView(v);
          setFolderId(null);
        }}
        stats={stats}
        folders={folders}
        folderId={folderId}
        setFolderId={(id) => {
          setFolderId(id);
          setView("all");
        }}
        tops={tops}
        onTag={(t) => addFilter(t, false)}
        onAddFolder={pickFolder}
        onRemoveFolder={async (id) => {
          await api.removeFolder(id);
          if (folderId === id) setFolderId(null);
          refreshMeta();
          runQuery();
        }}
      />
      <Grid
        cards={cards}
        total={total}
        filters={filters}
        selectedId={selectedId}
        onSelect={setSelectedId}
        onOpen={(id) => {
          setSelectedId(id);
          setViewerOpen(true);
        }}
        onFav={toggleFav}
        onLoadMore={loadMore}
        thumbWidth={thumbWidth}
      />
      <Inspector
        detail={detail}
        onTag={(t) => addFilter(t, false)}
        onFav={toggleFav}
        onRate={rate}
      />
      <StatusBar
        stats={stats}
        scanMsg={scanMsg}
        onRescan={() => api.rescanAll()}
        update={update}
        updating={updating}
        onInstallUpdate={installUpdate}
      />
      {viewerOpen && detail && (
        <Viewer
          detail={detail}
          onClose={() => setViewerOpen(false)}
          onNav={(dir) => {
            const idx = cards.findIndex((c) => c.id === selectedId);
            const next = idx + dir;
            if (next >= 0 && next < cards.length) setSelectedId(cards[next].id);
            if (next >= cards.length - 10) loadMore();
          }}
          onFav={toggleFav}
          onRate={rate}
        />
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
  view: View;
  setView: (v: View) => void;
  stats: api.LibraryStats | null;
  folders: api.FolderInfo[];
  folderId: number | null;
  setFolderId: (id: number | null) => void;
  tops: api.TagSuggestion[];
  onTag: (t: string) => void;
  onAddFolder: () => void;
  onRemoveFolder: (id: number) => void;
}) {
  return (
    <div className="sidebar">
      <div className="side-h">Library</div>
      <div
        className={`side-row ${props.view === "all" && props.folderId == null ? "active" : ""}`}
        onClick={() => {
          props.setView("all");
          props.setFolderId(null);
        }}
      >
        <span className="ico">◈</span>
        <span className="name">All images</span>
        <span className="n">{props.stats?.total ?? ""}</span>
      </div>
      <div
        className={`side-row ${props.view === "favorites" ? "active" : ""}`}
        onClick={() => props.setView("favorites")}
      >
        <span className="ico">♥</span>
        <span className="name">Favorites</span>
        <span className="n">{props.stats?.favorites ?? ""}</span>
      </div>
      <div className="side-h">Folders</div>
      {props.folders.map((f) => (
        <div
          key={f.id}
          className={`side-row ${props.folderId === f.id ? "active" : ""}`}
          onClick={() => props.setFolderId(props.folderId === f.id ? null : f.id)}
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

// ── Grid ─────────────────────────────────────────────────────

function Grid(props: {
  cards: api.ImageCard[];
  total: number;
  filters: Filter[];
  selectedId: number | null;
  onSelect: (id: number) => void;
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

  const selIdx = props.cards.findIndex((c) => c.id === props.selectedId);

  return (
    <div className="gridwrap" ref={wrapRef}>
      <div className="result-line">
        <b>{props.total}</b> image{props.total === 1 ? "" : "s"}
        {props.filters.length > 0 && (
          <> matching {props.filters.map((f) => (f.neg ? "−" : "") + f.tag).join(" · ")}</>
        )}
        {selIdx >= 0 && <span> · #{selIdx + 1} selected</span>}
      </div>
      <div className="masonry">
        {columns.map((bucket, i) => (
          <div className="mcol" key={i}>
            {bucket.map((c) => (
              <div
                key={c.id}
                className={`card ${c.id === props.selectedId ? "sel" : ""}`}
                onClick={() => props.onSelect(c.id)}
                onDoubleClick={() => props.onOpen(c.id)}
              >
                <img
                  src={api.thumbUrl(c.id)}
                  loading="lazy"
                  style={{ aspectRatio: `${c.width || 1} / ${c.height || 1}` }}
                  alt=""
                />
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
            ))}
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
