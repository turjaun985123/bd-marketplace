"use client";

import { Suspense, useEffect, useState } from "react";
import Link from "next/link";
import { useRouter, useSearchParams } from "next/navigation";
import ProductCard from "@/components/ProductCard";
import {
  searchProducts,
  type SearchResult,
  type SearchSort,
} from "@/lib/api";
import { parseTakaToPoisha } from "@/lib/format";
import { useAuth } from "@/hooks/useAuth";
import { useCart } from "@/hooks/useCart";

const SORT_OPTIONS: { value: SearchSort; label: string }[] = [
  { value: "relevance", label: "সেরা মিল — Best match" },
  { value: "newest", label: "নতুন আগে — Newest" },
  { value: "price_asc", label: "দাম: কম থেকে বেশি — Price low to high" },
  { value: "price_desc", label: "দাম: বেশি থেকে কম — Price high to low" },
  { value: "rating", label: "রেটিং — Rating" },
];

const PER_PAGE = 20;

/**
 * Search results (Wave 15): the URL query string is the state —
 * every filter change pushes a new URL, and the page fetches
 * /v1/search for exactly those parameters. Results render with
 * the shared ProductCard, and add-to-cart goes through the shared
 * useCart hook, exactly like the product grid.
 */
function SearchResultsView() {
  const router = useRouter();
  const searchParams = useSearchParams();
  const { token } = useAuth();
  const { add, error: cartError } = useCart();

  const q = searchParams.get("q") ?? "";
  const storeSlug = searchParams.get("store_slug") ?? "";
  const category = searchParams.get("category") ?? "";
  const minPoisha = searchParams.get("min_price_poisha");
  const maxPoisha = searchParams.get("max_price_poisha");
  const sort = (searchParams.get("sort") as SearchSort | null) ?? undefined;
  const page = Number(searchParams.get("page") ?? "1") || 1;

  // Filter form state, re-seeded whenever the URL changes (e.g.
  // arriving from the SearchBar with a fresh query).
  const [formQ, setFormQ] = useState(q);
  const [formCategory, setFormCategory] = useState(category);
  const [formMin, setFormMin] = useState(
    minPoisha ? String(Number(minPoisha) / 100) : ""
  );
  const [formMax, setFormMax] = useState(
    maxPoisha ? String(Number(maxPoisha) / 100) : ""
  );
  const [formSort, setFormSort] = useState<SearchSort>(sort ?? "relevance");
  const [formError, setFormError] = useState<string | null>(null);

  useEffect(() => {
    setFormQ(q);
    setFormCategory(category);
    setFormMin(minPoisha ? String(Number(minPoisha) / 100) : "");
    setFormMax(maxPoisha ? String(Number(maxPoisha) / 100) : "");
    setFormSort(sort ?? (q ? "relevance" : "newest"));
  }, [q, category, minPoisha, maxPoisha, sort]);

  const [results, setResults] = useState<SearchResult | null>(null);
  const [offline, setOffline] = useState(false);
  const [addedId, setAddedId] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    setResults(null);
    setOffline(false);
    searchProducts({
      q: q || undefined,
      store_slug: storeSlug || undefined,
      category: category || undefined,
      min_price_poisha: minPoisha ? Number(minPoisha) : undefined,
      max_price_poisha: maxPoisha ? Number(maxPoisha) : undefined,
      sort,
      page,
      per_page: PER_PAGE,
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
  }, [q, storeSlug, category, minPoisha, maxPoisha, sort, page]);

  function pushParams(next: Record<string, string | undefined>) {
    const params = new URLSearchParams();
    for (const [key, value] of Object.entries(next)) {
      if (value) params.set(key, value);
    }
    const qs = params.toString();
    router.push(qs ? `/search?${qs}` : "/search");
  }

  function onFilter(e: React.FormEvent) {
    e.preventDefault();
    const minText = formMin.trim();
    const maxText = formMax.trim();
    const min = minText ? parseTakaToPoisha(minText) : undefined;
    const max = maxText ? parseTakaToPoisha(maxText) : undefined;
    if (
      min === null ||
      max === null ||
      (min !== undefined && max !== undefined && min > max)
    ) {
      setFormError(
        "দামের ঘরে সঠিক টাকার পরিমাণ দিন (সর্বনিম্ন ≤ সর্বোচ্চ)। — Enter valid taka amounts, min ≤ max."
      );
      return;
    }
    setFormError(null);
    pushParams({
      q: formQ.trim() || undefined,
      store_slug: storeSlug || undefined,
      category: formCategory.trim() || undefined,
      min_price_poisha: min !== undefined ? String(min) : undefined,
      max_price_poisha: max !== undefined ? String(max) : undefined,
      sort: formSort,
    });
  }

  async function onAdd(productId: string) {
    setAddedId(null);
    await add(productId, 1);
    setAddedId(productId);
  }

  const totalPages =
    results !== null ? Math.max(1, Math.ceil(results.total / PER_PAGE)) : 1;

  return (
    <main>
      <h1>খুঁজুন — Search</h1>
      {storeSlug && (
        <p>
          স্টোর: <strong>{storeSlug}</strong>-এর মধ্যে খোঁজা হচ্ছে।{" "}
          <Link href="/search">সব স্টোরে খুঁজুন — Search all stores</Link>
        </p>
      )}

      <form onSubmit={onFilter}>
        <label>
          খুঁজুন — Keywords{" "}
          <input
            type="search"
            value={formQ}
            onChange={(e) => setFormQ(e.target.value)}
          />
        </label>{" "}
        <label>
          ক্যাটাগরি — Category{" "}
          <input
            type="text"
            value={formCategory}
            onChange={(e) => setFormCategory(e.target.value)}
            placeholder="যেমন: clothing"
          />
        </label>{" "}
        <label>
          সর্বনিম্ন দাম (৳) — Min{" "}
          <input
            type="text"
            inputMode="decimal"
            value={formMin}
            onChange={(e) => setFormMin(e.target.value)}
          />
        </label>{" "}
        <label>
          সর্বোচ্চ দাম (৳) — Max{" "}
          <input
            type="text"
            inputMode="decimal"
            value={formMax}
            onChange={(e) => setFormMax(e.target.value)}
          />
        </label>{" "}
        <label>
          সাজান — Sort{" "}
          <select
            value={formSort}
            onChange={(e) => setFormSort(e.target.value as SearchSort)}
          >
            {SORT_OPTIONS.map((option) => (
              <option key={option.value} value={option.value}>
                {option.label}
              </option>
            ))}
          </select>
        </label>{" "}
        <button type="submit">ফিল্টার করুন — Filter</button>
        {formError && <p role="alert">{formError}</p>}
      </form>

      {offline ? (
        <p>
          API-তে সংযোগ করা যাচ্ছে না — Rust API চালু আছে কিনা দেখুন। (Could
          not reach the API, so search cannot run.)
        </p>
      ) : results === null ? (
        <p>খোঁজা হচ্ছে… (Searching…)</p>
      ) : results.total === 0 ? (
        <p>
          কোনো পণ্য মেলেনি — অন্য শব্দ বা ফিল্টার চেষ্টা করুন। (No products
          matched.)
        </p>
      ) : (
        <div>
          <p>
            মোট {results.total}টি পণ্য — {results.total} products, page {page}{" "}
            of {totalPages}
          </p>
          {cartError && <p role="alert">{cartError}</p>}
          {results.items.map((p) => (
            <div key={p.id}>
              <ProductCard
                name={p.title_bn ?? p.title}
                storeName={p.store_slug}
                pricePoisha={p.price_poisha}
                href={`/products/${p.id}`}
                ratingAvg={p.rating_avg}
                ratingCount={p.rating_count}
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
          <p>
            {page > 1 && (
              <button
                type="button"
                onClick={() =>
                  pushParams({
                    q: q || undefined,
                    store_slug: storeSlug || undefined,
                    category: category || undefined,
                    min_price_poisha: minPoisha ?? undefined,
                    max_price_poisha: maxPoisha ?? undefined,
                    sort,
                    page: String(page - 1),
                  })
                }
              >
                ← আগের পাতা — Previous
              </button>
            )}{" "}
            {page < totalPages && (
              <button
                type="button"
                onClick={() =>
                  pushParams({
                    q: q || undefined,
                    store_slug: storeSlug || undefined,
                    category: category || undefined,
                    min_price_poisha: minPoisha ?? undefined,
                    max_price_poisha: maxPoisha ?? undefined,
                    sort,
                    page: String(page + 1),
                  })
                }
              >
                পরের পাতা — Next →
              </button>
            )}
          </p>
        </div>
      )}
    </main>
  );
}

export default function SearchPage() {
  return (
    <Suspense fallback={<p>লোড হচ্ছে… (Loading…)</p>}>
      <SearchResultsView />
    </Suspense>
  );
}
