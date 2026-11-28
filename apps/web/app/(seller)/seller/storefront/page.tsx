"use client";

import { useCallback, useEffect, useMemo, useState } from "react";
import Link from "next/link";
import StorefrontSectionView, {
  type SectionViewContext,
} from "@/components/StorefrontSectionViews";
import {
  getLayoutCatalogue,
  getSellerDashboard,
  getSellerLayout,
  getSellerLayoutStats,
  getStore,
  getStoreCategories,
  getStoreCollection,
  getStoreProducts,
  listSellerCollections,
  resetSellerLayout,
  saveSellerLayout,
  searchProducts,
  type LayoutCatalogue,
  type LayoutSection,
  type LayoutStats,
  type Product,
  type PublicStore,
  type ResolvedSection,
  type SectionTypeMeta,
  type Store,
  type StoreCollection,
} from "@/lib/api";
import { useAuth } from "@/hooks/useAuth";

/**
 * The storefront section editor (Wave 58): the seller
 * composes their storefront HOME from ordered sections —
 * add from the catalogue, configure each in place,
 * reorder, disable, schedule, delete — then saves the
 * whole document (PUT, full replace) or resets to the
 * platform default.
 *
 * The page's right pane is a LIVE PREVIEW rendered
 * through the very components the public storefront
 * uses, fed with sections resolved CLIENT-side from the
 * seller's own reads (products, collections, categories,
 * the store view). Fidelity limit, stated honestly: the
 * public page resolves server-side at request time, so a
 * preview can differ in edge cases (a flash sale starting
 * mid-edit, a collection changed in another tab) — the
 * saved layout read back from the API is the truth, and
 * the preview is a faithful draft, not a guarantee.
 *
 * Unknown (future) section types in a stored layout show
 * as locked blocks: preserved on save, never editable
 * here, so an older editor can never destroy a newer
 * layout by saving over it.
 */

// ---------------------------------------------------------------------------
// Config readers (a section's config is its type's own object)
// ---------------------------------------------------------------------------

function cfgStr(section: LayoutSection, key: string): string {
  const value = section.config[key];
  return typeof value === "string" ? value : "";
}

function cfgNum(section: LayoutSection, key: string, fallback: number): number {
  const value = section.config[key];
  return typeof value === "number" ? value : fallback;
}

function cfgIds(section: LayoutSection, key: string): string[] {
  const value = section.config[key];
  if (!Array.isArray(value)) return [];
  return value.filter((item): item is string => typeof item === "string");
}

interface FaqDraft {
  question: string;
  answer: string;
}

function cfgFaq(section: LayoutSection): FaqDraft[] {
  const value = section.config["items"];
  if (!Array.isArray(value)) return [];
  return value.map((item) => {
    const pair = (item ?? {}) as Record<string, unknown>;
    return {
      question: typeof pair.question === "string" ? pair.question : "",
      answer: typeof pair.answer === "string" ? pair.answer : "",
    };
  });
}

/** A fresh section id for a type: the type's own slug, numbered until unique. */
function freshSectionId(type: string, existing: LayoutSection[]): string {
  const base = type.replace(/_/g, "-");
  if (!existing.some((section) => section.id === base)) return base;
  let n = 2;
  while (existing.some((section) => section.id === `${base}-${n}`)) n += 1;
  return `${base}-${n}`;
}

/** The empty config a new section of each type starts with. */
function defaultConfig(type: string): Record<string, unknown> {
  switch (type) {
    case "product_rail":
      return { source: "newest", limit: 10 };
    case "collection_shelf":
      return { limit: 8 };
    case "category_grid":
      return { mode: "taxonomy", limit: 12 };
    case "faq":
      return { items: [{ question: "", answer: "" }] };
    default:
      return {};
  }
}

/** Preview data resolved once from the seller's own reads. */
interface PreviewData {
  store: Store;
  publicStore: PublicStore | null;
  railPool: Product[];
  sellerProducts: Product[];
  collections: StoreCollection[];
  labelTiles: { category: string; count: number }[];
  taxonomyTiles: ResolvedSection["categories"];
  shelfProducts: Record<string, Product[]>;
}

