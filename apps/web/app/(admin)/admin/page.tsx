"use client";

import { useCallback, useEffect, useState } from "react";
import Link from "next/link";
import AdminShell from "@/components/AdminShell";
import PriceTag from "@/components/PriceTag";
import {
  getAdminMetrics,
  getAdminOverview,
  getSearchAnalytics,
  listAdminMerchEvents,
  type AdminMetrics,
  type AdminOverview,
  type MerchEventView,
  type SearchAnalytics,
} from "@/lib/api";
import { formatDateTime } from "@/lib/format";
import { useAuth } from "@/hooks/useAuth";

/**
 * Admin overview (Wave 10): the platform at a glance from
 * GET /v1/admin/overview. GMV and commission exclude cancelled
 * sub-orders (they never moved money) — the definitions live on
 * the API and in the README; this page only displays them.
 * Wave 43 adds the system-status card below it, fed by
 * GET /v1/admin/metrics.
 */
export default function AdminOverviewPage() {
  return (
    <AdminShell>
      <Overview />
      <SystemStatus />
      <SearchAnalyticsCard />
      <RecentMerchEvents />
    </AdminShell>
  );
}

/**
 * Search analytics (Wave 53): what shoppers searched for
 * in the trailing window — the most-run query texts with
 * their mean result counts, and the zero-result texts,
 * which are the catalogue gaps worth stocking. The log
 * behind this carries no user id by design; the card
 * renders nothing while the window is empty.
 */
function SearchAnalyticsCard() {
  const { token } = useAuth();
  const [analytics, setAnalytics] = useState<SearchAnalytics | null>(null);

  useEffect(() => {
    if (!token) return;
    let cancelled = false;
    getSearchAnalytics(token)
      .then((data) => {
        if (!cancelled) setAnalytics(data);
      })
      .catch(() => {
        if (!cancelled) setAnalytics(null);
      });
    return () => {
      cancelled = true;
    };
  }, [token]);

  if (analytics === null || analytics.total_searches === 0) return null;
  return (
    <>
      <h2>খোঁজের হিসাব — Search analytics</h2>
      <p>
        <Link href="/admin/search-analytics">
          পুরো হিসাব — Full search analytics
        </Link>
      </p>
      <div className="card">
        <p>
          গত {analytics.window_days} দিনে মোট খোঁজ:{" "}
          <strong>{analytics.total_searches}</strong>টি — Total searches in the
          last {analytics.window_days} days
        </p>
        {analytics.top_searches.length > 0 && (
          <>
            <p>সবচেয়ে বেশি খোঁজা — Top searches:</p>
            <ul>
              {analytics.top_searches.map((row) => (
                <li key={row.query_text}>
                  “{row.query_text}” — {row.searches} বার, গড়ে{" "}
                  {row.avg_results}টি ফলাফল (avg {row.avg_results} results)
                </li>
              ))}
            </ul>
          </>
        )}
        {analytics.zero_result_searches.length > 0 && (
          <>
            <p>কিছুই মেলেনি — Zero-result searches (catalogue gaps):</p>
            <ul>
              {analytics.zero_result_searches.map((row) => (
                <li key={row.query_text}>
                  “{row.query_text}” — {row.searches} বার, শেষ{" "}
                  {formatDateTime(row.last_searched_at)}
                </li>
              ))}
            </ul>
          </>
        )}
      </div>
    </>
  );
}

/**
 * The most recent merchandising events (Wave 52): the
 * beacon's raw feed — which banners are being clicked and
 * which collections viewed, newest first, with each
 * target's current title (a deleted target shows its id —
 * the append-only log outlives its targets by design).
 */
function RecentMerchEvents() {
  const { token } = useAuth();
  const [events, setEvents] = useState<MerchEventView[] | null>(null);

  useEffect(() => {
    if (!token) return;
    let cancelled = false;
    listAdminMerchEvents(token, 10)
      .then((data) => {
        if (!cancelled) setEvents(data);
      })
      .catch(() => {
        if (!cancelled) setEvents([]);
      });
    return () => {
      cancelled = true;
    };
  }, [token]);

  if (events === null || events.length === 0) return null;
  return (
    <>
      <h2>সাম্প্রতিক মার্চেন্ডাইজিং ইভেন্ট — Recent merchandising events</h2>
      <ul>
        {events.map((event) => (
          <li key={event.id}>
            {event.kind === "banner_click"
              ? "ব্যানার ক্লিক — Banner click"
              : "কালেকশন ভিউ — Collection view"}
            {": "}
            {event.target_title ?? event.target_id} ·{" "}
            {formatDateTime(event.created_at)}
          </li>
        ))}
      </ul>
    </>
  );
}

