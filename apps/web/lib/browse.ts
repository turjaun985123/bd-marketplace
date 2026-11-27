import type { SavedSearchFilters, SearchSort } from "./api";
import { formatBdt } from "./format";

/**
 * The browse state and its URL form (Wave 53). ONE state
 * shape drives the /browse page, the /search page, the
 * filter panel, the saved searches and every deep link
 * (category tiles, home rails): the state lives in the
 * URL's search params so a browse is shareable and the
 * back button works, and this module is the only place
 * that translates between the two forms.
 *
 * Parameter names are the API's own (category, store,
 * min_price_poisha, …) so a browse URL is also an API URL
 * modulo the host. Repeated parameters carry the
 * multi-value groups; the comma form is accepted on read
 * for hand-written links.
 */
export interface BrowseState {
  q: string;
  categories: string[];
  /**
   * Taxonomy slugs (Wave 56): scope the browse to the
   * union of the named categories' SUBTREES. Travels as
   * the API's `category_slug` parameter; deliberately NOT
   * part of saved searches (the API's saved filter shape
   * has no slug group).
   */
  categorySlugs: string[];
  stores: string[];
  /** Legacy single-store scope (`store_slug`), kept for storefront links. */
  storeSlug: string;
  minPricePoisha: number | null;
  maxPricePoisha: number | null;
  minRating: number | null;
  inStock: boolean;
  onSale: boolean;
  sort: SearchSort;
  page: number;
  /** Page size (the API's `limit`, 1..=60; the UI offers 24/48/60). */
  limit: number;
}

/** The browse page size — the API's default, named once here. */
export const BROWSE_PAGE_SIZE = 24;

/** The sort dropdown's options, in display order. */
export const SORT_OPTIONS: { value: SearchSort; label: string }[] = [
  { value: "relevance", label: "সেরা মিল — Best match" },
  { value: "newest", label: "নতুন আগে — Newest" },
  { value: "sold", label: "বেস্ট সেলার — Best selling" },
  { value: "price_asc", label: "দাম: কম থেকে বেশি — Price low to high" },
  { value: "price_desc", label: "দাম: বেশি থেকে কম — Price high to low" },
  { value: "rating", label: "রেটিং — Rating" },
];

const SORT_KEYS: SearchSort[] = [
  "relevance",
  "newest",
  "sold",
  "price_asc",
  "price_desc",
  "rating",
];

/** Split one raw parameter value into its list entries. */
function splitValues(raw: string | null): string[] {
  if (!raw) return [];
  return raw
    .split(",")
    .map((value) => value.trim())
    .filter((value) => value.length > 0);
}

/** Every value of a repeatable parameter, comma forms split, deduped. */
function allValues(params: URLSearchParams, key: string): string[] {
  const seen = new Set<string>();
  const values: string[] = [];
  for (const raw of params.getAll(key)) {
    for (const value of splitValues(raw)) {
      if (!seen.has(value)) {
        seen.add(value);
        values.push(value);
      }
    }
  }
  return values;
}

/** A poisha parameter: a non-negative integer or null. */
function poishaParam(params: URLSearchParams, key: string): number | null {
  const raw = params.get(key);
  if (!raw) return null;
  const value = Number(raw);
  return Number.isInteger(value) && value >= 0 ? value : null;
}

/** Read the browse state out of URL search params. */
export function browseStateFromParams(params: URLSearchParams): BrowseState {
  const q = params.get("q") ?? "";
  const ratingRaw = params.get("min_rating");
  const rating = ratingRaw ? Number(ratingRaw) : NaN;
  const sortRaw = params.get("sort") as SearchSort | null;
  const pageRaw = Number(params.get("page") ?? "1");
  const limitRaw = Number(params.get("limit") ?? "");
  return {
    q,
    categories: allValues(params, "category"),
    categorySlugs: allValues(params, "category_slug"),
    stores: allValues(params, "store"),
    storeSlug: params.get("store_slug") ?? "",
    minPricePoisha: poishaParam(params, "min_price_poisha"),
    maxPricePoisha: poishaParam(params, "max_price_poisha"),
    minRating:
      Number.isFinite(rating) && rating > 0 && rating <= 5 ? rating : null,
    inStock: params.get("in_stock") === "true",
    onSale: params.get("on_sale") === "true",
    sort:
      sortRaw && SORT_KEYS.includes(sortRaw)
        ? sortRaw
        : q
          ? "relevance"
          : "newest",
    page: Number.isInteger(pageRaw) && pageRaw >= 1 ? pageRaw : 1,
    limit:
      Number.isInteger(limitRaw) && limitRaw >= 1 && limitRaw <= 60
        ? limitRaw
        : BROWSE_PAGE_SIZE,
  };
}

