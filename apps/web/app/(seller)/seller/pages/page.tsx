"use client";

import { useCallback, useEffect, useState } from "react";
import Link from "next/link";
import {
  createStorePage,
  deleteStorePage,
  getSellerDashboard,
  listSellerPages,
  reorderStorePages,
  setStorePageStatus,
  updateStorePage,
  type ContentStatus,
  type StorePage,
} from "@/lib/api";
import { useAuth } from "@/hooks/useAuth";

/**
 * Seller pages (Wave 59): the store's standing
 * documents — about, shipping information, size guide,
 * FAQ. Create a page as a draft, publish it when it is
 * ready, flag it for the storefront navigation, and
 * arrange the nav order with the up/down buttons (the
 * save sends the whole set to the reorder endpoint, so
 * a stale list is refused rather than half-applied).
 * Bodies are PLAIN TEXT: paragraphs, never markup —
 * the editor says so, because the storefront renders
 * exactly what is typed.
 */
export default function SellerPagesPage() {
  const { ready, token } = useAuth();
  const [storeSlug, setStoreSlug] = useState<string | null>(null);
  const [pages, setPages] = useState<StorePage[] | null>(null);
  const [message, setMessage] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  // The create form.
  const [newSlug, setNewSlug] = useState("");
  const [newTitle, setNewTitle] = useState("");
  const [newBody, setNewBody] = useState("");
  const [newInNav, setNewInNav] = useState(true);

  // The open editor (one at a time).
  const [editingId, setEditingId] = useState<string | null>(null);
  const [editSlug, setEditSlug] = useState("");
  const [editTitle, setEditTitle] = useState("");
  const [editBody, setEditBody] = useState("");
  const [editInNav, setEditInNav] = useState(false);

  const reload = useCallback(async () => {
    if (!token) return;
    try {
      const [dashboard, list] = await Promise.all([
        getSellerDashboard(token),
        listSellerPages(token),
      ]);
      setStoreSlug(dashboard.store.slug);
      setPages(list);
    } catch (err) {
      setMessage(err instanceof Error ? err.message : "পেজ লোড করা যায়নি।");
    }
  }, [token]);

  useEffect(() => {
    if (!token) return;
    let cancelled = false;
    reload().catch(() => {
      if (!cancelled) setMessage("পেজ লোড করা যায়নি।");
    });
    return () => {
      cancelled = true;
    };
  }, [token, reload]);

  async function onCreate(e: React.FormEvent) {
    e.preventDefault();
    if (!token) return;
    setBusy(true);
    setMessage(null);
    try {
      await createStorePage(token, {
        slug: newSlug.trim(),
        title: newTitle.trim(),
        body: newBody,
        show_in_nav: newInNav,
      });
      setNewSlug("");
      setNewTitle("");
      setNewBody("");
      setNewInNav(true);
      await reload();
      setMessage("পেজ তৈরি হয়েছে (খসড়া)। (Page created as a draft.)");
    } catch (err) {
      setMessage(err instanceof Error ? err.message : "তৈরি করা যায়নি।");
    } finally {
      setBusy(false);
    }
  }

  function openEditor(page: StorePage) {
    setEditingId(page.id);
    setEditSlug(page.slug);
    setEditTitle(page.title);
    setEditBody(page.body);
    setEditInNav(page.show_in_nav);
    setMessage(null);
  }

  async function onSaveEdit(e: React.FormEvent) {
    e.preventDefault();
    if (!token || !editingId) return;
    setBusy(true);
    setMessage(null);
    try {
      await updateStorePage(token, editingId, {
        slug: editSlug.trim(),
        title: editTitle.trim(),
        body: editBody,
        show_in_nav: editInNav,
      });
      setEditingId(null);
      await reload();
      setMessage("পেজ সংরক্ষণ হয়েছে। (Page saved.)");
    } catch (err) {
      setMessage(err instanceof Error ? err.message : "সংরক্ষণ করা যায়নি।");
    } finally {
      setBusy(false);
    }
  }

  async function onStatus(page: StorePage, status: ContentStatus) {
    if (!token) return;
    setBusy(true);
    setMessage(null);
    try {
      await setStorePageStatus(token, page.id, status);
      await reload();
      setMessage(
        status === "published"
          ? "পেজ প্রকাশ হয়েছে। (Page published.)"
          : status === "draft"
            ? "পেজ খসড়ায় ফিরেছে। (Page is a draft again.)"
            : "পেজ আর্কাইভ হয়েছে। (Page archived.)"
      );
    } catch (err) {
      setMessage(err instanceof Error ? err.message : "অবস্থা বদলানো যায়নি।");
    } finally {
      setBusy(false);
    }
  }

  async function onDelete(page: StorePage) {
    if (!token) return;
    if (
      !window.confirm(
        `"${page.title}" পেজটি মুছে ফেলবেন? (Delete the page "${page.title}"?)`
      )
    ) {
      return;
    }
    setBusy(true);
    setMessage(null);
    try {
      await deleteStorePage(token, page.id);
      if (editingId === page.id) setEditingId(null);
      await reload();
      setMessage("পেজ মুছে ফেলা হয়েছে। (Page deleted.)");
    } catch (err) {
      setMessage(err instanceof Error ? err.message : "মুছতে পারা যায়নি।");
    } finally {
      setBusy(false);
    }
  }

  async function onMove(page: StorePage, direction: -1 | 1) {
    if (!token || !pages) return;
    const index = pages.findIndex((p) => p.id === page.id);
    const swapWith = index + direction;
    if (index < 0 || swapWith < 0 || swapWith >= pages.length) return;
    const ids = pages.map((p) => p.id);
    const held = ids[index];
    ids[index] = ids[swapWith];
    ids[swapWith] = held;
    setBusy(true);
    setMessage(null);
    try {
      const reordered = await reorderStorePages(token, ids);
      setPages(reordered);
    } catch (err) {
      setMessage(err instanceof Error ? err.message : "ক্রম বদলানো যায়নি।");
    } finally {
      setBusy(false);
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
        <p>
          পেজ সম্পাদনা করতে <Link href="/login">সাইন ইন করুন — Sign in</Link>।
        </p>
      </main>
    );
  }

  return (
    <main>
      <h1>স্টোর পেজ — Store pages</h1>
      <p>
        <Link href="/seller">← ড্যাশবোর্ড — Dashboard</Link>
        {storeSlug && (
          <>
            {" · "}
            <Link href={`/${storeSlug}`}>স্টোরফ্রন্ট দেখুন — View storefront</Link>
          </>
        )}
      </p>
      <p>
        ডেলিভারি তথ্য, সাইজ গাইড, প্রশ্নোত্তর — স্টোরের স্থায়ী লেখাগুলো।
        লেখা সাধারণ টেক্সট: ফাঁকা লাইন দিলে নতুন অনুচ্ছেদ। কোনো HTML চলবে
        না — যা লিখবেন, পাঠক ঠিক তাই দেখবে।
      </p>
      {message && <p role="status">{message}</p>}

      <h2>নতুন পেজ — New page</h2>
      <form onSubmit={(e) => void onCreate(e)}>
        <p>
          <label>
            শিরোনাম — Title{" "}
            <input
              value={newTitle}
              onChange={(e) => setNewTitle(e.target.value)}
              maxLength={120}
              required
            />
          </label>
        </p>
        <p>
          <label>
            স্লাগ — Slug (URL){" "}
            <input
              value={newSlug}
              onChange={(e) => setNewSlug(e.target.value)}
              placeholder="shipping-info"
              required
            />
          </label>
        </p>
        <p>
          <label>
            লেখা — Body
            <br />
            <textarea
              value={newBody}
              onChange={(e) => setNewBody(e.target.value)}
              rows={10}
              cols={72}
              maxLength={20000}
              required
            />
          </label>
        </p>
        <p>
          <label>
            <input
              type="checkbox"
              checked={newInNav}
              onChange={(e) => setNewInNav(e.target.checked)}
            />{" "}
            স্টোরের নেভিগেশনে দেখাও — Show in the store navigation
          </label>
        </p>
        <button type="submit" disabled={busy}>
          খসড়া তৈরি করুন — Create draft
        </button>
      </form>

      <h2>সব পেজ — All pages</h2>
      {pages === null ? (
        <p>লোড হচ্ছে…</p>
      ) : pages.length === 0 ? (
        <p>এখনো কোনো পেজ নেই। (No pages yet.)</p>
      ) : (
        <ul className="content-list">
          {pages.map((page, index) => (
            <li key={page.id}>
              <strong>{page.title}</strong> — /{page.slug} · {page.status}
              {page.show_in_nav ? " · নেভিগেশনে আছে" : " · নেভিগেশনে নেই"}
              <br />
              <button
                type="button"
                disabled={busy || index === 0}
                onClick={() => void onMove(page, -1)}
              >
                ↑ উপরে
              </button>{" "}
              <button
                type="button"
                disabled={busy || index === pages.length - 1}
                onClick={() => void onMove(page, 1)}
              >
                ↓ নিচে
              </button>{" "}
              <button
                type="button"
                disabled={busy}
                onClick={() => openEditor(page)}
              >
                সম্পাদনা — Edit
              </button>{" "}
              {page.status !== "published" && (
                <>
                  <button
                    type="button"
                    disabled={busy}
                    onClick={() => void onStatus(page, "published")}
                  >
                    প্রকাশ করুন — Publish
                  </button>{" "}
                </>
              )}
              {page.status === "published" && (
                <>
                  <button
                    type="button"
                    disabled={busy}
                    onClick={() => void onStatus(page, "draft")}
                  >
                    খসড়া করুন — Unpublish
                  </button>{" "}
                </>
              )}
              {page.status !== "archived" && (
                <button
                  type="button"
                  disabled={busy}
                  onClick={() => void onStatus(page, "archived")}
                >
                  আর্কাইভ — Archive
                </button>
              )}{" "}
              <button
                type="button"
                disabled={busy}
                onClick={() => void onDelete(page)}
              >
                মুছুন — Delete
              </button>
              {storeSlug && page.status === "published" && (
                <>
                  {" "}
                  <Link href={`/${storeSlug}/pages/${page.slug}`}>
                    দেখুন — View
                  </Link>
                </>
              )}
              {editingId === page.id && (
                <form onSubmit={(e) => void onSaveEdit(e)}>
                  <p>
                    <label>
                      শিরোনাম — Title{" "}
                      <input
                        value={editTitle}
                        onChange={(e) => setEditTitle(e.target.value)}
                        maxLength={120}
                        required
                      />
                    </label>
                  </p>
                  <p>
                    <label>
                      স্লাগ — Slug{" "}
                      <input
                        value={editSlug}
                        onChange={(e) => setEditSlug(e.target.value)}
                        required
                      />
                    </label>
                  </p>
                  <p>
                    <label>
                      লেখা — Body
                      <br />
                      <textarea
                        value={editBody}
                        onChange={(e) => setEditBody(e.target.value)}
                        rows={12}
                        cols={72}
                        maxLength={20000}
                        required
                      />
                    </label>
                  </p>
                  <p>
                    <label>
                      <input
                        type="checkbox"
                        checked={editInNav}
                        onChange={(e) => setEditInNav(e.target.checked)}
                      />{" "}
                      নেভিগেশনে দেখাও — Show in navigation
                    </label>
                  </p>
                  <button type="submit" disabled={busy}>
                    সংরক্ষণ — Save
                  </button>{" "}
                  <button
                    type="button"
                    disabled={busy}
                    onClick={() => setEditingId(null)}
                  >
                    বাতিল — Cancel
                  </button>
                </form>
              )}
            </li>
          ))}
        </ul>
      )}
    </main>
  );
}
