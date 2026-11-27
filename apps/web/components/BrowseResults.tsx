"use client";

import { Suspense, useEffect, useState } from "react";
import Link from "next/link";
import { useRouter, useSearchParams } from "next/navigation";
import FilterPanel from "./FilterPanel";
import ProductCard from "./ProductCard";
import ProductListRow from "./ProductListRow";
import { Button, EmptyState, ProductCardSkeleton } from "@/components/ui";
import {
  createSavedSearch,
  getPopularSearches,
  searchProducts,
  type PopularSearch,
  type SearchResult,
  type SearchSort,
} from "@/lib/api";
import {
  activeFilterCount,
  browseStateFromParams,
  browseUrl,
  emptyBrowseState,
  pageWindow,
  savedFiltersFromState,
  SORT_OPTIONS,
  type BrowseState,
} from "@/lib/browse";
import { formatBdt } from "@/lib/format";
import { useAuth } from "@/hooks/useAuth";
import { useCart } from "@/hooks/useCart";

/**
 * The faceted browse (Wave 53) — ONE component behind
 * both /browse and /search, so the two surfaces can never
 * grow separate filter logic. The URL is the state: every
 * control writes the next BrowseState to the URL (via
 * lib/browse), the effect below fetches /v1/search for
 * exactly those parameters, and the facet block of the
 * response feeds the FilterPanel's counts.
 *
 * Layout: a filter sidebar on desktop, the same
 * FilterPanel in a slide-over drawer on mobile; a toolbar
 * with the result count, the sort dropdown, the
 * grid/list view toggle and (signed in) the save-search
 * action; active-filter chips with individual removal;
 * grid or list results; windowed pagination. The loaded
 * page also emits a JSON-LD ItemList of what it shows.
 */
export default function BrowseResults({
  basePath,
  heading,
  forcedStoreSlug,
  forcedCategory,
  forcedCategorySlug,
}: {
  basePath: string;
  heading: string;
  /**
   * Pin the browse to one store (the storefront shop
   * page): the store scope comes from the route, not the
   * URL, and the store filter group is hidden.
   */
  forcedStoreSlug?: string;
  /**
   * Pin the browse to one category (the category landing
   * page): the category comes from the route, and the
   * category filter group is hidden.
   */
  forcedCategory?: string;
  /**
   * Pin the browse to one TAXONOMY subtree (Wave 56 — the
   * taxonomy category landing): the slug scopes the
   * browse server-side to the node's whole subtree, and
   * the category filter group is hidden the same way.
   */
  forcedCategorySlug?: string;
}) {
  return (
    <Suspense fallback={<p>লোড হচ্ছে… (Loading…)</p>}>
      <BrowseResultsInner
        basePath={basePath}
        heading={heading}
        forcedStoreSlug={forcedStoreSlug}
        forcedCategory={forcedCategory}
        forcedCategorySlug={forcedCategorySlug}
      />
    </Suspense>
  );
}

