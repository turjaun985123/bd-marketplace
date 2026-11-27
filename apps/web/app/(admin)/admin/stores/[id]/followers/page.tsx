"use client";

import { useCallback, useEffect, useState } from "react";
import Link from "next/link";
import AdminShell from "@/components/AdminShell";
import {
  listAdminStoreFollowers,
  type StoreFollower,
} from "@/lib/api";
import { useAuth } from "@/hooks/useAuth";

/**
 * Admin store followers (Wave 54 extension): the
 * moderation view of one store's follower list — the
 * same masked display the seller sees (profile name
 * when set, masked phone otherwise, plus whether the
 * follower muted new-product notices). Read-only: an
 * admin moderates stores, not other people's follows.
 */
export default function AdminStoreFollowersPage({
  params,
}: {
  params: { id: string };
}) {
  return (
    <AdminShell>
      <Followers storeId={params.id} />
    </AdminShell>
  );
}

function Followers({ storeId }: { storeId: string }) {
  const { token } = useAuth();
  const [items, setItems] = useState<StoreFollower[] | null>(null);
  const [total, setTotal] = useState(0);
  const [page, setPage] = useState(1);
  const [error, setError] = useState<string | null>(null);
  const limit = 50;

  const load = useCallback(async () => {
    if (!token) return;
    try {
      const list = await listAdminStoreFollowers(token, storeId, {
        page,
        limit,
      });
      setItems(list.items);
      setTotal(list.total);
    } catch (err) {
      setError(err instanceof Error ? err.message : "তালিকা লোড করা যায়নি।");
    }
  }, [token, storeId, page]);

  useEffect(() => {
    void load();
  }, [load]);

  return (
    <main>
      <h1>স্টোরের ফলোয়ার — Store followers</h1>
      <p>
        <Link href="/admin/stores">← সব স্টোর — All stores</Link>
      </p>
      {error && <p role="alert">{error}</p>}
      {items === null && !error && <p>লোড হচ্ছে…</p>}
      {items !== null && items.length === 0 && (
        <p>এই স্টোরের এখনো কোনো ফলোয়ার নেই — No followers yet.</p>
      )}
      {items !== null && items.length > 0 && (
        <>
          <p>
            মোট <strong>{total}</strong> জন ফলোয়ার — followers
          </p>
          <table>
            <thead>
              <tr>
                <th>ফলোয়ার — Follower</th>
                <th>ফলো করেছেন — Since</th>
                <th>নোটিফিকেশন — Notices</th>
              </tr>
            </thead>
            <tbody>
              {items.map((follower) => (
                <tr key={follower.user_id}>
                  <td>{follower.display}</td>
                  <td>
                    {new Date(follower.followed_at).toLocaleDateString(
                      "en-GB"
                    )}
                  </td>
                  <td>
                    {follower.notify_new_products
                      ? "চালু — On"
                      : "বন্ধ — Muted"}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
          <p>
            <button
              type="button"
              disabled={page <= 1}
              onClick={() => setPage((value) => Math.max(1, value - 1))}
            >
              ← আগের — Previous
            </button>{" "}
            পৃষ্ঠা {page}{" "}
            <button
              type="button"
              disabled={page * limit >= total}
              onClick={() => setPage((value) => value + 1)}
            >
              পরের — Next →
            </button>
          </p>
        </>
      )}
    </main>
  );
}
