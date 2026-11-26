"use client";

import { useEffect, useRef, useState } from "react";
import Link from "next/link";
import { useRouter } from "next/navigation";
import { searchSuggest, type SearchSuggestions } from "@/lib/api";

/**
 * Shared search box (Wave 15): submits to the /search results
 * page and shows typeahead suggestions from /v1/search/suggest
 * while typing (debounced; the API itself stays silent below 2
 * characters). With `storeSlug` the search is scoped to that
 * store — the storefront's within-store search. Suggestions are a
 * convenience only: submitting always works, even when the
 * suggestion call fails or the API is unreachable.
 */
export default function SearchBar({ storeSlug }: { storeSlug?: string }) {
  const router = useRouter();
  const [text, setText] = useState("");
  const [suggestions, setSuggestions] = useState<SearchSuggestions | null>(
    null
  );
  const [open, setOpen] = useState(false);
  const debounce = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(() => {
    if (debounce.current) clearTimeout(debounce.current);
    const q = text.trim();
    if (q.length < 2) {
      setSuggestions(null);
      return;
    }
    debounce.current = setTimeout(() => {
      searchSuggest(q)
        .then((result) => {
          setSuggestions(result);
          setOpen(true);
        })
        .catch(() => setSuggestions(null));
    }, 250);
    return () => {
      if (debounce.current) clearTimeout(debounce.current);
    };
  }, [text]);

  function onSubmit(e: React.FormEvent) {
    e.preventDefault();
    const params = new URLSearchParams();
    const q = text.trim();
    if (q) params.set("q", q);
    if (storeSlug) params.set("store_slug", storeSlug);
    const qs = params.toString();
    setOpen(false);
    router.push(qs ? `/search?${qs}` : "/search");
  }

  const hasSuggestions =
    suggestions !== null &&
    (suggestions.products.length > 0 || suggestions.stores.length > 0);

  return (
    <div>
      <form onSubmit={onSubmit} role="search">
        <label>
          পণ্য খুঁজুন — Search products{" "}
          <input
            type="search"
            value={text}
            onChange={(e) => setText(e.target.value)}
            onFocus={() => setOpen(true)}
            placeholder={
              storeSlug
                ? "এই স্টোরে খুঁজুন — Search this store"
                : "যেমন: শার্ট, shirt, জুতা…"
            }
          />
        </label>{" "}
        <button type="submit">খুঁজুন — Search</button>
      </form>
      {open && hasSuggestions && (
        <div className="card">
          {suggestions.products.map((p) => (
            <p key={p.id}>
              <Link href={`/products/${p.id}`} onClick={() => setOpen(false)}>
                {p.title}
              </Link>
            </p>
          ))}
          {suggestions.stores.map((s) => (
            <p key={s.slug}>
              স্টোর:{" "}
              <Link href={`/${s.slug}`} onClick={() => setOpen(false)}>
                {s.name}
              </Link>
            </p>
          ))}
        </div>
      )}
    </div>
  );
}
