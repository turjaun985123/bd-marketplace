"use client";

import { useCallback, useState } from "react";
import Link from "next/link";
import ProductCard from "@/components/ProductCard";
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
 */
export default function SellerProductsPage() {
  const { ready, token } = useAuth();
  const [slug, setSlug] = useState("");
  const [store, setStore] = useState<Store | null>(null);
  const [products, setProducts] = useState<Product[]>([]);
  const [message, setMessage] = useState<string | null>(null);

  // Create-product form state (price typed in taka for humans).
  const [title, setTitle] = useState("");
  const [titleBn, setTitleBn] = useState("");
  const [priceTaka, setPriceTaka] = useState("");
  const [stock, setStock] = useState("0");
  const [status, setStatus] = useState("draft");

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
        price_poisha: Math.round(Number(priceTaka) * 100),
        stock: Number(stock),
        status,
      });
      setProducts((prev) => [created, ...prev]);
      setTitle("");
      setTitleBn("");
      setPriceTaka("");
      setStock("0");
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
              <ProductCard
                key={p.id}
                name={`${p.title_bn ?? p.title} (${p.status})`}
                storeName={p.store_slug}
                pricePoisha={p.price_poisha}
              />
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