function Overview() {
  const { token } = useAuth();
  const [overview, setOverview] = useState<AdminOverview | null>(null);
  const [message, setMessage] = useState<string | null>(null);

  const load = useCallback(async () => {
    if (!token) return;
    try {
      setOverview(await getAdminOverview(token));
    } catch (err) {
      setMessage(err instanceof Error ? err.message : "ওভারভিউ লোড করা যায়নি।");
    }
  }, [token]);

  useEffect(() => {
    void load();
  }, [load]);

  if (message) return <p>{message}</p>;
  if (!overview) return <p>লোড হচ্ছে…</p>;

  return (
    <>
      <h2>ওভারভিউ — Overview</h2>
      <div className="card">
        <p>
          ব্যবহারকারী: <strong>{overview.users}</strong>
          {" · "}স্টোর: <strong>{overview.stores}</strong>
          {" · "}পণ্য: <strong>{overview.products}</strong>
          {" · "}অর্ডার: <strong>{overview.orders}</strong>
        </p>
      </div>
      <div className="card">
        <p>
          মোট বিক্রি (GMV, বাতিল ছাড়া):{" "}
          <PriceTag amountPoisha={overview.gmv_goods_poisha} />
        </p>
        <p>
          কমিশন আয়: <PriceTag amountPoisha={overview.commission_earned_poisha} />
        </p>
      </div>
      <div className="card">
        <p>
          পেন্ডিং পে-আউট: <strong>{overview.payouts_pending.count}</strong> টি —{" "}
          <PriceTag amountPoisha={overview.payouts_pending.amount_poisha} />
        </p>
        <p>
          পেন্ডিং অনলাইন পেমেন্ট: <strong>{overview.payments_pending_count}</strong> টি
        </p>
      </div>
    </>
  );
}

/**
 * System status (Wave 43): the point-in-time health snapshot
 * from GET /v1/admin/metrics — today's trade, the queues an
 * operator must work (payout decisions, open returns), the
 * notification outbox backlog (a `sending` count that never
 * drains means a worker died mid-batch) and the stale
 * shipments the courier poller still owes. Definitions live
 * on the API; this card only displays them.
 */
function SystemStatus() {
  const { token } = useAuth();
  const [metrics, setMetrics] = useState<AdminMetrics | null>(null);
  const [message, setMessage] = useState<string | null>(null);

  const load = useCallback(async () => {
    if (!token) return;
    try {
      setMetrics(await getAdminMetrics(token));
    } catch (err) {
      setMessage(err instanceof Error ? err.message : "সিস্টেম অবস্থা লোড করা যায়নি।");
    }
  }, [token]);

  useEffect(() => {
    void load();
  }, [load]);

  return (
    <>
      <h2>সিস্টেম অবস্থা — System status</h2>
      <p>
        <Link href="/admin/media">ছবি মডারেশন — Media moderation</Link>
      </p>
      {message ? (
        <p>{message}</p>
      ) : !metrics ? (
        <p>লোড হচ্ছে…</p>
      ) : (
        <>
          <div className="card">
            <p>
              আজকের অর্ডার: <strong>{metrics.orders_today.count}</strong> টি — GMV{" "}
              <PriceTag amountPoisha={metrics.orders_today.gmv_poisha} />
            </p>
            <p>
              পেন্ডিং পে-আউট: <strong>{metrics.pending_payouts.count}</strong> টি —{" "}
              <PriceTag amountPoisha={metrics.pending_payouts.amount_poisha} />
            </p>
            <p>
              খোলা রিটার্ন: <strong>{metrics.open_returns}</strong> টি
            </p>
          </div>
          <div className="card">
            <p>
              নোটিফিকেশন ব্যাকলগ: অপেক্ষমাণ{" "}
              <strong>{metrics.notification_backlog.pending}</strong>
              {" · "}পাঠানো হচ্ছে <strong>{metrics.notification_backlog.sending}</strong>
              {" · "}পাঠানো হয়েছে <strong>{metrics.notification_backlog.sent}</strong>
              {" · "}ব্যর্থ <strong>{metrics.notification_backlog.failed}</strong>
            </p>
            <p>
              পুরনো শিপমেন্ট (poll বাকি): <strong>{metrics.stale_shipments}</strong> টি
            </p>
            <p>
              ব্যবহারকারী: <strong>{metrics.users_total}</strong>
              {" · "}স্টোর: <strong>{metrics.stores_total}</strong>
              {" · "}সক্রিয় পণ্য: <strong>{metrics.products_active}</strong>
            </p>
            <p>
              ছবি: মোট <strong>{metrics.media.images_total}</strong>
              {" · "}আপলোড <strong>{metrics.media.stored_images}</strong>
              {" · "}বাইরের URL <strong>{metrics.media.external_images}</strong>
              {" · "}সংরক্ষিত আকার{" "}
              <strong>{(metrics.media.stored_bytes / 1024).toFixed(1)} KB</strong>
            </p>
            <p>
              মার্চেন্ডাইজিং (Wave 52): ব্যানার ক্লিক{" "}
              <strong>{metrics.merchandising.banner_clicks_total}</strong> (আজ{" "}
              <strong>{metrics.merchandising.banner_clicks_today}</strong>)
              {" · "}কালেকশন ভিউ{" "}
              <strong>{metrics.merchandising.collection_views_total}</strong>{" "}
              (আজ <strong>{metrics.merchandising.collection_views_today}</strong>)
            </p>
            {metrics.media.top_stores.length > 0 && (
              <>
                <p>সবচেয়ে বেশি ছবির স্টোর — Top stores by media:</p>
                <ul>
                  {metrics.media.top_stores.map((store) => (
                    <li key={store.store_id}>
                      {store.store_name} ({store.store_slug}) — {store.images}{" "}
                      ছবি, {(store.stored_bytes / 1024).toFixed(1)} KB
                    </li>
                  ))}
                </ul>
              </>
            )}
          </div>
        </>
      )}
    </>
  );
}
