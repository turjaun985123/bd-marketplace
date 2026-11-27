"use client";

import { useCallback, useEffect, useState } from "react";
import AdminShell from "@/components/AdminShell";
import CategoryIcon, { CATEGORY_ICON_KEYS } from "@/components/CategoryIcon";
import { Badge, Button, EmptyState } from "@/components/ui";
import { useAuth } from "@/hooks/useAuth";
import {
  createCategory,
  deleteCategory,
  exportTaxonomy,
  getAdminCategoryProducts,
  importTaxonomy,
  listAdminCategories,
  mergeCategory,
  reorderCategories,
  updateCategory,
  type CategoryAdminView,
  type Product,
  type TaxonomyPortRow,
} from "@/lib/api";

/**
 * Category taxonomy admin (Wave 56): the managed category
 * tree as a flat list in tree order (a parent immediately
 * followed by its children), with the full write surface —
 * create, edit (rename / reparent / icon / order / active
 * / featured), guarded delete, sibling reorder, merge, a
 * per-category product drill-in, and the slug-keyed
 * import/export round-trip.
 *
 * Every write is audited server-side; this page is only
 * the form. Guard failures (delete with children or
 * products, reparent cycles, depth overflow) come back as
 * the API's bilingual errors and are shown verbatim —
 * they name the counts and the fix.
 */
export default function AdminCategoriesPage() {
  return (
    <AdminShell>
      <CategoriesAdmin />
    </AdminShell>
  );
}

interface FormState {
  slug: string;
  name_en: string;
  name_bn: string;
  parent_id: string;
  icon_key: string;
  sort_order: string;
  is_active: boolean;
  is_featured: boolean;
}

const EMPTY_FORM: FormState = {
  slug: "",
  name_en: "",
  name_bn: "",
  parent_id: "",
  icon_key: "tag",
  sort_order: "0",
  is_active: true,
  is_featured: false,
};

