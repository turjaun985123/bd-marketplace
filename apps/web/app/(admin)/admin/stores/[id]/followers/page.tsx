"use client";

import { useCallback, useEffect, useRef, useState } from "react";
import Link from "next/link";
import AdminShell from "@/components/AdminShell";
import { Button, EmptyState, Skeleton } from "@/components/ui";
import {
  listAdminStoreFollowers,
  type StoreFollower,
} from "@/lib/api";
import { formatCount, formatDhakaDateTime } from "@/lib/format";
import { useAuth } from "@/hooks/useAuth";

const PAGE_LIMIT = 50;

/**
 * Admin store followers (Wave 54 extension; presentation
 * brought to the remake standard by the admin stores
 * remake): the moderation view of one store's follower
 * list — the same masked display the seller sees (profile
 * name when set, masked phone otherwise, plus whether the
 * follower muted new-product notices). Read-only: an admin
 * moderates stores, not other people's follows. No
 * follower identifier beyond that masked display exists
 * here, by design.
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
  const [loadError, setLoadError] = useState(false);
  const [stale, setStale] = useState(false);
  const [tick, setTick] = useState(0);
  const hasDataRef = useRef(false);

  const load = useCallback(async () => {
    if (!token) return;
    try {
      const list = await listAdminStoreFollowers(token, storeId, {
        page,
        limit: PAGE_LIMIT,
      });
      hasDataRef.current = true;
      setItems(list.items);
      setTotal(list.total);
      setLoadError(false);
      setStale(false);
    } catch {
      if (hasDataRef.current) setStale(true);
      else setLoadError(true);
    }
  }, [token, storeId, page, tick]);

  useEffect(() => {
    void load();
  }, [load]);

  const reload = useCallback(() => setTick((value) => value + 1), []);
  const totalPages = Math.max(1, Math.ceil(total / PAGE_LIMIT));

  return (
    <>
      <div className="admin-overview-head">
        <div>
          <h2>স্টোরের ফলোয়ার — Store followers</h2>
          <p className="admin-muted">
            <Link href="/admin/stores">← সব স্টোর — All stores</Link>
          </p>
        </div>
        <Button onClick={reload}>↻ হালনাগাদ করুন — Refresh</Button>
      </div>

      {stale ? (
        <div className="payout-stale" role="alert">
          <span>
            হালনাগাদ করা যায়নি — নিচের তথ্য আগের লোডের। (Couldn&apos;t refresh —
            showing the last loaded data.)
          </span>
          <Button size="sm" onClick={reload}>
            আবার চেষ্টা করুন — Retry
          </Button>
        </div>
      ) : null}

      {!items && !loadError ? (
        <div className="admin-skel" aria-hidden="true">
          {Array.from({ length: 4 }, (_, i) => (
            <Skeleton key={i} width="100%" height={40} />
          ))}
        </div>
      ) : null}

      {loadError ? (
        <div className="admin-error">
          <p>ফলোয়ার তালিকা লোড করা যায়নি — Couldn&apos;t load followers.</p>
          <Button size="sm" onClick={reload}>
            আবার চেষ্টা করুন — Retry
          </Button>
        </div>
      ) : null}

      {items && items.length === 0 ? (
        <EmptyState title="এই স্টোরের এখনো কোনো ফলোয়ার নেই — No followers yet">
          <p>কেউ স্টোরটি ফলো করলেই এখানে দেখা যাবে। (Followers appear here once someone follows the store.)</p>
        </EmptyState>
      ) : null}

      {items && items.length > 0 ? (
        <>
          <p className="payout-card-meta">
            মোট <strong>{formatCount(total)}</strong> জন ফলোয়ার — followers
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
                  <td>{formatDhakaDateTime(follower.followed_at)}</td>
                  <td>
                    {follower.notify_new_products
                      ? "চালু — On"
                      : "বন্ধ — Muted"}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
          {total > PAGE_LIMIT ? (
            <div className="payout-controls">
              <Button
                size="sm"
                disabled={page <= 1}
                onClick={() => setPage((value) => Math.max(1, value - 1))}
              >
                ← আগের — Previous
              </Button>
              <span className="admin-muted">
                পেজ {formatCount(page)} / {formatCount(totalPages)}
              </span>
              <Button
                size="sm"
                disabled={page >= totalPages}
                onClick={() => setPage((value) => Math.min(totalPages, value + 1))}
              >
                পরের — Next →
              </Button>
            </div>
          ) : null}
        </>
      ) : null}
    </>
  );
}