/** The canonical URL params for a browse state. */
export function browseParamsFromState(state: BrowseState): URLSearchParams {
  const params = new URLSearchParams();
  if (state.q) params.set("q", state.q);
  for (const category of state.categories) params.append("category", category);
  for (const slug of state.categorySlugs) params.append("category_slug", slug);
  for (const store of state.stores) params.append("store", store);
  if (state.storeSlug) params.set("store_slug", state.storeSlug);
  if (state.minPricePoisha !== null)
    params.set("min_price_poisha", String(state.minPricePoisha));
  if (state.maxPricePoisha !== null)
    params.set("max_price_poisha", String(state.maxPricePoisha));
  if (state.minRating !== null)
    params.set("min_rating", String(state.minRating));
  if (state.inStock) params.set("in_stock", "true");
  if (state.onSale) params.set("on_sale", "true");
  params.set("sort", state.sort);
  if (state.page > 1) params.set("page", String(state.page));
  if (state.limit !== BROWSE_PAGE_SIZE)
    params.set("limit", String(state.limit));
  return params;
}

/** A browse URL for a state on a base path ("/browse" or "/search"). */
export function browseUrl(basePath: string, state: BrowseState): string {
  const qs = browseParamsFromState(state).toString();
  return qs ? `${basePath}?${qs}` : basePath;
}

/** The empty browse: no filters, default sort, first page. */
export function emptyBrowseState(): BrowseState {
  return {
    q: "",
    categories: [],
    categorySlugs: [],
    stores: [],
    storeSlug: "",
    minPricePoisha: null,
    maxPricePoisha: null,
    minRating: null,
    inStock: false,
    onSale: false,
    sort: "newest",
    page: 1,
    limit: BROWSE_PAGE_SIZE,
  };
}

/**
 * How many filter groups are actively narrowing the
 * browse (the query text and the sort do not count) —
 * drives the mobile drawer's badge and the reset button's
 * visibility.
 */
export function activeFilterCount(state: BrowseState): number {
  let count = 0;
  if (state.categories.length > 0) count += 1;
  if (state.categorySlugs.length > 0) count += 1;
  if (state.stores.length > 0 || state.storeSlug) count += 1;
  if (state.minPricePoisha !== null || state.maxPricePoisha !== null)
    count += 1;
  if (state.minRating !== null) count += 1;
  if (state.inStock) count += 1;
  if (state.onSale) count += 1;
  return count;
}

/**
 * The page-number window for the pagination controls: at
 * most seven numbers, centred on the current page, always
 * including the first and last stretch's edges.
 */
export function pageWindow(page: number, totalPages: number): number[] {
  if (totalPages <= 7) {
    return Array.from({ length: totalPages }, (_, i) => i + 1);
  }
  const start = Math.min(Math.max(1, page - 3), totalPages - 6);
  return Array.from({ length: 7 }, (_, i) => start + i);
}

/** A saved search's filters as a browse state (page 1). */
export function browseStateFromSaved(filters: SavedSearchFilters): BrowseState {
  const sort =
    filters.sort && SORT_KEYS.includes(filters.sort as SearchSort)
      ? (filters.sort as SearchSort)
      : filters.q
        ? "relevance"
        : "newest";
  return {
    q: filters.q ?? "",
    categories: filters.categories,
    categorySlugs: [],
    stores: filters.store_slugs,
    storeSlug: "",
    minPricePoisha: filters.min_price_poisha,
    maxPricePoisha: filters.max_price_poisha,
    minRating: filters.min_rating,
    inStock: filters.in_stock,
    onSale: filters.on_sale,
    sort,
    page: 1,
    limit: BROWSE_PAGE_SIZE,
  };
}

/**
 * A filter set as a readable one-liner ("shirt" · clothing ·
 * ৳100 – ৳500 · 4★+) — the saved-searches list and the
 * admin analytics combinations table speak the same words
 * for the same filters.
 */
export function describeFilters(filters: SavedSearchFilters): string {
  const parts: string[] = [];
  if (filters.q) parts.push(`“${filters.q}”`);
  if (filters.categories.length > 0)
    parts.push(filters.categories.join(", "));
  if (filters.store_slugs.length > 0)
    parts.push(`স্টোর: ${filters.store_slugs.join(", ")}`);
  if (filters.min_price_poisha !== null || filters.max_price_poisha !== null) {
    parts.push(
      `${formatBdt(filters.min_price_poisha ?? 0)} – ${
        filters.max_price_poisha !== null
          ? formatBdt(filters.max_price_poisha)
          : "∞"
      }`
    );
  }
  if (filters.min_rating !== null) parts.push(`${filters.min_rating}★+`);
  if (filters.in_stock) parts.push("স্টকে আছে");
  if (filters.on_sale) parts.push("অফারে আছে");
  return parts.length > 0 ? parts.join(" · ") : "সব পণ্য — Everything";
}

/** A browse state as storable saved-search filters. */
export function savedFiltersFromState(state: BrowseState): SavedSearchFilters {
  const stores = [...state.stores];
  if (state.storeSlug && !stores.includes(state.storeSlug)) {
    stores.push(state.storeSlug);
  }
  return {
    q: state.q || null,
    categories: state.categories,
    store_slugs: stores,
    min_price_poisha: state.minPricePoisha,
    max_price_poisha: state.maxPricePoisha,
    min_rating: state.minRating,
    in_stock: state.inStock,
    on_sale: state.onSale,
    sort: state.sort,
  };
}