function CategoriesAdmin() {
  const { token } = useAuth();
  const [items, setItems] = useState<CategoryAdminView[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [editingId, setEditingId] = useState<string | null>(null);
  const [form, setForm] = useState<FormState>(EMPTY_FORM);
  const [mergeTarget, setMergeTarget] = useState<Record<string, string>>({});
  const [drillId, setDrillId] = useState<string | null>(null);
  const [drillProducts, setDrillProducts] = useState<Product[] | null>(null);
  const [portText, setPortText] = useState("");

  const reload = useCallback(async () => {
    if (!token) return;
    try {
      const list = await listAdminCategories(token);
      setItems(list.items);
    } catch (err) {
      setError(err instanceof Error ? err.message : "লোড করা যায়নি।");
    }
  }, [token]);

  useEffect(() => {
    void reload();
  }, [reload]);

  function startEdit(item: CategoryAdminView) {
    setEditingId(item.id);
    setForm({
      slug: item.slug,
      name_en: item.name_en,
      name_bn: item.name_bn ?? "",
      parent_id: item.parent_id ?? "",
      icon_key: item.icon_key,
      sort_order: String(item.sort_order),
      is_active: item.is_active,
      is_featured: item.is_featured,
    });
    setNotice(null);
    setError(null);
  }

  function resetForm() {
    setEditingId(null);
    setForm(EMPTY_FORM);
  }

  async function onSubmit(e: React.FormEvent) {
    e.preventDefault();
    if (!token) return;
    setError(null);
    setNotice(null);
    try {
      if (editingId) {
        const existing = items?.find((item) => item.id === editingId);
        await updateCategory(token, editingId, {
          slug: form.slug.trim(),
          name_en: form.name_en.trim(),
          name_bn: form.name_bn,
          // Tri-state: only send parent_id when it moved.
          ...(existing && (existing.parent_id ?? "") !== form.parent_id
            ? { parent_id: form.parent_id || null }
            : {}),
          icon_key: form.icon_key,
          sort_order: Number(form.sort_order) || 0,
          is_active: form.is_active,
          is_featured: form.is_featured,
        });
        setNotice("ক্যাটাগরি হালনাগাদ হয়েছে — Category updated.");
      } else {
        await createCategory(token, {
          slug: form.slug.trim(),
          name_en: form.name_en.trim(),
          name_bn: form.name_bn.trim() || undefined,
          parent_id: form.parent_id || undefined,
          icon_key: form.icon_key,
          sort_order: Number(form.sort_order) || 0,
          is_active: form.is_active,
          is_featured: form.is_featured,
        });
        setNotice("ক্যাটাগরি তৈরি হয়েছে — Category created.");
      }
      resetForm();
      await reload();
    } catch (err) {
      setError(err instanceof Error ? err.message : "সংরক্ষণ করা যায়নি।");
    }
  }

  async function onDelete(item: CategoryAdminView) {
    if (!token) return;
    if (
      !window.confirm(
        `"${item.name_en}" ক্যাটাগরিটি মুছে ফেলবেন? — Delete "${item.name_en}"?`
      )
    ) {
      return;
    }
    setError(null);
    setNotice(null);
    try {
      await deleteCategory(token, item.id);
      setNotice("ক্যাটাগরি মোছা হয়েছে — Category deleted.");
      if (editingId === item.id) resetForm();
      await reload();
    } catch (err) {
      setError(err instanceof Error ? err.message : "মোছা যায়নি।");
    }
  }

  /** Swap a node's sort_order with its previous/next sibling's. */
  async function onMove(item: CategoryAdminView, direction: -1 | 1) {
    if (!token || !items) return;
    const siblings = items.filter(
      (other) => (other.parent_id ?? "") === (item.parent_id ?? "")
    );
    const index = siblings.findIndex((other) => other.id === item.id);
    const neighbour = siblings[index + direction];
    if (!neighbour) return;
    setError(null);
    try {
      const list = await reorderCategories(token, [
        { id: item.id, sort_order: neighbour.sort_order },
        { id: neighbour.id, sort_order: item.sort_order },
      ]);
      setItems(list.items);
    } catch (err) {
      setError(err instanceof Error ? err.message : "সাজানো যায়নি।");
    }
  }

  async function onMerge(item: CategoryAdminView) {
    if (!token) return;
    const targetId = mergeTarget[item.id];
    if (!targetId) {
      setError("আগে মার্জের লক্ষ্য ক্যাটাগরি বেছে নিন — pick a merge target first.");
      return;
    }
    if (
      !window.confirm(
        `"${item.name_en}"-এর সব পণ্য ও উপ-ক্যাটাগরি লক্ষ্যে যাবে এবং এই ক্যাটাগরি মুছে যাবে। এগিয়ে যাবেন?`
      )
    ) {
      return;
    }
    setError(null);
    setNotice(null);
    try {
      const report = await mergeCategory(token, item.id, targetId);
      setNotice(
        `মার্জ হয়েছে — Merged: ${report.products_moved}টি পণ্য ও ${report.children_moved}টি উপ-ক্যাটাগরি সরেছে।`
      );
      if (editingId === item.id) resetForm();
      await reload();
    } catch (err) {
      setError(err instanceof Error ? err.message : "মার্জ করা যায়নি।");
    }
  }

  async function onDrill(item: CategoryAdminView) {
    if (!token) return;
    if (drillId === item.id) {
      setDrillId(null);
      setDrillProducts(null);
      return;
    }
    setDrillId(item.id);
    setDrillProducts(null);
    try {
      const list = await getAdminCategoryProducts(token, item.id, {
        limit: 20,
      });
      setDrillProducts(list.items);
    } catch (err) {
      setError(err instanceof Error ? err.message : "পণ্য লোড করা যায়নি।");
    }
  }

  async function onExport() {
    if (!token) return;
    setError(null);
    try {
      const data = await exportTaxonomy(token);
      setPortText(JSON.stringify(data, null, 2));
      setNotice("এক্সপোর্ট তৈরি — Export ready below.");
    } catch (err) {
      setError(err instanceof Error ? err.message : "এক্সপোর্ট করা যায়নি।");
    }
  }

  async function onImport() {
    if (!token) return;
    setError(null);
    setNotice(null);
    try {
      const parsed = JSON.parse(portText) as {
        categories: TaxonomyPortRow[];
      };
      if (!Array.isArray(parsed.categories)) {
        throw new Error("JSON-এ categories তালিকা নেই — no categories array.");
      }
      const report = await importTaxonomy(token, parsed.categories);
      setNotice(
        `ইমপোর্ট হয়েছে — Imported: ${report.created}টি নতুন, ${report.updated}টি হালনাগাদ।`
      );
      await reload();
    } catch (err) {
      setError(err instanceof Error ? err.message : "ইমপোর্ট করা যায়নি।");
    }
  }

  return (
    <>
      <h2>ক্যাটাগরি ট্যাক্সোনমি — Category taxonomy</h2>
      <p>
        সর্বোচ্চ ৩ স্তর গভীর গাছ। যেসব ক্যাটাগরিতে উপ-ক্যাটাগরি বা
        পণ্য আছে সেগুলো মোছা যায় না — আগে সরাতে হয়, অথবা মার্জ
        ব্যবহার করুন। — A tree at most 3 levels deep. Categories
        with children or products cannot be deleted — reassign
        first, or merge.
      </p>
      {error && <p role="alert">{error}</p>}
      {notice && <p role="status">{notice}</p>}

      <form onSubmit={onSubmit}>
        <h3>
          {editingId
            ? "ক্যাটাগরি সম্পাদনা — Edit category"
            : "নতুন ক্যাটাগরি — New category"}
        </h3>
        <label>
          স্লাগ (slug)
          <input
            value={form.slug}
            onChange={(e) => setForm({ ...form, slug: e.target.value })}
            placeholder="mens-fashion"
            required
          />
        </label>
        <label>
          নাম (English)
          <input
            value={form.name_en}
            onChange={(e) => setForm({ ...form, name_en: e.target.value })}
            required
          />
        </label>
        <label>
          নাম (বাংলা)
          <input
            value={form.name_bn}
            onChange={(e) => setForm({ ...form, name_bn: e.target.value })}
          />
        </label>
        <label>
          মূল ক্যাটাগরি (parent)
          <select
            value={form.parent_id}
            onChange={(e) => setForm({ ...form, parent_id: e.target.value })}
          >
            <option value="">— শীর্ষ স্তর (top level) —</option>
            {(items ?? [])
              .filter((item) => item.id !== editingId)
              .map((item) => (
                <option key={item.id} value={item.id}>
                  {"  ".repeat(Math.max(0, item.depth - 1))}
                  {item.name_en} ({item.slug})
                </option>
              ))}
          </select>
        </label>
        <label>
          আইকন (icon)
          <select
            value={form.icon_key}
            onChange={(e) => setForm({ ...form, icon_key: e.target.value })}
          >
            {CATEGORY_ICON_KEYS.map((key) => (
              <option key={key} value={key}>
                {key}
              </option>
            ))}
          </select>
        </label>
        <label>
          ক্রম (sort order)
          <input
            value={form.sort_order}
            onChange={(e) => setForm({ ...form, sort_order: e.target.value })}
            inputMode="numeric"
          />
        </label>
        <label>
          <input
            type="checkbox"
            checked={form.is_active}
            onChange={(e) => setForm({ ...form, is_active: e.target.checked })}
          />{" "}
          সক্রিয় (active)
        </label>
        <label>
          <input
            type="checkbox"
            checked={form.is_featured}
            onChange={(e) =>
              setForm({ ...form, is_featured: e.target.checked })
            }
          />{" "}
          হোমে ফিচার্ড (featured on home)
        </label>
        <p>
          <Button type="submit">
            {editingId ? "হালনাগাদ করুন" : "তৈরি করুন"}
          </Button>{" "}
          {editingId && (
            <Button type="button" onClick={resetForm}>
              বাতিল — Cancel
            </Button>
          )}
        </p>
      </form>

      {items === null ? (
        <p>লোড হচ্ছে…</p>
      ) : items.length === 0 ? (
        <EmptyState title="কোনো ক্যাটাগরি নেই — No categories">
          <p>উপরের ফর্মে প্রথম ক্যাটাগরি তৈরি করুন।</p>
        </EmptyState>
      ) : (
        <ul className="admin-category-list">
          {items.map((item) => (
            <li key={item.id}>
              <span
                style={{ paddingLeft: `${(item.depth - 1) * 24}px` }}
                className="admin-category-row"
              >
                <CategoryIcon iconKey={item.icon_key} />{" "}
                <strong>{item.name_bn ?? item.name_en}</strong>{" "}
                <span>
                  {item.name_en} · <code>{item.slug}</code>
                </span>{" "}
                {!item.is_active && <Badge>নিষ্ক্রিয় — inactive</Badge>}{" "}
                {item.is_featured && <Badge>ফিচার্ড — featured</Badge>}{" "}
                <span>
                  {item.child_count}টি উপ-ক্যাটাগরি · {item.product_count}টি
                  পণ্য যুক্ত
                </span>{" "}
                <Button type="button" onClick={() => startEdit(item)}>
                  সম্পাদনা
                </Button>{" "}
                <Button type="button" onClick={() => void onMove(item, -1)}>
                  ↑
                </Button>{" "}
                <Button type="button" onClick={() => void onMove(item, 1)}>
                  ↓
                </Button>{" "}
                <Button type="button" onClick={() => void onDrill(item)}>
                  পণ্য দেখুন ({item.product_count})
                </Button>{" "}
                <Button type="button" onClick={() => void onDelete(item)}>
                  মুছুন
                </Button>{" "}
                <select
                  value={mergeTarget[item.id] ?? ""}
                  onChange={(e) =>
                    setMergeTarget({
                      ...mergeTarget,
                      [item.id]: e.target.value,
                    })
                  }
                  aria-label="মার্জের লক্ষ্য — Merge target"
                >
                  <option value="">মার্জের লক্ষ্য…</option>
                  {items
                    .filter((other) => other.id !== item.id)
                    .map((other) => (
                      <option key={other.id} value={other.id}>
                        {other.name_en}
                      </option>
                    ))}
                </select>{" "}
                <Button type="button" onClick={() => void onMerge(item)}>
                  মার্জ
                </Button>
              </span>
              {drillId === item.id && (
                <div className="admin-category-drill">
                  {drillProducts === null ? (
                    <p>লোড হচ্ছে…</p>
                  ) : drillProducts.length === 0 ? (
                    <p>
                      এই উপ-গাছে কোনো পণ্য নেই — No products in this
                      subtree.
                    </p>
                  ) : (
                    <ul>
                      {drillProducts.map((product) => (
                        <li key={product.id}>
                          {product.title} — {product.store_slug} (
                          {product.status})
                        </li>
                      ))}
                    </ul>
                  )}
                </div>
              )}
            </li>
          ))}
        </ul>
      )}

      <h3>ইমপোর্ট / এক্সপোর্ট — Import / export</h3>
      <p>
        পুরো গাছটি স্লাগ-ভিত্তিক JSON আকারে — এক পরিবেশ থেকে
        অন্য পরিবেশে নেওয়ার জন্য। ইমপোর্ট হলো স্লাগ দিয়ে
        আপসার্ট: নাম দেওয়া নোডগুলো তৈরি/হালনাগাদ হয়, বাকিরা
        অপরিবর্তিত থাকে। — The whole tree as slug-keyed JSON.
        Import upserts by slug; unnamed nodes are untouched.
      </p>
      <p>
        <Button type="button" onClick={() => void onExport()}>
          এক্সপোর্ট — Export
        </Button>{" "}
        <Button type="button" onClick={() => void onImport()}>
          ইমপোর্ট — Import
        </Button>
      </p>
      <textarea
        value={portText}
        onChange={(e) => setPortText(e.target.value)}
        rows={14}
        style={{ width: "100%" }}
        placeholder='{"categories": [{"slug": "…", "name_en": "…"}]}'
        aria-label="ট্যাক্সোনমি JSON"
      />
    </>
  );
}
