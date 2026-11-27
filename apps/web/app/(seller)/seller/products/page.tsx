"use client";

import { useCallback, useState } from "react";
import Link from "next/link";
import ImageUploader from "@/components/ImageUploader";
import ProductCard from "@/components/ProductCard";
import VariantManager from "@/components/VariantManager";
import {
  createProduct,
  getStore,
  getStoreProducts,
  type Product,
  type Store,
} from "@/lib/api";
import { useAuth } from "@/hooks/useAuth";

/**
 * Seller products page: load one of your stores by slug, see its
 * products (including your drafts — the API shows owners their own
 * non-active products), and add a new product with a simple form.
 * Prices are entered in taka and converted to poisha here; all money
 * maths still lives in the Rust API.
 *
 * Wave 33: each product expands into its variant manager
 * (the VariantManager component) — variants with their own
 * price differences and stock, managed per product.
 */
export default function SellerProductsPage() {
  const { ready, token } = useAuth();
  const [slug, setSlug] = useState("");
  const [store, setStore] = useState<Store | null>(null);
  const [products, setProducts] = useState<Product[]>([]);
  const [message, setMessage] = useState<string | null>(null);
  // Wave 33: which products have their variant manager open.
  const [variantOpen, setVariantOpen] = useState<Record<string, boolean>>({});
  // Wave 51: which products have their image manager open.
  const [imagesOpen, setImagesOpen] = useState<Record<string, boolean>>({});

  // Create-product form state (price typed in taka for humans).
  const [title, setTitle] = useState("");
  const [titleBn, setTitleBn] = useState("");
  const [category, setCategory] = useState("");
  const [priceTaka, setPriceTaka] = useState("");
  const [stock, setStock] = useState("0");
  const [status, setStatus] = useState("draft");
  const [weightGrams, setWeightGrams] = useState("1000");

  const loadStore = useCallback(
    async (storeSlug: string) => {
      if (!token) return;
      setMessage(null);
      try {
        const found = await getStore(storeSlug);
        setStore(found);
        const list = await getStoreProducts(storeSlug, token);
        setProducts(list.items);
      } catch (err) {
        setStore(null);
        setProducts([]);
        setMessage(
          err instanceof Error ? err.message : "স্টোর লোড করা যায়নি।"
        );
      }
    },
    [token]
  );

  // Nothing auto-loads: the seller enters one of their store slugs and
  // loads it explicitly via the form above.

  async function onCreate(e: React.FormEvent) {
    e.preventDefault();
    if (!token || !store) return;
    setMessage(null);
    try {
      const created = await createProduct(token, {
        store_id: store.id,
        title: title.trim(),
        title_bn: titleBn.trim() || undefined,
        category: category.trim() || undefined,
        price_poisha: Math.round(Number(priceTaka) * 100),
        stock: Number(stock),
        status,
        weight_grams: Number(weightGrams),
      });
      setProducts((prev) => [created, ...prev]);
      setTitle("");
      setTitleBn("");
      setCategory("");
      setPriceTaka("");
      setStock("0");
      setWeightGrams("1000");
      setMessage("পণ্য যোগ হয়েছে। (Product created.)");
    } catch (err) {
      setMessage(
        err instanceof Error ? err.message : "পণ্য যোগ করা যায়নি।"
      );
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
        <h1>আমার পণ্য</h1>
        <p>
          এই পেজটি দেখতে আগে লগইন করতে হবে।{" "}
          <Link href="/login">লগইন পেজে যান — go to login</Link>
        </p>
      </main>
    );
  }

  return (
    <main>
      <h1>আমার পণ্য — My products</h1>
      <p>
        Facebook Page-এর পোস্ট থেকে পণ্য আনতে{" "}
        <Link href="/seller/import">ইমপোর্ট পেজে যান — Import from Facebook</Link>
      </p>
      <form
        onSubmit={(e) => {
          e.preventDefault();
          void loadStore(slug.trim());
        }}
      >
        <label>
          আপনার স্টোরের স্লাগ (slug)
          <input
            value={slug}
            onChange={(e) => setSlug(e.target.value)}
            placeholder="rahim-store"
            required
          />
        </label>
        <button type="submit">স্টোর লোড করুন</button>
      </form>

      {message && <p role="status">{message}</p>}

      {store && (
        <>
          <h2>{store.name}</h2>
          {products.length === 0 ? (
            <p>এই স্টোরে এখনো কোনো পণ্য নেই। (No products in this store yet.)</p>
          ) : (
            products.map((p) => (
              <div key={p.id}>
                <ProductCard
                  name={`${p.title_bn ?? p.title} (${p.status}, ${p.weight_grams} গ্রাম)`}
                  storeName={p.store_slug}
                  pricePoisha={p.price_poisha}
                  isPromoted={p.is_promoted}
                  imageUrl={p.image_url}
                  hoverImageUrl={p.hover_image_url}
                  compareAtPoisha={p.compare_at_poisha}
                  soldCount={p.sold_count}
                  flashPricePoisha={p.flash_price_poisha}
                  flashEndsAt={p.flash_ends_at}
                />
                {p.flash_price_poisha !== null && (
                  <p>
                    ⚡ এই পণ্যটি এখন ফ্ল্যাশ সেলে আছে —{" "}
                    <Link href="/seller/flash-sale">
                      সেলের বিবরণ দেখুন — See the flash sale
                    </Link>
                  </p>
                )}
                <p>
                  <button
                    type="button"
                    onClick={() =>
                      setImagesOpen((prev) => ({
                        ...prev,
                        [p.id]: !prev[p.id],
                      }))
                    }
                  >
                    {imagesOpen[p.id]
                      ? "ছবি লুকান"
                      : "ছবি পরিচালনা — Manage images"}
                    {p.image_url ? " (আছে)" : " (নেই — যোগ করুন!)"}
                  </button>
                </p>
                {imagesOpen[p.id] && token && (
                  <ImageUploader token={token} productId={p.id} />
                )}
                {p.status === "active" && (
                  <p>
                    <Link href={`/seller/promotions?product=${p.id}`}>
                      প্রমোট করুন — Promote this product
                    </Link>
                  </p>
                )}
                <p>
                  <button
                    type="button"
                    onClick={() =>
                      setVariantOpen((prev) => ({
                        ...prev,
                        [p.id]: !prev[p.id],
                      }))
                    }
                  >
                    {variantOpen[p.id]
                      ? "ভ্যারিয়েন্ট লুকান"
                      : "ভ্যারিয়েন্ট পরিচালনা — Manage variants"}
                    {p.has_variants ? " (আছে)" : ""}
                  </button>
                </p>
                {variantOpen[p.id] && token && (
                  <VariantManager token={token} productId={p.id} />
                )}
              </div>
            ))
          )}

          <h2>নতুন পণ্য যোগ করুন — Add product</h2>
          <form onSubmit={onCreate}>
            <label>
              পণ্যের নাম (English)
              <input
                value={title}
                onChange={(e) => setTitle(e.target.value)}
                required
              />
            </label>
            <label>
              পণ্যের নাম (বাংলা)
              <input value={titleBn} onChange={(e) => setTitleBn(e.target.value)} />
            </label>
            <label>
              ক্যাটাগরি (Category) — খোঁজার ফিল্টারে ব্যবহৃত হয়
              <input
                value={category}
                onChange={(e) => setCategory(e.target.value)}
                placeholder="যেমন: clothing"
                maxLength={60}
              />
            </label>
            <label>
              দাম (টাকা)
              <input
                value={priceTaka}
                onChange={(e) => setPriceTaka(e.target.value)}
                inputMode="decimal"
                required
              />
            </label>
            <label>
              স্টক
              <input
                value={stock}
                onChange={(e) => setStock(e.target.value)}
                inputMode="numeric"
                required
              />
            </label>
            <label>
              ওজন (গ্রাম) — ডেলিভারি চার্জ এই ওজন দিয়ে হিসাব হয়
              <input
                value={weightGrams}
                onChange={(e) => setWeightGrams(e.target.value)}
                inputMode="numeric"
                min={1}
                max={50000}
                required
              />
            </label>
            <label>
              অবস্থা (status)
              <select value={status} onChange={(e) => setStatus(e.target.value)}>
                <option value="draft">draft — খসড়া</option>
                <option value="active">active — বিক্রির জন্য</option>
                <option value="archived">archived</option>
              </select>
            </label>
            <button type="submit">পণ্য যোগ করুন</button>
          </form>
        </>
      )}
    </main>
  );
}
