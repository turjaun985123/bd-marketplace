"use client";

import { useCallback, useEffect, useState } from "react";
import Link from "next/link";
import {
  getFacebookStatus,
  linkFacebookPage,
  previewImport,
  runImport,
  unlinkFacebookPage,
  type FacebookStatus,
  type ImportDraft,
  type ImportResult,
} from "@/lib/api";
import { parseTakaToPoisha } from "@/lib/format";
import { useAuth } from "@/hooks/useAuth";

/**
 * Seller Facebook import page (Wave 12): link the store's
 * Facebook Page, paste Page post texts, preview the product
 * drafts the API parses out of them (title + price), edit the
 * drafts, and import them as real products. The API does the
 * parsing and all validation; this page only collects and shows.
 * Fetching posts directly from the Page needs the Meta Graph API,
 * which is not configured yet — the paste flow is the real path,
 * and the status card says so plainly.
 */

interface PostRow {
  source_ref: string;
  text: string;
}

interface DraftRow {
  source_ref: string;
  title: string;
  description: string;
  /** Price as typed taka text; converted to poisha at import. */
  priceTaka: string;
  stock: string;
  weightGrams: string;
  valid: boolean;
  errors: string[];
}

/** Poisha -> the taka text a seller edits ("1500", "1250.5"). */
function poishaToTakaInput(poisha: number | null): string {
  return poisha === null ? "" : String(poisha / 100);
}

function toDraftRow(draft: ImportDraft): DraftRow {
  return {
    source_ref: draft.source_ref,
    title: draft.title,
    description: draft.description ?? "",
    priceTaka: poishaToTakaInput(draft.price_poisha),
    stock: String(draft.stock),
    weightGrams: String(draft.weight_grams),
    valid: draft.valid,
    errors: draft.errors,
  };
}

