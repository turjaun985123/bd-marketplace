"use client";

import { useCallback, useEffect, useState } from "react";
import Link from "next/link";
import {
  createStoreCollection,
  deleteStoreCollection,
  getSellerCollection,
  getSellerDashboard,
  getStoreProducts,
  listSellerCollections,
  setStoreCollectionProducts,
  updateStoreCollection,
  type Product,
  type StoreCollection,
} from "@/lib/api";
import { useAuth } from "@/hooks/useAuth";

/**
 * Seller collections (Wave 54 extension): the store's own
 * curated shelves. Create a shelf (slug + title +
 * description), then arrange its membership — the picker
 * lists the store's products, the checked ones in CLICK
 * ORDER are the shelf's order (the public page renders
 * exactly that order), and saving replaces the membership
 * wholesale through the API (which re-validates that
 * every product is the store's own). Deleting a shelf
 * removes only the shelf; its products are untouched.
 */
export default function SellerCollectionsPage() {
  const { ready, token } = useAuth();
  const [storeSlug, setStoreSlug] = useState<string | null>(null);
  const [collections, setCollections] = useState<StoreCollection[] | null>(
    null
  );
  const [products, setProducts] = useState<Product[]>([]);
  const [message, setMessage] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  // The create form.
  const [newSlug, setNewSlug] = useState("");
  const [newTitle, setNewTitle] = useState("");
  const [newDescription, setNewDescription] = useState("");

  // The open editor (one at a time).
  const [editingId, setEditingId] = useState<string | null>(null);
  const [editTitle, setEditTitle] = useState("");
  const [editSlug, setEditSlug] = useState("");
  const [editDescription, setEditDescription] = useState("");
  const [memberIds, setMemberIds] = useState<string[]>([]);

  const reload = useCallback(async () => {
    if (!token) return;
    try {
      const [dashboard, list] = await Promise.all([
        getSellerDashboard(token),
        listSellerCollections(token),
      ]);
      setStoreSlug(dashboard.store.slug);
      setCollections(list.items);
      const storeProducts = await getStoreProducts(
        dashboard.store.slug,
        token
      );
      setProducts(storeProducts.items);
    } catch (err) {
      setMessage(
        err instanceof Error ? err.message : "কালেকশন লোড করা যায়নি।"
      );
    }
  }, [token]);

  useEffect(() => {
    if (!token) return;
    let cancelled = false;
    reload().catch(() => {
      if (!cancelled) setMessage("কালেকশন লোড করা যায়নি।");
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
      await createStoreCollection(token, {
        slug: newSlug.trim(),
        title: newTitle.trim(),
        description: newDescription.trim(),
      });
      setNewSlug("");
      setNewTitle("");
      setNewDescription("");
      await reload();
      setMessage("কালেকশন তৈরি হয়েছে। (Collection created.)");
    } catch (err) {
      setMessage(err instanceof Error ? err.message : "তৈরি করা যায়নি।");
    } finally {
      setBusy(false);
    }
  }

  async function openEditor(collection: StoreCollection) {
    if (!token) return;
    setMessage(null);
    try {
      const detail = await getSellerCollection(token, collection.id);
      setEditingId(collection.id);
      setEditTitle(detail.title);
      setEditSlug(detail.slug);
      setEditDescription(detail.description ?? "");
      setMemberIds(detail.product_ids);
    } catch (err) {
      setMessage(err instanceof Error ? err.message : "খোলা যায়নি।");
    }
  }

  function toggleMember(productId: string) {
    setMemberIds((current) =>
      current.includes(productId)
        ? current.filter((id) => id !== productId)
        : [...current, productId]
    );
  }

  /**
   * Move one selected member one step in the shelf order
   * (Wave 54 extension): click order sets the initial
   * order, these buttons repair it without re-clicking
   * the whole shelf.
   */
  function moveMember(productId: string, delta: number) {
    setMemberIds((current) => {
      const index = current.indexOf(productId);
      const target = index + delta;
      if (index < 0 || target < 0 || target >= current.length) {
        return current;
      }
      const next = [...current];
      const [moved] = next.splice(index, 1);
      next.splice(target, 0, moved);
      return next;
    });
  }

  async function onSaveEditor() {
    if (!token || !editingId) return;
    setBusy(true);
    setMessage(null);
    try {
      await updateStoreCollection(token, editingId, {
        slug: editSlug.trim(),
        title: editTitle.trim(),
        description: editDescription.trim(),
      });
      await setStoreCollectionProducts(token, editingId, memberIds);
      setEditingId(null);
      await reload();
      setMessage("কালেকশন সেভ হয়েছে। (Collection saved.)");
    } catch (err) {
      setMessage(err instanceof Error ? err.message : "সেভ করা যায়নি।");
    } finally {
      setBusy(false);
    }
  }

  async function onDelete(collection: StoreCollection) {
    if (!token) return;
    setBusy(true);
    setMessage(null);
    try {
      await deleteStoreCollection(token, collection.id);
      if (editingId === collection.id) setEditingId(null);
      await reload();
      setMessage("কালেকশন মোছা হয়েছে। (Collection deleted.)");
    } catch (err) {
      setMessage(err instanceof Error ? err.message : "মোছা যায়নি।");
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
        <h1>কালেকশন — Collections</h1>
        <p>
          এই পেজটি দেখতে আগে লগইন করতে হবে।{" "}
          <Link href="/login">লগইন পেজে যান — go to login</Link>
        </p>
      </main>
    );
  }

  return (
    <main>
      <h1>কালেকশন — Collections</h1>
      <p>
        <Link href="/seller">← ড্যাশবোর্ডে ফিরুন — Dashboard</Link>
        {storeSlug && (
          <>
            {" · "}
            <Link href={`/${storeSlug}`}>
              স্টোরফ্রন্টে দেখুন — View storefront
            </Link>
          </>
        )}
      </p>
      <p>
        স্টোরফ্রন্টে সাজানো তাক — প্রতিটি কালেকশন আপনার বাছাই করা পণ্যের
        একটি ক্রম। একটি স্টোর সর্বোচ্চ ২০টি কালেকশন রাখতে পারে, প্রতিটিতে
        সর্বোচ্চ ১০০টি পণ্য।
      </p>
      {message && <p role="status">{message}</p>}

      <h2>নতুন কালেকশন — New collection</h2>
      <form onSubmit={(e) => void onCreate(e)}>
        <label>
          নাম (title)
          <input
            value={newTitle}
            onChange={(e) => setNewTitle(e.target.value)}
            placeholder="ঈদ কালেকশন"
          />
        </label>
        <label>
          স্লাগ (slug — ছোট হাতের অক্ষর, সংখ্যা, হাইফেন)
          <input
            value={newSlug}
            onChange={(e) => setNewSlug(e.target.value)}
            placeholder="eid-collection"
          />
        </label>
        <label>
          বর্ণনা (description, ঐচ্ছিক)
          <textarea
            value={newDescription}
            onChange={(e) => setNewDescription(e.target.value)}
            rows={3}
            cols={60}
          />
        </label>
        <p>
          <button type="submit" disabled={busy}>
            {busy ? "তৈরি হচ্ছে…" : "তৈরি করুন — Create"}
          </button>
        </p>
      </form>

      <h2>আমার কালেকশন — My collections</h2>
      {collections === null ? (
        <p>লোড হচ্ছে…</p>
      ) : collections.length === 0 ? (
        <p>এখনো কোনো কালেকশন নেই — ওপরে প্রথমটি তৈরি করুন।</p>
      ) : (
        collections.map((collection) => (
          <section key={collection.id} className="card">
            <h3>
              {collection.title}{" "}
              <small>
                (/{collection.slug} · {collection.product_count}টি পণ্য)
              </small>
            </h3>
            {collection.description && <p>{collection.description}</p>}
            <p>
              <button
                type="button"
                disabled={busy}
                onClick={() => void openEditor(collection)}
              >
                সাজান — Edit
              </button>{" "}
              <button
                type="button"
                disabled={busy}
                onClick={() => void onDelete(collection)}
              >
                মোছুন — Delete
              </button>{" "}
              {storeSlug && (
                <Link
                  href={`/${storeSlug}/collections/${collection.slug}`}
                >
                  পাবলিক পেজ — Public page
                </Link>
              )}
            </p>
            {editingId === collection.id && (
              <div>
                <label>
                  নাম (title)
                  <input
                    value={editTitle}
                    onChange={(e) => setEditTitle(e.target.value)}
                  />
                </label>
                <label>
                  স্লাগ (slug)
                  <input
                    value={editSlug}
                    onChange={(e) => setEditSlug(e.target.value)}
                  />
                </label>
                <label>
                  বর্ণনা (description)
                  <textarea
                    value={editDescription}
                    onChange={(e) => setEditDescription(e.target.value)}
                    rows={3}
                    cols={60}
                  />
                </label>
                <h4>পণ্য বাছুন — সাজানোর ক্রমে (click order = shelf order)</h4>
                {products.length === 0 ? (
                  <p>স্টোরে কোনো পণ্য নেই।</p>
                ) : (
                  <ul>
                    {products.map((product) => {
                      const position = memberIds.indexOf(product.id);
                      return (
                        <li key={product.id}>
                          <label>
                            <input
                              type="checkbox"
                              checked={position >= 0}
                              onChange={() => toggleMember(product.id)}
                            />{" "}
                            {position >= 0 ? `${position + 1}. ` : ""}
                            {product.title_bn ?? product.title}
                            {product.status !== "active" &&
                              " (খসড়া — draft, ক্রেতা দেখবে না)"}
                          </label>
                          {position >= 0 && (
                            <>
                              {" "}
                              <button
                                type="button"
                                disabled={position === 0}
                                aria-label="উপরে নিন — Move up"
                                onClick={() =>
                                  moveMember(product.id, -1)
                                }
                              >
                                ↑
                              </button>{" "}
                              <button
                                type="button"
                                disabled={position === memberIds.length - 1}
                                aria-label="নিচে নিন — Move down"
                                onClick={() => moveMember(product.id, 1)}
                              >
                                ↓
                              </button>
                            </>
                          )}
                        </li>
                      );
                    })}
                  </ul>
                )}
                <p>
                  <button
                    type="button"
                    disabled={busy}
                    onClick={() => void onSaveEditor()}
                  >
                    {busy ? "সেভ হচ্ছে…" : "সেভ করুন — Save collection"}
                  </button>{" "}
                  <button
                    type="button"
                    disabled={busy}
                    onClick={() => setEditingId(null)}
                  >
                    বন্ধ করুন — Close
                  </button>
                </p>
              </div>
            )}
          </section>
        ))
      )}
    </main>
  );
}