function BrowseResultsInner({
  basePath,
  heading,
  forcedStoreSlug,
  forcedCategory,
  forcedCategorySlug,
}: {
  basePath: string;
  heading: string;
  forcedStoreSlug?: string;
  forcedCategory?: string;
  forcedCategorySlug?: string;
}) {
  const router = useRouter();
  const searchParams = useSearchParams();
  const { token } = useAuth();
  const { add, error: cartError } = useCart();

  const parsed = browseStateFromParams(searchParams);
  let state: BrowseState = parsed;
  if (forcedStoreSlug) {
    state = { ...state, storeSlug: forcedStoreSlug, stores: [] };
  }
  if (forcedCategory) {
    state = { ...state, categories: [forcedCategory] };
  }
  if (forcedCategorySlug) {
    state = { ...state, categorySlugs: [forcedCategorySlug] };
  }
  const stateKey =
    searchParams.toString() +
    (forcedStoreSlug ?? "") +
    (forcedCategory ?? "") +
    (forcedCategorySlug ?? "");

  const [results, setResults] = useState<SearchResult | null>(null);
  const [offline, setOffline] = useState(false);
  const [view, setView] = useState<"grid" | "list">("grid");
  const [drawerOpen, setDrawerOpen] = useState(false);
  const [addedId, setAddedId] = useState<string | null>(null);
  const [saveOpen, setSaveOpen] = useState(false);
  const [saveName, setSaveName] = useState("");
  const [saveMessage, setSaveMessage] = useState<string | null>(null);
  const [popular, setPopular] = useState<PopularSearch[]>([]);
  const [shared, setShared] = useState(false);

  // Popular searches, for the empty state's recovery
  // links: a dead end should always offer a way back into
  // the catalogue. Best-effort — an empty list just means
  // the empty state shows its reset button only.
  useEffect(() => {
    let cancelled = false;
    getPopularSearches()
      .then((result) => {
        if (!cancelled) setPopular(result.searches);
      })
      .catch(() => {
        // Recovery links are garnish; their absence is silent.
      });
    return () => {
      cancelled = true;
    };
  }, []);

  async function onShare() {
    try {
      await navigator.clipboard.writeText(window.location.href);
      setShared(true);
      setTimeout(() => setShared(false), 2000);
    } catch {
      // Clipboard unavailable (permissions, insecure
      // context): the URL is in the address bar regardless.
      setShared(false);
    }
  }

  useEffect(() => {
    let cancelled = false;
    setResults(null);
    setOffline(false);
    searchProducts({
      q: state.q || undefined,
      categories: state.categories,
      category_slugs: state.categorySlugs,
      stores: state.stores,
      store_slug: state.storeSlug || undefined,
      min_price_poisha: state.minPricePoisha ?? undefined,
      max_price_poisha: state.maxPricePoisha ?? undefined,
      min_rating: state.minRating ?? undefined,
      in_stock: state.inStock || undefined,
      on_sale: state.onSale || undefined,
      sort: state.sort,
      page: state.page,
      limit: state.limit,
    })
      .then((result) => {
        if (!cancelled) setResults(result);
      })
      .catch(() => {
        if (!cancelled) setOffline(true);
      });
    return () => {
      cancelled = true;
    };
    // stateKey is the canonical form of `state`; the fetch
    // must re-run exactly when the URL's browse state does.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [stateKey]);

  function navigate(next: BrowseState) {
    setDrawerOpen(false);
    router.push(browseUrl(basePath, next));
  }

  async function onAdd(productId: string) {
    setAddedId(null);
    await add(productId, 1);
    setAddedId(productId);
  }

  async function onSaveSearch(e: React.FormEvent) {
    e.preventDefault();
    if (!token) return;
    setSaveMessage(null);
    try {
      await createSavedSearch(token, saveName, savedFiltersFromState(state));
      setSaveOpen(false);
      setSaveName("");
      setSaveMessage("সংরক্ষিত হয়েছে ✓ — Saved");
    } catch (err) {
      setSaveMessage(
        err instanceof Error ? err.message : "সংরক্ষণ করা যায়নি।"
      );
    }
  }

  const totalPages =
    results !== null ? Math.max(1, Math.ceil(results.total / state.limit)) : 1;
  const filterCount = activeFilterCount(state);

  const chips: { key: string; label: string; clear: BrowseState }[] = [];
  if (state.q) {
    chips.push({
      key: "q",
      label: `“${state.q}”`,
      clear: { ...state, q: "", page: 1 },
    });
  }
  for (const category of state.categories) {
    if (forcedCategory && category === forcedCategory) continue;
    chips.push({
      key: `category:${category}`,
      label: category,
      clear: {
        ...state,
        categories: state.categories.filter((entry) => entry !== category),
        page: 1,
      },
    });
  }
  for (const slug of state.categorySlugs) {
    if (forcedCategorySlug && slug === forcedCategorySlug) continue;
    chips.push({
      key: `category_slug:${slug}`,
      label: `ক্যাটাগরি: ${slug}`,
      clear: {
        ...state,
        categorySlugs: state.categorySlugs.filter((entry) => entry !== slug),
        page: 1,
      },
    });
  }
  for (const store of state.stores) {
    chips.push({
      key: `store:${store}`,
      label: store,
      clear: {
        ...state,
        stores: state.stores.filter((entry) => entry !== store),
        page: 1,
      },
    });
  }
  if (state.storeSlug && !forcedStoreSlug) {
    chips.push({
      key: "store_slug",
      label: `স্টোর: ${state.storeSlug}`,
      clear: { ...state, storeSlug: "", page: 1 },
    });
  }
  if (state.minPricePoisha !== null || state.maxPricePoisha !== null) {
    chips.push({
      key: "price",
      label: `${formatBdt(state.minPricePoisha ?? 0)} – ${
        state.maxPricePoisha !== null ? formatBdt(state.maxPricePoisha) : "∞"
      }`,
      clear: { ...state, minPricePoisha: null, maxPricePoisha: null, page: 1 },
    });
  }
  if (state.minRating !== null) {
    chips.push({
      key: "rating",
      label: `${state.minRating}★ ও বেশি`,
      clear: { ...state, minRating: null, page: 1 },
    });
  }
  if (state.inStock) {
    chips.push({
      key: "in_stock",
      label: "স্টকে আছে — In stock",
      clear: { ...state, inStock: false, page: 1 },
    });
  }
  if (state.onSale) {
    chips.push({
      key: "on_sale",
      label: "অফারে আছে — On sale",
      clear: { ...state, onSale: false, page: 1 },
    });
  }

  const jsonLd =
    results !== null && results.items.length > 0
      ? {
          "@context": "https://schema.org",
          "@type": "ItemList",
          numberOfItems: results.total,
          itemListElement: results.items.map((p, index) => ({
            "@type": "ListItem",
            position: (state.page - 1) * state.limit + index + 1,
            name: p.title_bn ?? p.title,
            url: `/products/${p.id}`,
          })),
        }
      : null;

  return (
    <main>
      <h1>{heading}</h1>
      {state.q && (
        <p className="browse-query-line">
          “{state.q}” দিয়ে খোঁজা হচ্ছে — Searching for “{state.q}”
        </p>
      )}
      {jsonLd && (
        <script
          type="application/ld+json"
          dangerouslySetInnerHTML={{ __html: JSON.stringify(jsonLd) }}
        />
      )}
      <div className="browse-layout">
        <aside className="browse-sidebar">
          <FilterPanel
            state={state}
            facets={results?.facets ?? null}
            onChange={navigate}
            hideStores={!!forcedStoreSlug}
            hideCategories={!!forcedCategory || !!forcedCategorySlug}
          />
        </aside>

        <div className="browse-main">
          <div className="browse-toolbar">
            <p className="browse-count" aria-live="polite">
              {results !== null
                ? `মোট ${results.total}টি পণ্য — ${results.total} products`
                : "লোড হচ্ছে…"}
            </p>
            <div className="browse-toolbar-controls">
              <button
                type="button"
                className="browse-filter-button"
                onClick={() => setDrawerOpen(true)}
              >
                ফিল্টার — Filters
                {filterCount > 0 ? ` (${filterCount})` : ""}
              </button>
              <label className="browse-sort">
                সাজান — Sort{" "}
                <select
                  value={state.sort}
                  onChange={(e) =>
                    navigate({
                      ...state,
                      sort: e.target.value as SearchSort,
                      page: 1,
                    })
                  }
                >
                  {SORT_OPTIONS.map((option) => (
                    <option key={option.value} value={option.value}>
                      {option.label}
                    </option>
                  ))}
                </select>
              </label>
              <label className="browse-sort">
                প্রতি পাতায় — Per page{" "}
                <select
                  value={state.limit}
                  onChange={(e) =>
                    navigate({
                      ...state,
                      limit: Number(e.target.value),
                      page: 1,
                    })
                  }
                >
                  {[24, 48, 60].map((size) => (
                    <option key={size} value={size}>
                      {size}
                    </option>
                  ))}
                </select>
              </label>
              <div className="browse-view-toggle" role="group" aria-label="View">
                <button
                  type="button"
                  className={view === "grid" ? "is-active" : ""}
                  onClick={() => setView("grid")}
                >
                  গ্রিড — Grid
                </button>
                <button
                  type="button"
                  className={view === "list" ? "is-active" : ""}
                  onClick={() => setView("list")}
                >
                  তালিকা — List
                </button>
              </div>
              <button type="button" onClick={() => void onShare()}>
                {shared ? "কপি হয়েছে ✓ — Copied" : "লিংক কপি — Copy link"}
              </button>
              {token ? (
                <button type="button" onClick={() => setSaveOpen((v) => !v)}>
                  খোঁজটি সংরক্ষণ করুন — Save search
                </button>
              ) : (
                <Link href="/saved-searches">সংরক্ষিত খোঁজ — Saved searches</Link>
              )}
            </div>
          </div>

          {saveOpen && token && (
            <form onSubmit={(e) => void onSaveSearch(e)} className="browse-save">
              <label>
                খোঁজের নাম — Name{" "}
                <input
                  type="text"
                  value={saveName}
                  onChange={(e) => setSaveName(e.target.value)}
                  placeholder="যেমন: সস্তা পোশাক"
                  required
                />
              </label>{" "}
              <Button type="submit" variant="primary" size="sm">
                সংরক্ষণ — Save
              </Button>
            </form>
          )}
          {saveMessage && (
            <p>
              {saveMessage}{" "}
              <Link href="/saved-searches">সংরক্ষিত খোঁজ দেখুন — View saved</Link>
            </p>
          )}

          {chips.length > 0 && (
            <div className="browse-chips">
              {chips.map((chip) => (
                <button
                  key={chip.key}
                  type="button"
                  className="browse-chip"
                  onClick={() => navigate(chip.clear)}
                  title="সরান — Remove"
                >
                  {chip.label} ✕
                </button>
              ))}
              <button
                type="button"
                className="browse-chip browse-chip--clear"
                onClick={() => navigate(emptyBrowseState())}
              >
                সব ফিল্টার মুছে ফেলুন — Clear all
              </button>
            </div>
          )}

          {offline ? (
            <p>
              API-তে সংযোগ করা যাচ্ছে না — Rust API চালু আছে কিনা দেখুন।
              (Could not reach the API, so browsing cannot run.)
            </p>
          ) : results === null ? (
            <div className="search-grid" aria-hidden="true">
              {Array.from({ length: 8 }, (_, i) => (
                <ProductCardSkeleton key={i} />
              ))}
            </div>
          ) : results.total === 0 ? (
            <EmptyState title="কোনো পণ্য মেলেনি — No products matched">
              <p>
                ফিল্টারগুলো খুব কড়া হয়ে গেছে, অথবা এমন পণ্য এখনো নেই।
                (The filters are too narrow, or no such product exists yet.)
              </p>
              <Button
                type="button"
                variant="primary"
                onClick={() => navigate(emptyBrowseState())}
              >
                সব ফিল্টার মুছে ফেলুন — Clear all filters
              </Button>
              {popular.length > 0 && (
                <p className="browse-empty-popular">
                  জনপ্রিয় খোঁজ — Popular:{" "}
                  {popular.slice(0, 5).map((row, index) => (
                    <span key={row.query_text}>
                      {index > 0 ? " · " : ""}
                      <Link
                        href={`${basePath}?q=${encodeURIComponent(row.query_text)}`}
                      >
                        {row.query_text}
                      </Link>
                    </span>
                  ))}
                </p>
              )}
            </EmptyState>
          ) : (
            <>
              {cartError && <p role="alert">{cartError}</p>}
              {view === "grid" ? (
                <div className="search-grid">
                  {results.items.map((p) => (
                    <div key={p.id} className="search-result">
                      <ProductCard
                        name={p.title_bn ?? p.title}
                        storeName={p.store_slug}
                        pricePoisha={p.price_poisha}
                        productId={p.id}
                        href={`/products/${p.id}`}
                        ratingAvg={p.rating_avg}
                        ratingCount={p.rating_count}
                        isPromoted={p.is_promoted}
                        imageUrl={p.image_url}
                        hoverImageUrl={p.hover_image_url}
                        compareAtPoisha={p.compare_at_poisha}
                        soldCount={p.sold_count}
                        flashPricePoisha={p.flash_price_poisha}
                        flashEndsAt={p.flash_ends_at}
                      />
                      {token ? (
                        <p>
                          <button type="button" onClick={() => void onAdd(p.id)}>
                            কার্টে যোগ করুন — Add to cart
                          </button>{" "}
                          {addedId === p.id && !cartError && (
                            <span>
                              যোগ হয়েছে ✓ <Link href="/cart">কার্ট দেখুন</Link>
                            </span>
                          )}
                        </p>
                      ) : (
                        <p>
                          <Link href="/login">কার্টে যোগ করতে লগইন করুন</Link>
                        </p>
                      )}
                    </div>
                  ))}
                </div>
              ) : (
                <div className="browse-list">
                  {results.items.map((p) => (
                    <ProductListRow
                      key={p.id}
                      product={p}
                      onAdd={token ? (id) => void onAdd(id) : undefined}
                      added={addedId === p.id && !cartError}
                    />
                  ))}
                </div>
              )}

              {totalPages > 1 && (
                <p className="browse-page-line">
                  পাতা {state.page} / {totalPages} — Page {state.page} of {totalPages}
                </p>
              )}
              {totalPages > 1 && (
                <nav className="browse-pagination" aria-label="Pagination">
                  <button
                    type="button"
                    disabled={state.page <= 1}
                    onClick={() => navigate({ ...state, page: state.page - 1 })}
                  >
                    ← আগের — Prev
                  </button>
                  {pageWindow(state.page, totalPages).map((pageNumber) => (
                    <button
                      key={pageNumber}
                      type="button"
                      className={pageNumber === state.page ? "is-active" : ""}
                      onClick={() => navigate({ ...state, page: pageNumber })}
                    >
                      {pageNumber}
                    </button>
                  ))}
                  <button
                    type="button"
                    disabled={state.page >= totalPages}
                    onClick={() => navigate({ ...state, page: state.page + 1 })}
                  >
                    পরের — Next →
                  </button>
                </nav>
              )}
            </>
          )}
        </div>
      </div>

      {drawerOpen && (
        <div className="browse-drawer-backdrop" onClick={() => setDrawerOpen(false)}>
          <div
            className="browse-drawer"
            role="dialog"
            aria-label="Filters"
            onClick={(e) => e.stopPropagation()}
          >
            <div className="browse-drawer-head">
              <strong>ফিল্টার — Filters</strong>
              <button type="button" onClick={() => setDrawerOpen(false)}>
                বন্ধ করুন — Close ✕
              </button>
            </div>
            <FilterPanel
              state={state}
              facets={results?.facets ?? null}
              onChange={navigate}
              hideStores={!!forcedStoreSlug}
            hideCategories={!!forcedCategory || !!forcedCategorySlug}
            />
          </div>
        </div>
      )}
    </main>
  );
}
