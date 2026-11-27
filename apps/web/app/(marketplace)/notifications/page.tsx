"use client";

import { useCallback, useEffect, useState } from "react";
import Link from "next/link";
import { EmptyState } from "@/components/ui";
import {
  listMyNotifications,
  markAllNotificationsRead,
  markNotificationRead,
  type UserNotification,
} from "@/lib/api";
import { useAuth } from "@/hooks/useAuth";

/**
 * The user's notification inbox (Wave 54 extension): the
 * in-app read side of the platform's outbox — order
 * updates, payout news, and (new in Wave 54) the
 * new-product notices from followed stores. Rows are the
 * user's own copies; marking read is the user's state
 * only and never touches delivery. Unread rows are
 * emphasised; the header count is the server's unread
 * total, not a page-local guess.
 */
export default function NotificationsPage() {
  const { ready, token } = useAuth();
  const [items, setItems] = useState<UserNotification[] | null>(null);
  const [total, setTotal] = useState(0);
  const [unread, setUnread] = useState(0);
  const [page, setPage] = useState(1);
  const [error, setError] = useState<string | null>(null);
  // Wave 54 extension: an unread-only view over the
  // loaded page (client-side — the inbox pages are
  // small, and the header count stays the server's).
  const [unreadOnly, setUnreadOnly] = useState(false);
  const LIMIT = 20;

  const reload = useCallback(async () => {
    if (!token) return;
    try {
      const list = await listMyNotifications(token, {
        page,
        limit: LIMIT,
      });
      setItems(list.items);
      setTotal(list.total);
      setUnread(list.unread_count);
      setError(null);
    } catch (err) {
      setError(
        err instanceof Error ? err.message : "নোটিফিকেশন লোড করা যায়নি।"
      );
    }
  }, [token, page]);

  useEffect(() => {
    if (!ready || !token) return;
    let cancelled = false;
    reload().catch(() => {
      if (!cancelled) setError("নোটিফিকেশন লোড করা যায়নি।");
    });
    return () => {
      cancelled = true;
    };
  }, [ready, token, reload]);

  async function onMarkRead(item: UserNotification) {
    if (!token || item.read_at) return;
    try {
      const updated = await markNotificationRead(token, item.id);
      setItems((current) =>
        (current ?? []).map((n) => (n.id === updated.id ? updated : n))
      );
      setUnread((count) => Math.max(0, count - 1));
    } catch {
      // A failed mark leaves the row unread; the next
      // reload tells the truth.
    }
  }

  async function onMarkAll() {
    if (!token) return;
    try {
      await markAllNotificationsRead(token);
      await reload();
    } catch (err) {
      setError(
        err instanceof Error ? err.message : "সব পড়া হয়েছে করা যায়নি।"
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
        <h1>নোটিফিকেশন — Notifications</h1>
        <p>
          নোটিফিকেশন দেখতে আগে লগইন করতে হবে।{" "}
          <Link href="/login">লগইন পেজে যান — go to login</Link>
        </p>
      </main>
    );
  }

  const pages = Math.max(1, Math.ceil(total / LIMIT));

  return (
    <main>
      <h1>
        নোটিফিকেশন — Notifications
        {unread > 0 && <> ({unread}টি অপঠিত)</>}
      </h1>
      {unread > 0 && (
        <p>
          <button type="button" onClick={() => void onMarkAll()}>
            সব পড়া হয়েছে — Mark all read
          </button>{" "}
          <label>
            <input
              type="checkbox"
              checked={unreadOnly}
              onChange={(e) => setUnreadOnly(e.target.checked)}
            />{" "}
            শুধু অপঠিত — Unread only
          </label>
        </p>
      )}
      {error && <p role="alert">{error}</p>}
      {items === null ? (
        <p>লোড হচ্ছে…</p>
      ) : items.length === 0 ? (
        <EmptyState title="কোনো নোটিফিকেশন নেই — No notifications yet">
          <p>
            অর্ডারের আপডেট আর ফলো করা স্টোরের নতুন পণ্যের খবর এখানে আসবে।
          </p>
        </EmptyState>
      ) : (
        items
          .filter((item) => !unreadOnly || !item.read_at)
          .map((item) => (
          <article
            key={item.id}
            className={
              item.read_at
                ? "card notification-row"
                : "card notification-row notification-row--unread"
            }
          >
            <p>
              <span className="notification-kind">
                {kindLabel(item.kind)}
              </span>{" "}
              {item.body}
            </p>
            <p className="notification-meta">
              {new Date(item.created_at).toLocaleString("en-GB")}
              {item.status === "failed" &&
                " · পৌঁছায়নি (SMS গেটওয়ে নেই) — delivery failed"}
              {!item.read_at && (
                <>
                  {" · "}
                  <button
                    type="button"
                    onClick={() => void onMarkRead(item)}
                  >
                    পড়া হয়েছে — Mark read
                  </button>
                </>
              )}
            </p>
          </article>
        ))
      )}
      {pages > 1 && (
        <p>
          <button
            type="button"
            disabled={page <= 1}
            onClick={() => setPage(page - 1)}
          >
            ← আগের
          </button>{" "}
          পৃষ্ঠা {page} / {pages}{" "}
          <button
            type="button"
            disabled={page >= pages}
            onClick={() => setPage(page + 1)}
          >
            পরের →
          </button>
        </p>
      )}
    </main>
  );
}

/**
 * The reader-facing label for one notification kind
 * (Wave 54 extension). The kinds are the domain's
 * NOTIFICATION_* constants; an unknown kind (a future
 * wave's) falls back to a plain "আপডেট — Update" rather
 * than leaking the raw key to the reader.
 */
function kindLabel(kind: string): string {
  switch (kind) {
    case "order_placed":
      return "অর্ডার — Order";
    case "sub_order_shipped":
      return "পাঠানো হয়েছে — Shipped";
    case "sub_order_delivered":
      return "ডেলিভারি — Delivered";
    case "sub_order_cancelled":
      return "বাতিল — Cancelled";
    case "sub_order_returned":
      return "রিটার্ন — Returned";
    case "payout_paid":
      return "পে-আউট — Payout";
    case "return_update":
      return "রিটার্ন আপডেট — Return update";
    case "store_new_product":
      return "নতুন পণ্য — New product";
    case "store_flash_sale":
      return "ফ্ল্যাশ সেল — Flash sale";
    default:
      return "আপডেট — Update";
  }
}
