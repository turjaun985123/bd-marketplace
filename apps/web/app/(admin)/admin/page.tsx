"use client";

import { useCallback, useEffect, useState } from "react";
import AdminShell from "@/components/AdminShell";
import PriceTag from "@/components/PriceTag";
import {
  getAdminMetrics,
  getAdminOverview,
  type AdminMetrics,
  type AdminOverview,
} from "@/lib/api";
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
    </AdminShell>
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
          </div>
        </>
      )}
    </>
  );
}
