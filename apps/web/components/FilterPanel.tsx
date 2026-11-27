"use client";

import { useEffect, useState } from "react";
import type { SearchFacets } from "@/lib/api";
import { activeFilterCount, type BrowseState } from "@/lib/browse";
import { formatBdt, parseTakaToPoisha } from "@/lib/format";

/**
 * The browse filter panel (Wave 53) — ONE component for
 * the desktop sidebar and the mobile drawer, so the two
 * can never drift. It is controlled: the URL-driven
 * BrowseState comes in, and every control reports the
 * next state up; the parent (BrowseResults) writes it to
 * the URL, which re-fetches and re-renders.
 *
 * The checkbox lists and counts come from the response's
 * facet block, which counts each group against the OTHER
 * filters — so the category list stays wide while a
 * category is picked (that is how a shopper widens), and
 * a selected value missing from the facet list (count 0
 * under the other filters) is still rendered, checked, so
 * it can be unpicked.
 */
export default function FilterPanel({
  state,
  facets,
  onChange,
  hideStores = false,
  hideCategories = false,
}: {
  state: BrowseState;
  facets: SearchFacets | null;
  onChange: (next: BrowseState) => void;
  /** Hide the store group (the storefront shop page pins the store). */
  hideStores?: boolean;
  /** Hide the category group (the category landing pins the category). */
  hideCategories?: boolean;
}) {
  const [minText, setMinText] = useState(
    state.minPricePoisha !== null ? String(state.minPricePoisha / 100) : ""
  );
  const [maxText, setMaxText] = useState(
    state.maxPricePoisha !== null ? String(state.maxPricePoisha / 100) : ""
  );
  const [priceError, setPriceError] = useState<string | null>(null);

  // Re-seed the price inputs when the URL state changes
  // underneath them (chip removal, reset, a saved search).
  useEffect(() => {
    setMinText(
      state.minPricePoisha !== null ? String(state.minPricePoisha / 100) : ""
    );
    setMaxText(
      state.maxPricePoisha !== null ? String(state.maxPricePoisha / 100) : ""
    );
    setPriceError(null);
  }, [state.minPricePoisha, state.maxPricePoisha]);

  function toggleListValue(
    key: "categories" | "stores",
    value: string,
    checked: boolean
  ) {
    const current = state[key];
    const next = checked
      ? [...current, value]
      : current.filter((entry) => entry !== value);
    onChange({ ...state, [key]: next, page: 1 });
  }

  function applyPrice(e: React.FormEvent) {
    e.preventDefault();
    const min = minText.trim() ? parseTakaToPoisha(minText.trim()) : undefined;
    const max = maxText.trim() ? parseTakaToPoisha(maxText.trim()) : undefined;
    if (
      min === null ||
      max === null ||
      (min !== undefined && max !== undefined && min > max)
    ) {
      setPriceError(
        "দামের ঘরে সঠিক টাকার পরিমাণ দিন (সর্বনিম্ন ≤ সর্বোচ্চ)। — Enter valid taka amounts, min ≤ max."
      );
      return;
    }
    setPriceError(null);
    onChange({
      ...state,
      minPricePoisha: min ?? null,
      maxPricePoisha: max ?? null,
      page: 1,
    });
  }

  const categoryRows: { value: string; count: number | null }[] = [
    ...(facets?.categories.map((facet) => ({
      value: facet.category,
      count: facet.count as number | null,
    })) ?? []),
    ...state.categories
      .filter(
        (category) =>
          !(facets?.categories.some((facet) => facet.category === category))
      )
      .map((category) => ({ value: category, count: 0 as number | null })),
  ];
  const storeRows: { value: string; label: string; count: number | null }[] = [
    ...(facets?.stores.map((facet) => ({
      value: facet.slug,
      label: facet.name,
      count: facet.count as number | null,
    })) ?? []),
    ...state.stores
      .filter(
        (slug) => !(facets?.stores.some((facet) => facet.slug === slug))
      )
      .map((slug) => ({ value: slug, label: slug, count: 0 as number | null })),
  ];
  const ratingCount = (threshold: number) =>
    facets?.ratings.find((facet) => facet.min_rating === threshold)?.count;

  return (
    <div className="filter-panel">
      {!hideCategories && (
      <section className="filter-group">
        <h3>ক্যাটাগরি — Category</h3>
        {categoryRows.length === 0 ? (
          <p className="filter-empty">কোনো ক্যাটাগরি নেই — None</p>
        ) : (
          categoryRows.map((row) => (
            <label key={row.value} className="filter-check">
              <input
                type="checkbox"
                checked={state.categories.includes(row.value)}
                onChange={(e) =>
                  toggleListValue("categories", row.value, e.target.checked)
                }
              />{" "}
              {row.value}
              {row.count !== null && (
                <span className="filter-count"> ({row.count})</span>
              )}
            </label>
          ))
        )}
      </section>
      )}

      {!hideStores && (
      <section className="filter-group">
        <h3>স্টোর — Store</h3>
        {storeRows.length === 0 ? (
          <p className="filter-empty">কোনো স্টোর নেই — None</p>
        ) : (
          storeRows.map((row) => (
            <label key={row.value} className="filter-check">
              <input
                type="checkbox"
                checked={state.stores.includes(row.value)}
                onChange={(e) =>
                  toggleListValue("stores", row.value, e.target.checked)
                }
              />{" "}
              {row.label}
              {row.count !== null && (
                <span className="filter-count"> ({row.count})</span>
              )}
            </label>
          ))
        )}
      </section>
      )}

      <section className="filter-group">
        <h3>দাম — Price</h3>
        {facets && facets.price.min_poisha !== null && (
          <p className="filter-hint">
            এই সেটে {formatBdt(facets.price.min_poisha)} –{" "}
            {formatBdt(facets.price.max_poisha ?? 0)}
          </p>
        )}
        {facets && facets.price.histogram.length > 1 && (
          <div
            className="filter-histogram"
            role="group"
            aria-label="Price distribution"
          >
            {facets.price.histogram.map((bucket) => {
              const tallest = Math.max(
                ...facets.price.histogram.map((entry) => entry.count),
                1
              );
              const active =
                state.minPricePoisha === bucket.min_poisha &&
                state.maxPricePoisha === bucket.max_poisha;
              return (
                <button
                  key={`${bucket.min_poisha}-${bucket.max_poisha}`}
                  type="button"
                  className={`filter-histogram-bar${active ? " is-active" : ""}`}
                  title={`${formatBdt(bucket.min_poisha)} – ${formatBdt(bucket.max_poisha)}: ${bucket.count}টি`}
                  onClick={() =>
                    // Clicking the active band again clears
                    // it — a band is a toggle, not a trap.
                    onChange(
                      active
                        ? {
                            ...state,
                            minPricePoisha: null,
                            maxPricePoisha: null,
                            page: 1,
                          }
                        : {
                            ...state,
                            minPricePoisha: bucket.min_poisha,
                            maxPricePoisha: bucket.max_poisha,
                            page: 1,
                          }
                    )
                  }
                >
                  <span
                    className="filter-histogram-fill"
                    style={{
                      height: `${Math.max(8, Math.round((bucket.count / tallest) * 100))}%`,
                    }}
                  />
                  <span className="filter-histogram-count">{bucket.count}</span>
                </button>
              );
            })}
          </div>
        )}
        <form onSubmit={applyPrice} className="filter-price">
          <label>
            সর্বনিম্ন (৳){" "}
            <input
              type="text"
              inputMode="decimal"
              value={minText}
              onChange={(e) => setMinText(e.target.value)}
              placeholder={
                facets && facets.price.min_poisha != null
                  ? String(facets.price.min_poisha / 100)
                  : "0"
              }
            />
          </label>
          <label>
            সর্বোচ্চ (৳){" "}
            <input
              type="text"
              inputMode="decimal"
              value={maxText}
              onChange={(e) => setMaxText(e.target.value)}
              placeholder={
                facets && facets.price.max_poisha != null
                  ? String(facets.price.max_poisha / 100)
                  : "যেকোনো"
              }
            />
          </label>
          <button type="submit">প্রয়োগ — Apply</button>
          {priceError && (
            <p role="alert" className="merch-error">
              {priceError}
            </p>
          )}
        </form>
      </section>

      <section className="filter-group">
        <h3>রেটিং — Rating</h3>
        <label className="filter-check">
          <input
            type="radio"
            name="browse-min-rating"
            checked={state.minRating === null}
            onChange={() => onChange({ ...state, minRating: null, page: 1 })}
          />{" "}
          যেকোনো — Any
        </label>
        {[4, 3, 2].map((threshold) => (
          <label key={threshold} className="filter-check">
            <input
              type="radio"
              name="browse-min-rating"
              checked={state.minRating === threshold}
              onChange={() =>
                onChange({ ...state, minRating: threshold, page: 1 })
              }
            />{" "}
            {threshold}★ ও বেশি
            {ratingCount(threshold) !== undefined && (
              <span className="filter-count">
                {" "}
                ({ratingCount(threshold)})
              </span>
            )}
          </label>
        ))}
      </section>

      <section className="filter-group">
        <h3>অন্যান্য — More</h3>
        <label className="filter-check">
          <input
            type="checkbox"
            checked={state.inStock}
            onChange={(e) =>
              onChange({ ...state, inStock: e.target.checked, page: 1 })
            }
          />{" "}
          স্টকে আছে — In stock only
        </label>
        <label className="filter-check">
          <input
            type="checkbox"
            checked={state.onSale}
            onChange={(e) =>
              onChange({ ...state, onSale: e.target.checked, page: 1 })
            }
          />{" "}
          অফার চলছে — On sale only
        </label>
      </section>

      {activeFilterCount(state) > 0 && (
        <button
          type="button"
          className="filter-reset"
          onClick={() =>
            onChange({
              ...state,
              categories: [],
              stores: [],
              storeSlug: "",
              minPricePoisha: null,
              maxPricePoisha: null,
              minRating: null,
              inStock: false,
              onSale: false,
              page: 1,
            })
          }
        >
          সব ফিল্টার মুছে ফেলুন — Clear all filters
        </button>
      )}
    </div>
  );
}