export default function SellerImportPage() {
  const { ready, token } = useAuth();
  const [status, setStatus] = useState<FacebookStatus | null>(null);
  const [pageId, setPageId] = useState("");
  const [pageName, setPageName] = useState("");
  const [posts, setPosts] = useState<PostRow[]>([
    { source_ref: "post-1", text: "" },
  ]);
  const [drafts, setDrafts] = useState<DraftRow[] | null>(null);
  const [result, setResult] = useState<ImportResult | null>(null);
  const [message, setMessage] = useState<string | null>(null);

  const loadStatus = useCallback(async () => {
    if (!token) return;
    try {
      setStatus(await getFacebookStatus(token));
    } catch (err) {
      setMessage(err instanceof Error ? err.message : "স্ট্যাটাস লোড করা যায়নি।");
    }
  }, [token]);

  useEffect(() => {
    void loadStatus();
  }, [loadStatus]);

  async function onLink(e: React.FormEvent) {
    e.preventDefault();
    if (!token) return;
    setMessage(null);
    try {
      await linkFacebookPage(token, pageId.trim(), pageName.trim());
      setMessage("Facebook Page লিংক হয়েছে।");
      await loadStatus();
    } catch (err) {
      setMessage(err instanceof Error ? err.message : "লিংক করা যায়নি।");
    }
  }

  async function onUnlink() {
    if (!token) return;
    setMessage(null);
    try {
      await unlinkFacebookPage(token);
      setMessage("Facebook Page লিংক সরানো হয়েছে।");
      await loadStatus();
    } catch (err) {
      setMessage(err instanceof Error ? err.message : "লিংক সরানো যায়নি।");
    }
  }

  function updatePost(index: number, patch: Partial<PostRow>) {
    setPosts((prev) =>
      prev.map((row, i) => (i === index ? { ...row, ...patch } : row))
    );
  }

  function addPost() {
    setPosts((prev) =>
      prev.length >= 50
        ? prev
        : [...prev, { source_ref: `post-${prev.length + 1}`, text: "" }]
    );
  }

  function removePost(index: number) {
    setPosts((prev) => prev.filter((_, i) => i !== index));
  }

  async function onPreview(e: React.FormEvent) {
    e.preventDefault();
    if (!token) return;
    setMessage(null);
    setResult(null);
    try {
      const preview = await previewImport(
        token,
        posts
          .filter((row) => row.text.trim() !== "")
          .map((row) => ({ source_ref: row.source_ref.trim(), text: row.text }))
      );
      setDrafts(preview.drafts.map(toDraftRow));
      if (preview.drafts.length === 0) {
        setMessage("আগে অন্তত একটি পোস্টের লেখা দিন।");
      }
    } catch (err) {
      setMessage(err instanceof Error ? err.message : "প্রিভিউ করা যায়নি।");
    }
  }

  function updateDraft(index: number, patch: Partial<DraftRow>) {
    setDrafts((prev) =>
      prev === null
        ? prev
        : prev.map((row, i) => (i === index ? { ...row, ...patch } : row))
    );
  }

  async function onImport() {
    if (!token || drafts === null) return;
    setMessage(null);
    try {
      const outcome = await runImport(
        token,
        drafts.map((row) => ({
          source_ref: row.source_ref,
          title: row.title.trim(),
          description: row.description.trim() || undefined,
          // An unparseable price goes as 0; the API then skips the
          // row with a price reason instead of guessing a number.
          price_poisha: parseTakaToPoisha(row.priceTaka) ?? 0,
          stock: Number(row.stock) || 0,
          weight_grams: Number(row.weightGrams) || 1000,
        }))
      );
      setResult(outcome);
    } catch (err) {
      setMessage(err instanceof Error ? err.message : "ইমপোর্ট করা যায়নি।");
    }
  }

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
        <h1>Facebook থেকে ইমপোর্ট</h1>
        <p>
          এই পেজটি দেখতে আগে লগইন করতে হবে।{" "}
          <Link href="/login">লগইন পেজে যান — go to login</Link>
        </p>
      </main>
    );
  }

  return (
    <main>
      <h1>Facebook থেকে ইমপোর্ট — Import from Facebook</h1>
      <p>
        <Link href="/seller">ড্যাশবোর্ড</Link>
        {" · "}
        <Link href="/seller/products">আমার পণ্য — My products</Link>
      </p>
      {message && <p role="status">{message}</p>}

      <section className="card">
        <h2>Facebook Page লিংক</h2>
        {status === null ? (
          <p>লোড হচ্ছে…</p>
        ) : status.linked ? (
          <>
            <p>
              লিংক করা Page: <strong>{status.page_name}</strong> (আইডি:{" "}
              {status.page_id})
            </p>
            <button type="button" onClick={() => void onUnlink()}>
              লিংক সরান — Unlink
            </button>
          </>
        ) : (
          <form onSubmit={onLink}>
            <label>
              Page আইডি
              <input
                value={pageId}
                onChange={(e) => setPageId(e.target.value)}
                placeholder="102345678901234"
                required
              />
            </label>
            <label>
              Page-এর নাম
              <input
                value={pageName}
                onChange={(e) => setPageName(e.target.value)}
                placeholder="আমার দোকান"
                required
              />
            </label>
            <button type="submit">Page লিংক করুন</button>
          </form>
        )}
        <p>
          {status?.graph_api === "configured"
            ? "Graph API কনফিগার করা আছে, তবে Page থেকে সরাসরি পোস্ট আনার OAuth ধাপটি এখনো আসেনি — আপাতত পোস্ট পেস্ট করেই ইমপোর্ট করুন।"
            : "Graph API এখনো কনফিগার হয়নি, তাই Page থেকে সরাসরি পোস্ট আনা যাচ্ছে না — পোস্টের লেখা পেস্ট করেই ইমপোর্ট করুন।"}
        </p>
      </section>

      <section>
        <h2>পোস্ট পেস্ট করুন</h2>
        <form onSubmit={onPreview}>
          {posts.map((row, index) => (
            <div key={index} className="card">
              <label>
                পোস্টের আইডি (source ref)
                <input
                  value={row.source_ref}
                  onChange={(e) => updatePost(index, { source_ref: e.target.value })}
                  required
                />
              </label>
              <label>
                আপনার Facebook পোস্টের লেখা এখানে পেস্ট করুন
                <textarea
                  value={row.text}
                  onChange={(e) => updatePost(index, { text: e.target.value })}
                  rows={5}
                  placeholder={"নতুন শাড়ি এসেছে!\nদামঃ ১,৫০০ টাকা"}
                />
              </label>
              {posts.length > 1 && (
                <button type="button" onClick={() => removePost(index)}>
                  এই পোস্টটি সরান
                </button>
              )}
            </div>
          ))}
          <p>
            <button type="button" onClick={addPost} disabled={posts.length >= 50}>
              আরেকটি পোস্ট যোগ করুন
            </button>{" "}
            <button type="submit">প্রিভিউ দেখুন</button>
          </p>
        </form>
      </section>

      {drafts !== null && drafts.length > 0 && (
        <section>
          <h2>ড্রাফট যাচাই করুন</h2>
          {drafts.map((row, index) => (
            <div
              key={row.source_ref}
              className="card"
              style={row.valid ? undefined : { borderColor: "#c0392b" }}
            >
              <p>
                <strong>{row.source_ref}</strong>
                {!row.valid && (
                  <span style={{ color: "#c0392b" }}>
                    {" "}
                    — ঠিক করা দরকার: {row.errors.join("; ")}
                  </span>
                )}
              </p>
              <label>
                পণ্যের নাম
                <input
                  value={row.title}
                  onChange={(e) => updateDraft(index, { title: e.target.value })}
                />
              </label>
              <label>
                দাম (টাকা)
                <input
                  value={row.priceTaka}
                  onChange={(e) => updateDraft(index, { priceTaka: e.target.value })}
                  inputMode="decimal"
                  placeholder="1500"
                />
              </label>
              <label>
                স্টক
                <input
                  value={row.stock}
                  onChange={(e) => updateDraft(index, { stock: e.target.value })}
                  inputMode="numeric"
                />
              </label>
              <label>
                ওজন (গ্রাম)
                <input
                  value={row.weightGrams}
                  onChange={(e) =>
                    updateDraft(index, { weightGrams: e.target.value })
                  }
                  inputMode="numeric"
                />
              </label>
            </div>
          ))}
          <p>
            <button type="button" onClick={() => void onImport()}>
              ইমপোর্ট করুন
            </button>
          </p>
        </section>
      )}

      {result !== null && (
        <section className="card">
          <h2>ইমপোর্টের ফলাফল</h2>
          <p>
            তৈরি হয়েছে {result.created.length} টি, বাদ গেছে{" "}
            {result.skipped.length} টি।
          </p>
          {result.created.length > 0 && (
            <ul>
              {result.created.map((p) => (
                <li key={p.id}>
                  {p.title} — {p.source_ref}
                </li>
              ))}
            </ul>
          )}
          {result.skipped.length > 0 && (
            <ul>
              {result.skipped.map((s) => (
                <li key={s.source_ref}>
                  {s.source_ref}: {s.reason}
                </li>
              ))}
            </ul>
          )}
        </section>
      )}
    </main>
  );
}
