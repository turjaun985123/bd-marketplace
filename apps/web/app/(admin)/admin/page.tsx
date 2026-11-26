"use client";

import { useCallback, useEffect, useState } from "react";
import AdminShell from "@/components/AdminShell";
import PriceTag from "@/components/PriceTag";
import { getAdminOverview, type AdminOverview } from "@/lib/api";
import { useAuth } from "@/hooks/useAuth";

/**
 * Admin overview (Wave 10): the platform at a glance from
 * GET /v1/admin/overview. GMV and commission exclude cancelled
 * sub-orders (they never moved money) — the definitions live on
 * the API and in the README; this page only displays them.
 */
export default function AdminOverviewPage() {
  return (
    <AdminShell>
      <Overview />
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
