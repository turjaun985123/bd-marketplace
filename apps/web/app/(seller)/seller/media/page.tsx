"use client";

import { useCallback, useEffect, useState } from "react";
import Link from "next/link";
import {
  bulkDeleteSellerMedia,
  listSellerMedia,
  type SellerMediaItem,
} from "@/lib/api";
import { useAuth } from "@/hooks/useAuth";

/**
 * Seller media library (Wave 51): every product image across
 * every store the signed-in seller can manage, in one grid —
 * with bulk-select delete for cleaning up dead photos. Per-
 * product editing (order, primary, alt text) lives in the
 * products page's image manager; this page is the warehouse
 * view: what exists, where it hangs, and how much storage the
 * uploads take.
 */
export default function SellerMediaPage() {
  const { ready, token } = useAuth();
  const [items, setItems] = useState<SellerMediaItem[] | null>(null);
  const [selected, setSelected] = useState<Record<string, boolean>>({});
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const [usage, setUsage] = useState<{ bytes: number; quota: number } | null>(null);
  const [sourceFilter, setSourceFilter] = useState<"" | "upload" | "external">("");
  const [sortOrder, setSortOrder] = useState<"newest" | "oldest" | "largest">("newest");
  const [productFilter, setProductFilter] = useState("");
  // The product options come from the seller's own media:
  // every product that HAS an image, seen on an unfiltered
  // load, so the select never loses the current choice
  // when a filter narrows the grid.
  const [productOptions, setProductOptions] = useState<
    { id: string; title: string }[]
  >([]);

  const reload = useCallback(async () => {
    if (!token) return;
    try {
      const page = await listSellerMedia(
        token,
        productFilter || undefined,
        sourceFilter || undefined,
        sortOrder
      );
      setItems(page.items);
      setUsage({ bytes: page.storage_bytes, quota: page.storage_quota_bytes });
      setSelected({});
      if (!productFilter && !sourceFilter) {
        const seen = new Map<string, string>();
        for (const item of page.items) {
          if (!seen.has(item.product_id)) {
            seen.set(item.product_id, item.product_title);
          }
        }
        setProductOptions(
          Array.from(seen, ([id, title]) => ({ id, title })).sort((a, b) =>
            a.title.localeCompare(b.title)
          )
        );
      }
    } catch (err) {
      setError(err instanceof Error ? err.message : "লোড হয়নি — Load failed");
    }
  }, [token, sourceFilter, productFilter, sortOrder]);

  useEffect(() => {
    void reload();
  }, [reload]);

  const selectedIds = Object.keys(selected).filter((id) => selected[id]);
  const storedBytes = (items ?? [])
    .filter((item) => item.source === "upload")
    .reduce((sum, item) => sum + (item.size_bytes ?? 0), 0);

  const bulkDelete = async () => {
    if (!token || selectedIds.length === 0) return;
    if (
      !window.confirm(
        `${selectedIds.length}টি ছবি মুছে ফেলবেন? — Delete ${selectedIds.length} image(s)?`
      )
    ) {
      return;
    }
    setBusy(true);
    setError(null);
    try {
      const result = await bulkDeleteSellerMedia(token, selectedIds);
      setNotice(
        `${result.deleted}টি মোছা হয়েছে — ${result.deleted} deleted` +
          (result.skipped > 0 ? `, ${result.skipped}টি এড়ানো হয়েছে — ${result.skipped} skipped` : "")
      );
      await reload();
    } catch (err) {
      setError(err instanceof Error ? err.message : "মোছা যায়নি — Delete failed");
    } finally {
      setBusy(false);
    }
  };

  if (!ready) {
    return (
      <main>
        <p>লোড হচ্ছে…</p>
      </main>
    );
  }
  if (!token) {
    return (
      <main>
        <h1>মিডিয়া লাইব্রেরি — Media library</h1>
        <p>
          দেখতে <Link href="/login">লগইন করুন</Link>।
        </p>
      </main>
    );
  }

  return (
    <main>
      <h1>মিডিয়া লাইব্রেরি — Media library</h1>
      <p>
        <Link href="/seller">← বিক্রেতা ড্যাশবোর্ড</Link>
        {" · "}
        <Link href="/seller/products">পণ্য — Products</Link>
      </p>
      {error && <p role="alert">{error}</p>}
      {notice && <p role="status">{notice}</p>}
      <p>
        <label>
          ধরন — Source{" "}
          <select
            value={sourceFilter}
            onChange={(event) =>
              setSourceFilter(event.target.value as "" | "upload" | "external")
            }
          >
            <option value="">সব — All</option>
            <option value="upload">আপলোড — Uploaded</option>
            <option value="external">বাইরের URL — External</option>
          </select>
        </label>{" "}
        <label>
          পণ্য — Product{" "}
          <select
            value={productFilter}
            onChange={(event) => setProductFilter(event.target.value)}
          >
            <option value="">সব পণ্য — All products</option>
            {productOptions.map((option) => (
              <option key={option.id} value={option.id}>
                {option.title}
              </option>
            ))}
          </select>
        </label>{" "}
        <label>
          সাজান — Sort{" "}
          <select
            value={sortOrder}
            onChange={(event) =>
              setSortOrder(event.target.value as "newest" | "oldest" | "largest")
            }
          >
            <option value="newest">নতুন আগে — Newest</option>
            <option value="oldest">পুরনো আগে — Oldest</option>
            <option value="largest">বড় ফাইল আগে — Largest</option>
          </select>
        </label>
      </p>
      {items === null ? (
        <p>লোড হচ্ছে…</p>
      ) : items.length === 0 ? (
        <p>
          এখনো কোনো ছবি নেই — পণ্যের পেজ থেকে ছবি যোগ করুন। (No images yet —
          add them from the products page.)
        </p>
      ) : (
        <>
          <p>
            মোট {items.length}টি ছবি — {items.length} images · আপলোড করা
            ফাইলের আকার {(storedBytes / 1024).toFixed(1)} KB — stored uploads
          </p>
          {usage && (
            <p>
              স্টোরেজ ব্যবহার — Storage: {(usage.bytes / (1024 * 1024)).toFixed(1)} MB
              {" / "}
              {(usage.quota / (1024 * 1024)).toFixed(0)} MB প্রতি স্টোর — per store
              <progress
                className="media-library-usage"
                max={usage.quota}
                value={usage.bytes}
                aria-label="স্টোরেজ ব্যবহার — Storage usage"
              />
            </p>
          )}
          <p>
            <button
              type="button"
              disabled={busy || items.length === 0}
              onClick={() => {
                if (selectedIds.length === items.length) {
                  setSelected({});
                } else {
                  setSelected(
                    Object.fromEntries(items.map((item) => [item.id, true]))
                  );
                }
              }}
            >
              {selectedIds.length === items.length && items.length > 0
                ? "নির্বাচন ছাড়ুন — Clear selection"
                : "সব নির্বাচন করুন — Select all"}
            </button>{" "}
            <button
              type="button"
              disabled={busy || selectedIds.length === 0}
              onClick={() => void bulkDelete()}
            >
              নির্বাচিত মোছুন — Delete selected ({selectedIds.length})
            </button>
          </p>
          <ul className="media-library-grid">
            {items.map((item) => (
              <li key={item.id} className="media-library-item">
                <label className="media-library-select">
                  <input
                    type="checkbox"
                    checked={!!selected[item.id]}
                    onChange={(event) =>
                      setSelected((current) => ({
                        ...current,
                        [item.id]: event.target.checked,
                      }))
                    }
                  />
                  নির্বাচন — Select
                </label>
                {/* eslint-disable-next-line @next/next/no-img-element */}
                <img
                  src={item.url_card}
                  alt={item.alt_text ?? item.product_title}
                  loading="lazy"
                  className="media-library-thumb"
                />
                <p className="media-library-meta">
                  <Link href={`/products/${item.product_id}`}>
                    {item.product_title}
                  </Link>
                  <br />
                  {item.source === "upload"
                    ? `${item.width ?? "?"}×${item.height ?? "?"} · ${((item.size_bytes ?? 0) / 1024).toFixed(1)} KB`
                    : "বাইরের URL — External"}
                  {item.is_primary ? " · ★ প্রধান — Primary" : ""}
                  {item.original_filename ? (
                    <>
                      <br />
                      {item.original_filename}
                    </>
                  ) : null}
                  {item.warnings.length > 0 ? (
                    <>
                      <br />
                      <span className="image-uploader-warning">
                        ⚠{" "}
                        {item.warnings
                          .map(
                            (code) =>
                              ({
                                low_resolution: "ছোট ছবি — Low resolution",
                                extreme_aspect: "অনুপাত — Extreme aspect",
                                very_large: "অনেক বড় — Very large",
                              })[code] ?? code
                          )
                          .join(" · ")}
                      </span>
                    </>
                  ) : null}
                </p>
              </li>
            ))}
          </ul>
        </>
      )}
    </main>
  );
}
