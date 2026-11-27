"use client";

import { useCallback, useEffect, useState } from "react";
import Link from "next/link";
import {
  deleteAdminMedia,
  listAdminMedia,
  type AdminMediaItem,
} from "@/lib/api";
import { useAuth } from "@/hooks/useAuth";

/**
 * Admin media moderation (Wave 51 extension): the newest
 * product images across every store, with a delete that
 * goes through the audited admin path — the platform's
 * answer to a policy-breaking photo. Sellers keep their
 * own library (/seller/media); this page exists because
 * moderation cannot wait for a seller to act.
 */
export default function AdminMediaPage() {
  const { ready, token } = useAuth();
  const [items, setItems] = useState<AdminMediaItem[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [storeFilter, setStoreFilter] = useState("");

  const reload = useCallback(async () => {
    if (!token) return;
    try {
      setItems(await listAdminMedia(token, storeFilter.trim() || undefined));
    } catch (err) {
      setError(err instanceof Error ? err.message : "লোড হয়নি — Load failed");
    }
  }, [token, storeFilter]);

  useEffect(() => {
    void reload();
  }, [reload]);

  const remove = async (item: AdminMediaItem) => {
    if (!token) return;
    if (
      !window.confirm(
        `এই ছবিটি প্ল্যাটফর্ম থেকে মুছে ফেলবেন? — Remove this image platform-wide?\n${item.product_title} (${item.store_slug})`
      )
    ) {
      return;
    }
    setBusy(true);
    setError(null);
    try {
      await deleteAdminMedia(token, item.id);
      setNotice("ছবি মোছা হয়েছে (অডিট লগে লেখা হয়েছে) — Image removed (audit-logged).");
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
        <h1>ছবি মডারেশন — Media moderation</h1>
        <p>
          দেখতে <Link href="/login">লগইন করুন</Link>।
        </p>
      </main>
    );
  }

  return (
    <main>
      <h1>ছবি মডারেশন — Media moderation</h1>
      <p>
        <Link href="/admin">← অ্যাডমিন — Admin</Link>
      </p>
      {error && <p role="alert">{error}</p>}
      {notice && <p role="status">{notice}</p>}
      <p>
        <label>
          স্টোর স্লাগ — Store slug{" "}
          <input
            value={storeFilter}
            onChange={(event) => setStoreFilter(event.target.value)}
            placeholder="wave51-store"
          />
        </label>{" "}
        <button type="button" onClick={() => void reload()}>
          খুঁজুন — Filter
        </button>
      </p>
      {items === null ? (
        <p>লোড হচ্ছে…</p>
      ) : items.length === 0 ? (
        <p>কোনো ছবি নেই — No images on the platform yet.</p>
      ) : (
        <ul className="media-library-grid">
          {items.map((item) => (
            <li key={item.id} className="media-library-item">
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
                স্টোর — Store: {item.store_slug}
                <br />
                {item.source === "upload"
                  ? `${item.width ?? "?"}×${item.height ?? "?"}`
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
                    ⚠ {item.warnings.join(", ")}
                  </>
                ) : null}
              </p>
              <button
                type="button"
                disabled={busy}
                onClick={() => void remove(item)}
              >
                মুছুন — Remove
              </button>
            </li>
          ))}
        </ul>
      )}
    </main>
  );
}
