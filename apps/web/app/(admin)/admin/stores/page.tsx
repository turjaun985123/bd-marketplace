"use client";

import { useCallback, useEffect, useState } from "react";
import AdminShell from "@/components/AdminShell";
import {
  listAdminStores,
  setAdminStoreStatus,
  type AdminStore,
} from "@/lib/api";
import { useAuth } from "@/hooks/useAuth";

/**
 * Admin stores (Wave 10): every store with its owner and product
 * count, plus the suspend / reactivate switch. Suspension hides
 * the store's products from the public marketplace, storefront
 * and product detail, and checkout refuses its products (409) —
 * it deletes nothing, and the seller can still manage the
 * catalogue while suspended.
 */
export default function AdminStoresPage() {
  return (
    <AdminShell>
      <Stores />
    </AdminShell>
  );
}

function Stores() {
  const { token } = useAuth();
  const [stores, setStores] = useState<AdminStore[]>([]);
  const [q, setQ] = useState("");
  const [message, setMessage] = useState<string | null>(null);
  const [busyId, setBusyId] = useState<string | null>(null);

  const load = useCallback(async () => {
    if (!token) return;
    try {
      const list = await listAdminStores(token, {
        q: q.trim() || undefined,
        limit: 50,
      });
      setStores(list.items);
    } catch (err) {
      setMessage(err instanceof Error ? err.message : "স্টোর লোড করা যায়নি।");
    }
  }, [token, q]);

  useEffect(() => {
    void load();
  }, [load]);

  async function toggle(store: AdminStore) {
    if (!token) return;
    setBusyId(store.id);
    setMessage(null);
    try {
      await setAdminStoreStatus(
        token,
        store.id,
        store.status === "suspended" ? "active" : "suspended"
      );
      await load();
    } catch (err) {
      setMessage(err instanceof Error ? err.message : "স্ট্যাটাস বদলানো যায়নি।");
    } finally {
      setBusyId(null);
    }
  }

  return (
    <>
      <h2>স্টোর — Stores</h2>
      <label>
        খুঁজুন (নাম বা স্লাগ)
        <input value={q} onChange={(e) => setQ(e.target.value)} />
      </label>
      {message && <p>{message}</p>}
      <table>
        <thead>
          <tr>
            <th>স্টোর</th>
            <th>মালিক</th>
            <th>পণ্য</th>
            <th>স্ট্যাটাস</th>
            <th></th>
          </tr>
        </thead>
        <tbody>
          {stores.map((store) => (
            <tr key={store.id}>
              <td>
                {store.logo_url ? (
                  <>
                    {/* eslint-disable-next-line @next/next/no-img-element */}
                    <img
                      src={store.logo_url}
                      alt=""
                      style={{
                        height: 28,
                        width: 28,
                        objectFit: "cover",
                        borderRadius: 6,
                        verticalAlign: "middle",
                        marginRight: 8,
                      }}
                    />
                  </>
                ) : null}
                {store.name} <small>({store.slug})</small>
              </td>
              <td>{store.owner_phone}</td>
              <td>{store.product_count}</td>
              <td>{store.status === "suspended" ? "সাসপেন্ডেড" : "সক্রিয়"}</td>
              <td>
                <button
                  type="button"
                  disabled={busyId === store.id}
                  onClick={() => void toggle(store)}
                >
                  {store.status === "suspended" ? "সক্রিয় করুন" : "সাসপেন্ড করুন"}
                </button>
              </td>
            </tr>
          ))}
        </tbody>
      </table>
      {stores.length === 0 && <p>কোনো স্টোর পাওয়া যায়নি।</p>}
    </>
  );
}
