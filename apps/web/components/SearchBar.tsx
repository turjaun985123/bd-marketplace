"use client";

import { useEffect, useRef, useState } from "react";
import Link from "next/link";
import { useRouter } from "next/navigation";
import {
  getPopularSearches,
  searchSuggest,
  type PopularSearch,
  type SearchSuggestions,
} from "@/lib/api";
import { formatBdt } from "@/lib/format";
import {
  recordRecentSearch,
  useRecentSearches,
} from "@/hooks/useRecentSearches";

/**
 * Shared search box (Wave 15; Wave 53's dropdown):
 * submits to the browse page and shows typeahead
 * suggestions from /v1/search/suggest while typing
 * (debounced; the API itself stays silent below 2
 * characters). Product suggestions render as mini rows —
 * thumbnail, title, category and the EFFECTIVE price
 * (suggest v2) — and the whole list is keyboard
 * navigable: ↑/↓ move, Enter opens the active row (or
 * submits when none is active), Escape closes. Clicking
 * outside closes. With `storeSlug` the search is scoped
 * to that store — the storefront's within-store search,
 * which still lands on /search with the scope in the URL.
 * Suggestions are a convenience only: submitting always
 * works, even when the suggestion call fails.
 */
export default function SearchBar({ storeSlug }: { storeSlug?: string }) {
  const router = useRouter();
  const [text, setText] = useState("");
  const [suggestions, setSuggestions] = useState<SearchSuggestions | null>(
    null
  );
  const [open, setOpen] = useState(false);
  const [activeIndex, setActiveIndex] = useState(-1);
  const [popular, setPopular] = useState<PopularSearch[] | null>(null);
  const { searches: recentSearches, clear: clearRecentSearches } =
    useRecentSearches();
  const debounce = useRef<ReturnType<typeof setTimeout> | null>(null);
  const rootRef = useRef<HTMLDivElement | null>(null);

  // The popular list loads lazily on first focus (an
  // empty search box shows what everyone is looking for);
  // a failed fetch simply leaves the box plain. The list
  // is marketplace-wide, so a store-scoped box skips it.
  function loadPopular() {
    if (storeSlug || popular !== null) return;
    getPopularSearches()
      .then((result) => setPopular(result.searches))
      .catch(() => setPopular([]));
  }

  useEffect(() => {
    if (debounce.current) clearTimeout(debounce.current);
    const q = text.trim();
    if (q.length < 2) {
      setSuggestions(null);
      setActiveIndex(-1);
      return;
    }
    debounce.current = setTimeout(() => {
      searchSuggest(q)
        .then((result) => {
          setSuggestions(result);
          setActiveIndex(-1);
          setOpen(true);
        })
        .catch(() => setSuggestions(null));
    }, 250);
    return () => {
      if (debounce.current) clearTimeout(debounce.current);
    };
  }, [text]);

  // Click-outside closes the dropdown.
  useEffect(() => {
    if (!open) return;
    function onPointerDown(event: PointerEvent) {
      if (
        rootRef.current &&
        !rootRef.current.contains(event.target as Node)
      ) {
        setOpen(false);
      }
    }
    document.addEventListener("pointerdown", onPointerDown);
    return () => document.removeEventListener("pointerdown", onPointerDown);
  }, [open ]);

  // The idle dropdown (empty or near-empty box): the
  // visitor's own recent searches first (device-local),
  // then the marketplace's popular searches (aggregate,
  // and marketplace-wide — a store-scoped box skips them).
  type IdleRow = { key: string; label: string; sub?: string; href: string };
  const recentRows: IdleRow[] = recentSearches.map((query) => {
    const params = new URLSearchParams();
    params.set("q", query);
    if (storeSlug) params.set("store_slug", storeSlug);
    return {
      key: `recent:${query}`,
      label: query,
      href: storeSlug
        ? `/search?${params.toString()}`
        : `/browse?${params.toString()}`,
    };
  });
  const popularRows: IdleRow[] =
    !storeSlug && popular
      ? popular.map((row) => ({
          key: `popular:${row.query_text}`,
          label: row.query_text,
          sub: `${row.searches} বার খোঁজা হয়েছে — searches`,
          href: `/browse?q=${encodeURIComponent(row.query_text)}`,
        }))
      : [];
  const idleRows: IdleRow[] = [...recentRows, ...popularRows];
  const showIdle = open && text.trim().length < 2 && idleRows.length > 0;
  const productCount = suggestions?.products.length ?? 0;
  const categoryCount = suggestions?.categories.length ?? 0;
  const rowCount = showIdle
    ? idleRows.length
    : productCount + categoryCount + (suggestions?.stores.length ?? 0);

  function rowHref(index: number): string | null {
    if (showIdle) {
      return idleRows[index]?.href ?? null;
    }
    if (!suggestions) return null;
    if (index < productCount) {
      return `/products/${suggestions.products[index].id}`;
    }
    if (index < productCount + categoryCount) {
      const category = suggestions.categories[index - productCount];
      if (!category) return null;
      const params = new URLSearchParams();
      const q = text.trim();
      if (q) params.set("q", q);
      params.set("category", category.category);
      if (storeSlug) {
        params.set("store_slug", storeSlug);
        return `/search?${params.toString()}`;
      }
      return `/browse?${params.toString()}`;
    }
    const store = suggestions.stores[index - productCount - categoryCount];
    return store ? `/${store.slug}` : null;
  }

  function onSubmit(e: React.FormEvent) {
    e.preventDefault();
    const params = new URLSearchParams();
    const q = text.trim();
    if (q) {
      params.set("q", q);
      recordRecentSearch(q);
    }
    if (storeSlug) params.set("store_slug", storeSlug);
    const qs = params.toString();
    setOpen(false);
    if (storeSlug) {
      router.push(qs ? `/search?${qs}` : "/search");
    } else {
      router.push(qs ? `/browse?${qs}` : "/browse");
    }
  }

  function onKeyDown(e: React.KeyboardEvent) {
    if (e.key === "Escape") {
      setOpen(false);
      setActiveIndex(-1);
      return;
    }
    if (!open || rowCount === 0) return;
    if (e.key === "ArrowDown") {
      e.preventDefault();
      setActiveIndex((index) => (index + 1) % rowCount);
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setActiveIndex((index) => (index <= 0 ? rowCount - 1 : index - 1));
    } else if (e.key === "Enter" && activeIndex >= 0) {
      const href = rowHref(activeIndex);
      if (href) {
        e.preventDefault();
        setOpen(false);
        router.push(href);
      }
    }
  }

  const hasSuggestions =
    suggestions !== null &&
    (suggestions.products.length > 0 ||
      suggestions.categories.length > 0 ||
      suggestions.stores.length > 0);

  return (
    <div ref={rootRef} className="searchbar">
      <form onSubmit={onSubmit} role="search">
        <label>
          পণ্য খুঁজুন — Search products{" "}
          <input
            type="search"
            value={text}
            onChange={(e) => setText(e.target.value)}
            onFocus={() => {
              setOpen(true);
              loadPopular();
            }}
            onKeyDown={onKeyDown}
            placeholder={
              storeSlug
                ? "এই স্টোরে খুঁজুন — Search this store"
                : "যেমন: শার্ট, shirt, জুতা…"
            }
            aria-expanded={open && hasSuggestions}
            aria-controls="searchbar-suggestions"
          />
        </label>{" "}
        <button type="submit">খুঁজুন — Search</button>
      </form>
      {showIdle && (
        <div className="card searchbar-dropdown" id="searchbar-suggestions">
          {recentRows.length > 0 && (
            <p className="searchbar-dropdown-title">
              সাম্প্রতিক খোঁজ — Recent searches{" "}
              <button
                type="button"
                className="searchbar-clear"
                onClick={clearRecentSearches}
              >
                মুছে ফেলুন — Clear
              </button>
            </p>
          )}
          {recentRows.map((row, index) => (
            <Link
              key={row.key}
              href={row.href}
              className={`searchbar-row${index === activeIndex ? " is-active" : ""}`}
              onClick={() => setOpen(false)}
              onMouseEnter={() => setActiveIndex(index)}
            >
              <span className="searchbar-row-main">
                <span className="searchbar-row-title">{row.label}</span>
              </span>
            </Link>
          ))}
          {popularRows.length > 0 && (
            <p className="searchbar-dropdown-title">
              জনপ্রিয় খোঁজ — Popular searches
            </p>
          )}
          {popularRows.map((row, offset) => {
            const index = recentRows.length + offset;
            return (
              <Link
                key={row.key}
                href={row.href}
                className={`searchbar-row${index === activeIndex ? " is-active" : ""}`}
                onClick={() => setOpen(false)}
                onMouseEnter={() => setActiveIndex(index)}
              >
                <span className="searchbar-row-main">
                  <span className="searchbar-row-title">{row.label}</span>
                  {row.sub && (
                    <span className="searchbar-row-category">{row.sub}</span>
                  )}
                </span>
              </Link>
            );
          })}
        </div>
      )}
      {open && hasSuggestions && !showIdle && (
        <div className="card searchbar-dropdown" id="searchbar-suggestions">
          {suggestions.products.map((p, index) => (
            <Link
              key={p.id}
              href={`/products/${p.id}`}
              className={`searchbar-row${index === activeIndex ? " is-active" : ""}`}
              onClick={() => setOpen(false)}
              onMouseEnter={() => setActiveIndex(index)}
            >
              {p.image_url ? (
                // eslint-disable-next-line @next/next/no-img-element
                <img
                  src={p.image_url}
                  alt=""
                  className="searchbar-thumb"
                  loading="lazy"
                />
              ) : (
                <span className="searchbar-thumb product-image-placeholder" aria-hidden="true">
                  {p.title.slice(0, 1)}
                </span>
              )}
              <span className="searchbar-row-main">
                <span className="searchbar-row-title">{p.title}</span>
                {p.category && (
                  <span className="searchbar-row-category">{p.category}</span>
                )}
              </span>
              <span className="searchbar-row-price">
                {formatBdt(p.price_poisha)}
              </span>
            </Link>
          ))}
          {suggestions.categories.map((c, offset) => {
            const href = rowHref(productCount + offset);
            return (
              <Link
                key={c.category}
                href={href ?? "/browse"}
                className={`searchbar-row${productCount + offset === activeIndex ? " is-active" : ""}`}
                onClick={() => setOpen(false)}
                onMouseEnter={() => setActiveIndex(productCount + offset)}
              >
                <span className="searchbar-row-main">
                  <span className="searchbar-row-title">
                    {c.category} ক্যাটাগরিতে — in {c.category}
                  </span>
                  <span className="searchbar-row-category">
                    {c.product_count}টি পণ্য — products
                  </span>
                </span>
              </Link>
            );
          })}
          {suggestions.stores.map((s, offset) => (
            <Link
              key={s.slug}
              href={`/${s.slug}`}
              className={`searchbar-row${productCount + categoryCount + offset === activeIndex ? " is-active" : ""}`}
              onClick={() => setOpen(false)}
              onMouseEnter={() =>
                setActiveIndex(productCount + categoryCount + offset)
              }
            >
              <span className="searchbar-row-main">
                <span className="searchbar-row-title">স্টোর: {s.name}</span>
              </span>
            </Link>
          ))}
        </div>
      )}
    </div>
  );
}