export default function SellerStorefrontPage() {
  const { ready, token } = useAuth();
  const [store, setStore] = useState<Store | null>(null);
  const [catalogue, setCatalogue] = useState<LayoutCatalogue | null>(null);
  const [stats, setStats] = useState<LayoutStats | null>(null);
  const [sections, setSections] = useState<LayoutSection[] | null>(null);
  const [isCustom, setIsCustom] = useState(false);
  const [preview, setPreview] = useState<PreviewData | null>(null);
  const [pickerType, setPickerType] = useState("hero");
  const [message, setMessage] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [showStats, setShowStats] = useState(false);

  const loadAll = useCallback(async (authToken: string) => {
    const [dashboard, layout, catalogueData, statsData] = await Promise.all([
      getSellerDashboard(authToken),
      getSellerLayout(authToken),
      getLayoutCatalogue(authToken),
      getSellerLayoutStats(authToken).catch(() => null),
    ]);
    setStore(dashboard.store);
    setSections(layout.sections);
    setIsCustom(layout.is_custom);
    setCatalogue(catalogueData);
    setStats(statsData);
    // Preview data: the seller's own reads, in parallel.
    const slug = dashboard.store.slug;
    const [publicStore, productsPage, railPage, collectionsList, facetsPage, categories] =
      await Promise.all([
        getStore(slug).catch(() => null),
        getStoreProducts(slug, authToken).catch(() => null),
        searchProducts({ store_slug: slug, per_page: 50 }).catch(() => null),
        listSellerCollections(authToken).catch(() => null),
        searchProducts({ stores: [slug], limit: 1 }).catch(() => null),
        getStoreCategories(slug).catch(() => null),
      ]);
    const collections = collectionsList?.items ?? [];
    // Single-collection shelves preview from the public
    // collection page read (products in position order).
    const shelfProducts: Record<string, Product[]> = {};
    await Promise.all(
      collections.map(async (collection) => {
        const page = await getStoreCollection(slug, collection.slug).catch(
          () => null
        );
        if (page) shelfProducts[collection.id] = page.products;
      })
    );
    setPreview({
      store: dashboard.store,
      publicStore,
      railPool: railPage?.items ?? [],
      sellerProducts: productsPage?.items ?? [],
      collections,
      labelTiles: (facetsPage?.facets.categories ?? []).map((facet) => ({
        category: facet.category,
        count: facet.count,
      })),
      taxonomyTiles: (categories?.categories ?? []).map((specialty) => ({
        name: specialty.name_bn ?? specialty.name_en,
        name_en: specialty.name_en,
        name_bn: specialty.name_bn,
        slug: specialty.slug,
        category: null,
        icon_key: specialty.icon_key,
        product_count: specialty.product_count,
        path: specialty.path,
      })),
      shelfProducts,
    });
  }, []);

  useEffect(() => {
    if (!token) return;
    loadAll(token).catch((err: unknown) => {
      setError(
        err instanceof Error ? err.message : "লেআউট লোড হয়নি — Could not load the layout."
      );
    });
  }, [token, loadAll]);

  // -------------------------------------------------------------
  // Draft mutations
  // -------------------------------------------------------------

  const patchSection = (id: string, patch: Partial<LayoutSection>) => {
    setSections((current) =>
      (current ?? []).map((section) =>
        section.id === id ? { ...section, ...patch } : section
      )
    );
  };

  const patchConfig = (id: string, key: string, value: unknown) => {
    setSections((current) =>
      (current ?? []).map((section) => {
        if (section.id !== id) return section;
        const config = { ...section.config };
        if (value === undefined || value === null || value === "") {
          delete config[key];
        } else {
          config[key] = value;
        }
        return { ...section, config };
      })
    );
  };

  const moveSection = (index: number, direction: -1 | 1) => {
    setSections((current) => {
      const list = Array.from(current ?? []);
      const target = index + direction;
      if (target < 0 || target >= list.length) return list;
      const [moved] = list.splice(index, 1);
      list.splice(target, 0, moved);
      return list;
    });
  };

  const removeSection = (id: string) => {
    setSections((current) =>
      (current ?? []).filter((section) => section.id !== id)
    );
  };

  /** Rename a section's id (its analytics handle), by position. */
  const renameSection = (index: number, newId: string) => {
    setSections((current) =>
      (current ?? []).map((section, i) =>
        i === index ? { ...section, id: newId } : section
      )
    );
  };

  /** Duplicate a section right below itself, with a fresh unique id. */
  const duplicateSection = (index: number) => {
    setSections((current) => {
      const list = Array.from(current ?? []);
      if (catalogue && list.length >= catalogue.max_sections) return list;
      const original = list[index];
      if (!original) return list;
      const copy: LayoutSection = {
        ...original,
        id: freshSectionId(original.type, list),
        config: { ...original.config },
      };
      list.splice(index + 1, 0, copy);
      return list;
    });
  };

  const addSection = () => {
    setSections((current) => {
      const list = current ?? [];
      if (catalogue && list.length >= catalogue.max_sections) return list;
      return [
        ...list,
        {
          id: freshSectionId(pickerType, list),
          type: pickerType,
          enabled: true,
          config: defaultConfig(pickerType),
        },
      ];
    });
  };

  const applyPreset = (presetKey: string) => {
    const preset = catalogue?.presets.find((entry) => entry.key === presetKey);
    if (!preset) return;
    if (
      !window.confirm(
        "এই প্রিসেটটি বর্তমান খসড়া বদলে দেবে। এগিয়ে যাবেন? — This preset replaces the current draft. Continue?"
      )
    ) {
      return;
    }
    setSections(preset.sections.map((section) => ({ ...section })));
    setMessage("প্রিসেট খসড়ায় বসানো হয়েছে — সেভ করলেই লাইভ হবে। (Preset loaded into the draft — save to publish.)");
  };

  const save = async () => {
    if (!token || sections === null) return;
    setBusy(true);
    setError(null);
    setMessage(null);
    try {
      const saved = await saveSellerLayout(token, {
        version: catalogue?.version ?? 1,
        sections,
      });
      setSections(saved.sections);
      setIsCustom(saved.is_custom);
      setMessage("লেআউট সেভ হয়েছে — আপনার স্টোরফ্রন্ট এখন এই সাজেই দেখা যাচ্ছে। (Layout saved — your storefront now renders this composition.)");
      const statsData = await getSellerLayoutStats(token).catch(() => null);
      setStats(statsData);
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : "সেভ হয়নি — Save failed.");
    } finally {
      setBusy(false);
    }
  };

  const reset = async () => {
    if (!token) return;
    if (
      !window.confirm(
        "কাস্টম লেআউট মুছে প্ল্যাটফর্ম ডিফল্টে ফিরে যাবেন? — Delete the custom layout and return to the platform default?"
      )
    ) {
      return;
    }
    setBusy(true);
    setError(null);
    try {
      const fresh = await resetSellerLayout(token);
      setSections(fresh.sections);
      setIsCustom(fresh.is_custom);
      setMessage("ডিফল্ট লেআউট ফিরিয়ে আনা হয়েছে। (Back to the platform default.)");
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : "রিসেট হয়নি — Reset failed.");
    } finally {
      setBusy(false);
    }
  };

  // -------------------------------------------------------------
  // Preview resolution (client-side; see the page doc)
  // -------------------------------------------------------------

  const previewSections = useMemo<ResolvedSection[]>(() => {
    if (!sections || !preview || !store) return [];
    const now = Date.now();
    const resolved: ResolvedSection[] = [];
    for (const section of sections) {
      if (!section.enabled) continue;
      if (section.starts_at && Date.parse(section.starts_at) > now) continue;
      if (section.ends_at && Date.parse(section.ends_at) <= now) continue;
      const base: ResolvedSection = {
        id: section.id,
        type: section.type,
        config: section.config,
      };
      switch (section.type) {
        case "hero": {
          const customised = [
            "headline",
            "subtext",
            "image_url",
            "link_url",
            "link_label",
          ].some((key) => cfgStr(section, key) !== "");
          base.hero = {
            headline: cfgStr(section, "headline") || store.name,
            subtext: cfgStr(section, "subtext") || store.description,
            image_url: cfgStr(section, "image_url") || store.banner_url,
            announcement: store.announcement,
            link_url: cfgStr(section, "link_url") || null,
            link_label: cfgStr(section, "link_label") || null,
            is_customised: customised,
          };
          break;
        }
        case "product_rail": {
          const source = cfgStr(section, "source") || "newest";
          const limit = cfgNum(section, "limit", 10);
          let products: Product[];
          if (source === "manual") {
            const byId = new Map(
              preview.sellerProducts.map((product) => [product.id, product])
            );
            products = cfgIds(section, "product_ids")
              .map((id) => byId.get(id))
              .filter((product): product is Product => product !== undefined);
          } else if (source === "on_sale") {
            products = preview.railPool.filter(
              (product) =>
                product.flash_price_poisha !== null ||
                (product.compare_at_poisha !== null &&
                  product.compare_at_poisha > product.price_poisha)
            );
          } else if (source === "best_sellers") {
            products = preview.railPool
              .filter((product) => product.sold_count > 0)
              .sort((a, b) => b.sold_count - a.sold_count);
          } else {
            products = Array.from(preview.railPool).sort((a, b) =>
              b.created_at.localeCompare(a.created_at)
            );
          }
          products = products.slice(0, limit);
          if (products.length === 0) continue;
          base.products = products;
          break;
        }
        case "collection_shelf": {
          const collectionId = cfgStr(section, "collection_id");
          if (collectionId) {
            const collection = preview.collections.find(
              (entry) => entry.id === collectionId
            );
            const products = (preview.shelfProducts[collectionId] ?? []).slice(
              0,
              cfgNum(section, "limit", 8)
            );
            if (!collection || products.length === 0) continue;
            base.collection = collection;
            base.products = products;
          } else {
            const tiles = preview.collections.filter(
              (entry) => entry.product_count > 0
            );
            if (tiles.length === 0) continue;
            base.collections = tiles;
          }
          break;
        }
        case "category_grid": {
          const mode = cfgStr(section, "mode") || "taxonomy";
          const limit = cfgNum(section, "limit", 12);
          if (mode === "labels") {
            if (preview.labelTiles.length === 0) continue;
            base.categories = preview.labelTiles.slice(0, limit).map((tile) => ({
              name: tile.category,
              name_en: null,
              name_bn: null,
              slug: null,
              category: tile.category,
              icon_key: null,
              product_count: tile.count,
              path: [],
            }));
          } else {
            const tiles = (preview.taxonomyTiles ?? []).slice(0, limit);
            if (tiles.length === 0) continue;
            base.categories = tiles;
          }
          break;
        }
        case "product_grid": {
          base.total_products =
            preview.publicStore?.product_count ?? preview.sellerProducts.length;
          break;
        }
        case "store_info": {
          if (!store.about && !store.return_policy) continue;
          base.store_info = {
            about: store.about,
            return_policy: store.return_policy,
            rating_avg: preview.publicStore?.rating_avg ?? null,
            rating_count: preview.publicStore?.rating_count ?? 0,
            total_sold: preview.publicStore?.total_sold ?? 0,
            product_count: preview.publicStore?.product_count ?? 0,
            follower_count: preview.publicStore?.follower_count ?? 0,
          };
          break;
        }
        case "faq": {
          const items = cfgFaq(section).filter(
            (item) => item.question.trim() !== "" && item.answer.trim() !== ""
          );
          if (items.length === 0) continue;
          base.faq_items = items;
          break;
        }
        case "rich_text": {
          if (cfgStr(section, "body") === "") continue;
          break;
        }
        case "image_banner": {
          if (cfgStr(section, "image_url") === "") continue;
          break;
        }
        default:
          // Unknown (future) types: skipped at render,
          // exactly like the server.
          continue;
      }
      resolved.push(base);
    }
    return resolved;
  }, [sections, preview, store]);

  if (!ready) return <p>লোড হচ্ছে… (Loading…)</p>;
  if (!token) {
    return (
      <p>
        স্টোরফ্রন্ট এডিটর ব্যবহার করতে আগে <Link href="/login">লগ ইন</Link>{" "}
        করুন। (Sign in to use the storefront editor.)
      </p>
    );
  }
  if (error && sections === null) {
    return <p>{error}</p>;
  }
  if (sections === null || store === null || catalogue === null) {
    return <p>লেআউট লোড হচ্ছে… (Loading the layout…)</p>;
  }

  const previewContext: SectionViewContext = {
    slug: store.slug,
    theme: store.theme_key,
    accent: store.brand_color ?? undefined,
    gridLayout:
      store.theme_key === "minimal"
        ? "list"
        : store.theme_key === "bold"
          ? "large"
          : "grid",
  };
  const statsById = new Map(
    (stats?.sections ?? []).map((row) => [row.section_id, row])
  );

  return (
    <main className="layout-editor">
      <h1>স্টোরফ্রন্ট এডিটর — Storefront editor</h1>
      <p>
        আপনার স্টোরের হোমপেজ কোন কোন অংশ নিয়ে, কোন ক্রমে সাজানো হবে — এখান
        থেকে ঠিক করুন। থিম (রঙ ও সাজের ধরন){" "}
        <Link href="/seller/branding">ব্র্যান্ডিং পেজে</Link> থাকে; এখানে শুধু
        অংশগুলোর বিন্যাস।{" "}
        {isCustom
          ? "এখন আপনার নিজের লেআউট চলছে।"
          : "এখন প্ল্যাটফর্ম ডিফল্ট লেআউট চলছে — কিছু বদলালেই তা আপনার নিজের হয়ে যাবে।"}
      </p>
      {message && <p className="layout-editor-message">{message}</p>}
      {error && <p className="layout-editor-error">{error}</p>}

      <div className="layout-editor-actions">
        <button type="button" onClick={save} disabled={busy}>
          লেআউট সেভ করুন — Save layout
        </button>
        <button type="button" onClick={reset} disabled={busy}>
          ডিফল্টে ফিরুন — Reset to default
        </button>
        <button type="button" onClick={() => setShowStats((value) => !value)}>
          {showStats ? "পরিসংখ্যান লুকান" : "সেকশন পরিসংখ্যান — Section stats"}
        </button>
        <Link href={`/${store.slug}`}>লাইভ স্টোরফ্রন্ট দেখুন — View live storefront</Link>
      </div>

      {showStats && stats && (
        <section className="layout-editor-stats">
          <h2>
            গত {stats.window_days} দিনের সেকশন পরিসংখ্যান — Section stats (
            {stats.window_days} days)
          </h2>
          <table>
            <thead>
              <tr>
                <th>সেকশন — Section</th>
                <th>দেখা (মোট) — Views</th>
                <th>ক্লিক (মোট) — Clicks</th>
                <th>আজকের দেখা — Today</th>
                <th>আজকের ক্লিক — Today</th>
              </tr>
            </thead>
            <tbody>
              {sections.map((section) => {
                const row = statsById.get(section.id);
                return (
                  <tr key={section.id}>
                    <td>{section.id}</td>
                    <td>{row?.views_total ?? 0}</td>
                    <td>{row?.clicks_total ?? 0}</td>
                    <td>{row?.views_today ?? 0}</td>
                    <td>{row?.clicks_today ?? 0}</td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </section>
      )}

      <section className="layout-editor-add">
        <h2>নতুন সেকশন যোগ করুন — Add a section</h2>
        <label>
          ধরন — Type{" "}
          <select
            value={pickerType}
            onChange={(event) => setPickerType(event.target.value)}
          >
            {catalogue.types.map((meta) => (
              <option key={meta.section_type} value={meta.section_type}>
                {meta.label_bn} — {meta.label_en}
              </option>
            ))}
          </select>
        </label>
        <button
          type="button"
          onClick={addSection}
          disabled={sections.length >= catalogue.max_sections}
        >
          যোগ করুন — Add
        </button>
        <p>
          {catalogue.types.find((meta) => meta.section_type === pickerType)
            ?.description_bn ?? ""}
        </p>
        <p>
          প্রিসেট দিয়ে শুরু করুন — Start from a preset:{" "}
          {catalogue.presets.map((preset) => (
            <button
              key={preset.key}
              type="button"
              onClick={() => applyPreset(preset.key)}
            >
              {preset.label_bn}
            </button>
          ))}
        </p>
        <p>
          {sections.length}/{catalogue.max_sections}টি সেকশন — sections used
        </p>
      </section>

      <div className="layout-editor-columns">
        <section className="layout-editor-list">
          <h2>আপনার সেকশনগুলো — Your sections</h2>
          {sections.length === 0 && (
            <p>
              কোনো সেকশন নেই — স্টোরফ্রন্ট ফাঁকা দেখাবে। উপরে থেকে যোগ করুন,
              অথবা ডিফল্টে ফিরুন। (No sections — the storefront home will be
              empty. Add some above, or reset to the default.)
            </p>
          )}
          {sections.map((section, index) => (
            <SectionEditorCard
              key={section.id}
              section={section}
              index={index}
              total={sections.length}
              meta={catalogue.types.find(
                (entry) => entry.section_type === section.type
              )}
              products={preview?.sellerProducts ?? []}
              collections={preview?.collections ?? []}
              railSources={catalogue.rail_sources}
              gridModes={catalogue.category_grid_modes}
              onPatch={patchSection}
              onConfig={patchConfig}
              onMove={moveSection}
              onRemove={removeSection}
              onRename={renameSection}
              onDuplicate={duplicateSection}
            />
          ))}
        </section>

        <section className="layout-editor-preview">
          <h2>লাইভ প্রিভিউ — Live preview</h2>
          <p className="layout-editor-preview-note">
            খসড়া অবস্থার প্রিভিউ — আপনার নিজের পণ্য/কালেকশন ডেটা দিয়ে
            সাজানো। লাইভ পেজ সার্ভার থেকে সাজায়, তাই মাঝে মাঝে সামান্য
            পার্থক্য হতে পারে। (Draft preview resolved from your own data;
            the live page resolves server-side and can differ slightly.)
          </p>
          <div
            className={`storefront storefront--${store.theme_key} layout-editor-preview-frame`}
          >
            {previewSections.map((section) => (
              <StorefrontSectionView
                key={section.id}
                section={section}
                context={previewContext}
              />
            ))}
            {previewSections.length === 0 && (
              <p>প্রিভিউতে দেখানোর মতো সেকশন নেই। (Nothing to preview.)</p>
            )}
          </div>
        </section>
      </div>
    </main>
  );
}

// ---------------------------------------------------------------------------
// One section's editor card
// ---------------------------------------------------------------------------

function SectionEditorCard({
  section,
  index,
  total,
  meta,
  products,
  collections,
  railSources,
  gridModes,
  onPatch,
  onConfig,
  onMove,
  onRemove,
  onRename,
  onDuplicate,
}: {
  section: LayoutSection;
  index: number;
  total: number;
  meta?: SectionTypeMeta;
  products: Product[];
  collections: StoreCollection[];
  railSources: string[];
  gridModes: string[];
  onPatch: (id: string, patch: Partial<LayoutSection>) => void;
  onConfig: (id: string, key: string, value: unknown) => void;
  onMove: (index: number, direction: -1 | 1) => void;
  onRemove: (id: string) => void;
  onRename: (index: number, newId: string) => void;
  onDuplicate: (index: number) => void;
}) {
  const known = meta !== undefined;
  return (
    <article className="layout-editor-card">
      <header className="layout-editor-card-head">
        <strong>
          {index + 1}. {meta ? `${meta.label_bn} — ${meta.label_en}` : section.type}
        </strong>
        <label className="layout-editor-card-id">
          আইডি — ID{" "}
          <input
            type="text"
            value={section.id}
            maxLength={40}
            onChange={(event) => onRename(index, event.target.value)}
          />
        </label>
        <label>
          <input
            type="checkbox"
            checked={section.enabled}
            onChange={(event) =>
              onPatch(section.id, { enabled: event.target.checked })
            }
          />{" "}
          চালু — On
        </label>
        <button
          type="button"
          onClick={() => onMove(index, -1)}
          disabled={index === 0}
        >
          ↑ উপরে
        </button>
        <button
          type="button"
          onClick={() => onMove(index, 1)}
          disabled={index === total - 1}
        >
          ↓ নিচে
        </button>
        <button type="button" onClick={() => onDuplicate(index)}>
          কপি — Duplicate
        </button>
        <button type="button" onClick={() => onRemove(section.id)}>
          মুছুন — Delete
        </button>
      </header>

      {!known && (
        <p>
          এই সেকশনের ধরন ({section.type}) এই এডিটর চেনে না — এটি সম্ভবত নতুন
          সংস্করণে তৈরি। এটি যেমন আছে তেমনই থাকবে; সেভ করলেও মুছবে না। (This
          section type is unknown to this editor — likely from a newer
          version. It is preserved exactly as-is when you save.)
        </p>
      )}

      {known && (
        <SectionConfigForm
          section={section}
          products={products}
          collections={collections}
          railSources={railSources}
          gridModes={gridModes}
          onConfig={onConfig}
        />
      )}

      <fieldset className="layout-editor-schedule">
        <legend>সময়সূচি (ঐচ্ছিক) — Schedule (optional)</legend>
        <label>
          শুরু — From{" "}
          <input
            type="datetime-local"
            value={section.starts_at ? section.starts_at.slice(0, 16) : ""}
            onChange={(event) =>
              onPatch(section.id, {
                starts_at: event.target.value
                  ? new Date(event.target.value).toISOString()
                  : null,
              })
            }
          />
        </label>
        <label>
          শেষ — Until{" "}
          <input
            type="datetime-local"
            value={section.ends_at ? section.ends_at.slice(0, 16) : ""}
            onChange={(event) =>
              onPatch(section.id, {
                ends_at: event.target.value
                  ? new Date(event.target.value).toISOString()
                  : null,
              })
            }
          />
        </label>
      </fieldset>
    </article>
  );
}

// ---------------------------------------------------------------------------
// Per-type config forms
// ---------------------------------------------------------------------------

/** Rail source labels for the catalogue's source keys. */
const RAIL_SOURCE_LABELS: Record<string, string> = {
  newest: "নতুন পণ্য — Newest",
  best_sellers: "সেরা বিক্রিত — Best sellers",
  on_sale: "অফারের পণ্য — On sale",
  manual: "হাতে-বাছাই — Hand-picked",
};

/** Category-grid mode labels for the catalogue's mode keys. */
const GRID_MODE_LABELS: Record<string, string> = {
  taxonomy: "ট্যাক্সোনমি — Taxonomy categories",
  labels: "লেবেল — Free-text labels",
};

function SectionConfigForm({
  section,
  products,
  collections,
  railSources,
  gridModes,
  onConfig,
}: {
  section: LayoutSection;
  products: Product[];
  collections: StoreCollection[];
  railSources: string[];
  gridModes: string[];
  onConfig: (id: string, key: string, value: unknown) => void;
}) {
  const id = section.id;
  const text = (
    label: string,
    key: string,
    placeholder?: string,
    maxLength?: number
  ) => (
    <label key={key}>
      {label}{" "}
      <input
        type="text"
        value={cfgStr(section, key)}
        placeholder={placeholder}
        maxLength={maxLength}
        onChange={(event) => onConfig(id, key, event.target.value)}
      />
    </label>
  );
  const number = (label: string, key: string, fallback: number) => (
    <label key={key}>
      {label}{" "}
      <input
        type="number"
        min={4}
        max={24}
        value={cfgNum(section, key, fallback)}
        onChange={(event) =>
          onConfig(id, key, Number.parseInt(event.target.value, 10) || fallback)
        }
      />
    </label>
  );

  switch (section.type) {
    case "hero":
      return (
        <div className="layout-editor-fields">
          {text("শিরোনাম — Headline", "headline", "খালি রাখলে স্টোরের নাম", 120)}
          {text("উপ-লেখা — Subtext", "subtext", "খালি রাখলে স্টোরের বর্ণনা", 300)}
          {text("ছবির লিংক — Image URL", "image_url", "খালি রাখলে ব্যানার ছবি")}
          {text("বোতামের লিংক — Link URL", "link_url", "/shop অথবা https://…")}
          {text("বোতামের লেখা — Link label", "link_label", "কিনুন", 60)}
        </div>
      );
    case "rich_text":
      return (
        <div className="layout-editor-fields">
          {text("শিরোনাম — Heading", "heading", "", 120)}
          <label>
            লেখা — Body{" "}
            <textarea
              value={cfgStr(section, "body")}
              maxLength={2000}
              rows={4}
              onChange={(event) => onConfig(id, "body", event.target.value)}
            />
          </label>
        </div>
      );
    case "product_rail": {
      const source = cfgStr(section, "source") || "newest";
      const picked = cfgIds(section, "product_ids");
      return (
        <div className="layout-editor-fields">
          {text("শিরোনাম — Title", "title", "", 80)}
          <label>
            উৎস — Source{" "}
            <select
              value={source}
              onChange={(event) => onConfig(id, "source", event.target.value)}
            >
              {railSources.map((key) => (
                <option key={key} value={key}>
                  {RAIL_SOURCE_LABELS[key] ?? key}
                </option>
              ))}
            </select>
          </label>
          {number("কয়টি পণ্য — Limit", "limit", 10)}
          {source === "manual" && (
            <fieldset className="layout-editor-products">
              <legend>
                পণ্য বাছুন (ক্রম অনুযায়ী) — Pick products (in order)
              </legend>
              {products.map((product) => {
                const checked = picked.includes(product.id);
                return (
                  <label key={product.id}>
                    <input
                      type="checkbox"
                      checked={checked}
                      onChange={(event) => {
                        const next = event.target.checked
                          ? [...picked, product.id]
                          : picked.filter((entry) => entry !== product.id);
                        onConfig(id, "product_ids", next);
                      }}
                    />{" "}
                    {product.title_bn ?? product.title}
                    {checked ? ` (#${picked.indexOf(product.id) + 1})` : ""}
                  </label>
                );
              })}
              {products.length === 0 && (
                <p>কোনো পণ্য পাওয়া যায়নি। (No products found.)</p>
              )}
            </fieldset>
          )}
        </div>
      );
    }
    case "collection_shelf":
      return (
        <div className="layout-editor-fields">
          {text("শিরোনাম — Title", "title", "", 80)}
          <label>
            কালেকশন — Collection{" "}
            <select
              value={cfgStr(section, "collection_id")}
              onChange={(event) =>
                onConfig(id, "collection_id", event.target.value)
              }
            >
              <option value="">সব কালেকশন (টাইল) — All collections (tiles)</option>
              {collections.map((collection) => (
                <option key={collection.id} value={collection.id}>
                  {collection.title}
                </option>
              ))}
            </select>
          </label>
          {number("কয়টি পণ্য — Limit", "limit", 8)}
        </div>
      );
    case "category_grid":
      return (
        <div className="layout-editor-fields">
          {text("শিরোনাম — Title", "title", "", 80)}
          <label>
            মোড — Mode{" "}
            <select
              value={cfgStr(section, "mode") || "taxonomy"}
              onChange={(event) => onConfig(id, "mode", event.target.value)}
            >
              {gridModes.map((key) => (
                <option key={key} value={key}>
                  {GRID_MODE_LABELS[key] ?? key}
                </option>
              ))}
            </select>
          </label>
          {number("কয়টি টাইল — Limit", "limit", 12)}
        </div>
      );
    case "product_grid":
      return (
        <div className="layout-editor-fields">
          {text("শিরোনাম — Title", "title", "সব পণ্য — All products", 80)}
        </div>
      );
    case "image_banner":
      return (
        <div className="layout-editor-fields">
          {text("ছবির লিংক — Image URL", "image_url")}
          {text("লিংক — Link URL", "link_url", "/shop অথবা https://…")}
          {text("ক্যাপশন — Caption", "caption", "", 120)}
        </div>
      );
    case "store_info":
      return (
        <p>
          এই অংশে স্টোরের পরিচিতি ও রিটার্ন নীতি দেখায় — লেখাগুলো{" "}
          <Link href="/seller/branding">ব্র্যান্ডিং পেজে</Link> লিখতে হয়, এখানে
          শুধু অবস্থান বদলায়। (This section shows your about text and return
          policy — written on the branding page; here you only move it.)
        </p>
      );
    case "faq": {
      const items = cfgFaq(section);
      const setItems = (next: FaqDraft[]) => onConfig(id, "items", next);
      return (
        <div className="layout-editor-fields">
          {items.map((item, index) => (
            <fieldset key={index} className="layout-editor-faq-pair">
              <legend>প্রশ্ন {index + 1} — Question {index + 1}</legend>
              <label>
                প্রশ্ন — Question{" "}
                <input
                  type="text"
                  value={item.question}
                  maxLength={200}
                  onChange={(event) => {
                    const next = Array.from(items);
                    next[index] = { ...item, question: event.target.value };
                    setItems(next);
                  }}
                />
              </label>
              <label>
                উত্তর — Answer{" "}
                <textarea
                  value={item.answer}
                  maxLength={1000}
                  rows={2}
                  onChange={(event) => {
                    const next = Array.from(items);
                    next[index] = { ...item, answer: event.target.value };
                    setItems(next);
                  }}
                />
              </label>
              <button
                type="button"
                onClick={() =>
                  setItems(items.filter((_, i) => i !== index))
                }
              >
                এই জোড়া মুছুন — Remove pair
              </button>
            </fieldset>
          ))}
          <button
            type="button"
            disabled={items.length >= 10}
            onClick={() => setItems([...items, { question: "", answer: "" }])}
          >
            আরেকটি প্রশ্ন যোগ করুন — Add a question
          </button>
        </div>
      );
    }
    default:
      return null;
  }
}
