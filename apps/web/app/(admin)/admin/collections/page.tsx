"use client";

import { useCallback, useEffect, useState } from "react";
import AdminShell from "@/components/AdminShell";
import { Badge, Button, EmptyState } from "@/components/ui";
import { useAuth } from "@/hooks/useAuth";
import {
  createAdminCollection,
  deleteAdminCollection,
  getAdminCollection,
  listAdminCollections,
  searchProducts,
  setAdminCollectionProducts,
  updateAdminCollection,
  type MerchCollection,
  type Product,
} from "@/lib/api";
import { formatBdt } from "@/lib/format";

/**
 * Collections (Wave 52): admin-curated product sets. The
 * list manages the collections themselves; the editor below
 * manages one collection's membership — search products,
 * add them, reorder with the up/down controls, and save:
 * the saved list IS the display order, on the home feed and
 * on the public /collections/<slug> page alike.
 *
 * Inactive collections never appear publicly (the feed
 * skips them and the landing page 404s), so a collection
 * can be assembled privately and published with the toggle.
 */
export default function AdminCollectionsPage() {
  const { token } = useAuth();
  const [collections, setCollections] = useState<MerchCollection[]>([]);
  const [page, setPage] = useState(1);
  const [total, setTotal] = useState(0);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const PAGE_SIZE = 20;

  const [editingId, setEditingId] = useState<string | null>(null);
  const [slug, setSlug] = useState("");
  const [title, setTitle] = useState("");
  const [subtitle, setSubtitle] = useState("");
  const [position, setPosition] = useState("0");
  const [isActive, setIsActive] = useState(true);

  // The membership editor: the collection being edited and
  // its products in membership order, plus the picker state.
  const [memberTarget, setMemberTarget] = useState<MerchCollection | null>(null);
  const [members, setMembers] = useState<Product[]>([]);
  const [query, setQuery] = useState("");
  const [results, setResults] = useState<Product[]>([]);
  const [savingMembers, setSavingMembers] = useState(false);

  const load = useCallback(async () => {
    if (!token) return;
    try {
      const data = await listAdminCollections(token, { page, limit: PAGE_SIZE });
      setCollections(data.items);
      setTotal(data.total);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  }, [token, page]);

  useEffect(() => {
    void load();
  }, [load]);

  function resetForm() {
    setEditingId(null);
    setSlug("");
    setTitle("");
    setSubtitle("");
    setPosition("0");
    setIsActive(true);
  }

  function startEdit(collection: MerchCollection) {
    setEditingId(collection.id);
    setSlug(collection.slug);
    setTitle(collection.title);
    setSubtitle(collection.subtitle ?? "");
    setPosition(String(collection.position));
    setIsActive(collection.is_active);
    setError(null);
  }

  async function onSubmit(event: React.FormEvent) {
    event.preventDefault();
    if (!token) return;
    setBusy(true);
    setError(null);
    setNotice(null);
    try {
      const input = {
        slug: slug.trim(),
        title: title.trim(),
        subtitle: subtitle.trim() === "" ? null : subtitle.trim(),
        position: Number.parseInt(position, 10) || 0,
        is_active: isActive,
      };
      if (editingId) {
        await updateAdminCollection(token, editingId, input);
        setNotice("কালেকশন আপডেট হয়েছে — Collection updated.");
      } else {
        await createAdminCollection(token, input);
        setNotice("কালেকশন তৈরি হয়েছে — Collection created.");
      }
      resetForm();
      await load();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  }

  async function onDelete(collection: MerchCollection) {
    if (!token) return;
    if (
      !window.confirm(
        `"${collection.title}" কালেকশনটি মুছবেন? (Delete this collection? Its products are not deleted.)`
      )
    ) {
      return;
    }
    setError(null);
    try {
      await deleteAdminCollection(token, collection.id);
      if (editingId === collection.id) resetForm();
      if (memberTarget?.id === collection.id) setMemberTarget(null);
      await load();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  }

  async function openMembers(collection: MerchCollection) {
    if (!token) return;
    setError(null);
    try {
      const detail = await getAdminCollection(token, collection.id);
      setMemberTarget(collection);
      setMembers(detail.products);
      setResults([]);
      setQuery("");
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  }

  async function onSearch(event: React.FormEvent) {
    event.preventDefault();
    setError(null);
    try {
      const page = await searchProducts({ q: query.trim(), per_page: 20 });
      setResults(page.items);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  }

  function moveMember(index: number, delta: number) {
    setMembers((current) => {
      const next = [...current];
      const target = index + delta;
      if (target < 0 || target >= next.length) return current;
      const [item] = next.splice(index, 1);
      if (item) next.splice(target, 0, item);
      return next;
    });
  }

  async function onSaveMembers() {
    if (!token || !memberTarget) return;
    setSavingMembers(true);
    setError(null);
    setNotice(null);
    try {
      const detail = await setAdminCollectionProducts(
        token,
        memberTarget.id,
        members.map((p) => p.id)
      );
      setMembers(detail.products);
      setNotice("সদস্য তালিকা সংরক্ষিত — Membership saved.");
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSavingMembers(false);
    }
  }

  const memberIds = new Set(members.map((p) => p.id));

  return (
    <AdminShell>
      <h1>কালেকশন — Collections</h1>
      <p>
        হোমপেজ ও নিজস্ব পেজে দেখানো কিউরেটেড পণ্যের সেট। (Curated product
        sets for the home page and their own landing pages.)
      </p>
      {error && (
        <p role="alert" className="merch-error">
          {error}
        </p>
      )}
      {notice && <p>{notice}</p>}

      <form onSubmit={onSubmit} className="merch-form">
        <h2>
          {editingId
            ? "কালেকশন সম্পাদনা — Edit collection"
            : "নতুন কালেকশন — New collection"}
        </h2>
        <div className="merch-form-row">
          <label>
            স্লাগ — Slug (required, URL: /collections/…)
            <input
              value={slug}
              onChange={(e) => setSlug(e.target.value)}
              required
              placeholder="eid-special"
            />
          </label>
          <label>
            শিরোনাম — Title (required)
            <input
              value={title}
              onChange={(e) => setTitle(e.target.value)}
              required
              maxLength={120}
            />
          </label>
        </div>
        <label>
          সাবটাইটেল — Subtitle
          <input
            value={subtitle}
            onChange={(e) => setSubtitle(e.target.value)}
            maxLength={200}
          />
        </label>
        <div className="merch-form-row">
          <label>
            পজিশন — Position
            <input
              type="number"
              value={position}
              onChange={(e) => setPosition(e.target.value)}
            />
          </label>
          <label>
            <input
              type="checkbox"
              checked={isActive}
              onChange={(e) => setIsActive(e.target.checked)}
            />{" "}
            প্রকাশিত — Published (active)
          </label>
        </div>
        <p>
          <Button type="submit" variant="primary" loading={busy}>
            {editingId ? "আপডেট করুন — Update" : "তৈরি করুন — Create"}
          </Button>{" "}
          {editingId && (
            <Button variant="ghost" onClick={resetForm}>
              বাতিল — Cancel edit
            </Button>
          )}
        </p>
      </form>

      {collections.length === 0 ? (
        <EmptyState title="কোনো কালেকশন নেই — No collections yet" />
      ) : (
        <table className="merch-table">
          <thead>
            <tr>
              <th>কালেকশন — Collection</th>
              <th>অবস্থা — State</th>
              <th>পদক্ষেপ — Actions</th>
            </tr>
          </thead>
          <tbody>
            {collections.map((collection) => (
              <tr key={collection.id}>
                <td>
                  <strong>{collection.title}</strong>
                  <div>
                    /collections/{collection.slug} · পজিশন {collection.position}
                  </div>
                  {collection.subtitle ? <div>{collection.subtitle}</div> : null}
                </td>
                <td>
                  {collection.is_active ? (
                    <Badge tone="brand">প্রকাশিত — Live</Badge>
                  ) : (
                    <Badge tone="muted">খসড়া — Draft</Badge>
                  )}
                </td>
                <td>
                  <div className="merch-actions">
                    <Button
                      size="sm"
                      variant="ghost"
                      onClick={() => void openMembers(collection)}
                    >
                      পণ্য সাজান — Edit products
                    </Button>
                    <Button
                      size="sm"
                      variant="ghost"
                      onClick={() => startEdit(collection)}
                    >
                      সম্পাদনা — Edit
                    </Button>
                    <Button
                      size="sm"
                      variant="danger"
                      onClick={() => void onDelete(collection)}
                    >
                      মুছুন — Delete
                    </Button>
                  </div>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
      {total > PAGE_SIZE && (
        <p>
          মোট {total}টি — Total {total} · পৃষ্ঠা {page} /{" "}
          {Math.ceil(total / PAGE_SIZE)}{" "}
          <Button
            size="sm"
            variant="ghost"
            disabled={page <= 1}
            onClick={() => setPage((p) => Math.max(1, p - 1))}
          >
            ← আগের — Previous
          </Button>{" "}
          <Button
            size="sm"
            variant="ghost"
            disabled={page >= Math.ceil(total / PAGE_SIZE)}
            onClick={() => setPage((p) => p + 1)}
          >
            পরের — Next →
          </Button>
        </p>
      )}

      {memberTarget && (
        <section className="merch-editor">
          <h2>
            পণ্য সাজান — Products of “{memberTarget.title}”
          </h2>
          <p>
            তালিকার ক্রমই দেখানোর ক্রম। (The list order is the display
            order.)
          </p>
          {members.length === 0 ? (
            <EmptyState title="এখনো কোনো পণ্য নেই — No products yet" />
          ) : (
            <ol className="merch-member-list">
              {members.map((product, index) => (
                <li key={product.id} className="merch-member">
                  <span>
                    {product.title} — {formatBdt(product.price_poisha)} (
                    {product.store_slug})
                  </span>
                  <span className="merch-actions">
                    <Button
                      size="sm"
                      variant="ghost"
                      onClick={() => moveMember(index, -1)}
                    >
                      ↑ উপরে
                    </Button>
                    <Button
                      size="sm"
                      variant="ghost"
                      onClick={() => moveMember(index, 1)}
                    >
                      ↓ নিচে
                    </Button>
                    <Button
                      size="sm"
                      variant="danger"
                      onClick={() =>
                        setMembers((current) =>
                          current.filter((p) => p.id !== product.id)
                        )
                      }
                    >
                      সরান — Remove
                    </Button>
                  </span>
                </li>
              ))}
            </ol>
          )}
          <p>
            <Button variant="primary" loading={savingMembers} onClick={() => void onSaveMembers()}>
              সদস্য তালিকা সংরক্ষণ — Save membership
            </Button>
          </p>

          <form onSubmit={onSearch} className="merch-form">
            <label>
              পণ্য খুঁজুন — Search products to add
              <input
                value={query}
                onChange={(e) => setQuery(e.target.value)}
                placeholder="শাড়ি, phone, …"
              />
            </label>
            <Button type="submit" variant="ghost">
              খুঁজুন — Search
            </Button>
          </form>
          {results.length > 0 && (
            <ul className="merch-member-list">
              {results.map((product) => (
                <li key={product.id} className="merch-member">
                  <span>
                    {product.title} — {formatBdt(product.price_poisha)} (
                    {product.store_slug})
                  </span>
                  <Button
                    size="sm"
                    disabled={memberIds.has(product.id)}
                    onClick={() => setMembers((current) => [...current, product])}
                  >
                    {memberIds.has(product.id)
                      ? "যোগ হয়েছে — Added"
                      : "যোগ করুন — Add"}
                  </Button>
                </li>
              ))}
            </ul>
          )}
        </section>
      )}
    </AdminShell>
  );
}
