"use client";

import { useCallback, useEffect, useState, type ReactNode } from "react";
import Link from "next/link";
import AdminShell from "@/components/AdminShell";
import PriceTag from "@/components/PriceTag";
import DailyBars from "@/components/DailyBars";
import {
  Badge,
  Button,
  Countdown,
  EmptyState,
  SectionHeader,
  Skeleton,
} from "@/components/ui";
import {
  getAdminAnalytics,
  getAdminAuditLog,
  getAdminMetrics,
  getAdminOverview,
  getAdminSettings,
  getSearchAnalytics,
  listAdminMerchEvents,
  orderStatusLabel,
  paymentStatusLabel,
  type AdminAnalytics,
  type AdminAuditEntry,
  type AdminMetrics,
  type AdminOverview,
  type AdminSettings,
  type MerchEventView,
  type SearchAnalytics,
} from "@/lib/api";
import {
  formatAgeSeconds,
  formatBdt,
  formatBytes,
  formatCount,
  formatDateTime,
  formatDhakaDateTime,
} from "@/lib/format";
import { useAuth } from "@/hooks/useAuth";

/**
 * Admin overview — the operator's home, remade after the
 * ~100-agent surface analysis. Layout, top to bottom:
 * today's numbers first (with yesterday deltas), then the
 * "needs attention" queue — every row a deep link to the
 * page that clears it — then the order pipeline, the last
 * 7 days, geography, and the compact platform blocks.
 *
 * Definitions are the platform's canonical ones and live
 * on the API + README; this page only displays them. In
 * particular: GMV is goods − discount + delivery over
 * non-cancelled sub-orders (Wave 27), commission is earned
 * on DELIVERED sub-orders only, and `advance_pending` is a
 * PAYMENT status — it is never rendered as an order status.
 * Every section loads, fails, and retries on its own: a
 * failed section shows an error with a retry button, never
 * a silently missing card, and an empty section says so.
 */
export default function AdminOverviewPage() {
  const [refreshKey, setRefreshKey] = useState(0);
  const [asOf, setAsOf] = useState<string | null>(null);

  useEffect(() => {
    setAsOf(new Date().toISOString());
  }, [refreshKey]);

  return (
    <AdminShell>
      <div className="admin-overview-head">
        <div>
          <h2>ওভারভিউ — Overview</h2>
          <p className="admin-muted">
            সর্বশেষ হালনাগাদ:{" "}
            {asOf ? formatDhakaDateTime(asOf) : "—"} (ঢাকা সময়)
          </p>
        </div>
        <Button onClick={() => setRefreshKey((key) => key + 1)}>
          ↻ হালনাগাদ করুন — Refresh
        </Button>
      </div>
      <Dashboard refreshKey={refreshKey} />
      <TrendSection refreshKey={refreshKey} />
      <SearchSection refreshKey={refreshKey} />
      <MerchEventsSection refreshKey={refreshKey} />
      <AuditSection refreshKey={refreshKey} />
    </AdminShell>
  );
}

// ---------------------------------------------------------------------------
// Shared section plumbing
// ---------------------------------------------------------------------------

/**
 * One section's data lifecycle: fetch on mount / refresh /
 * retry, keep the last good data while refetching, and
 * surface failure as a flag the section renders as an
 * error row — never as a vanished card.
 */
function useAdminData<T>(
  load: (token: string) => Promise<T>,
  refreshKey: number,
): {
  data: T | null;
  error: boolean;
  reload: () => void;
} {
  const { token } = useAuth();
  const [data, setData] = useState<T | null>(null);
  const [error, setError] = useState(false);
  const [tick, setTick] = useState(0);

  useEffect(() => {
    if (!token) return;
    let cancelled = false;
    load(token)
      .then((next) => {
        if (!cancelled) {
          setData(next);
          setError(false);
        }
      })
      .catch(() => {
        if (!cancelled) setError(true);
      });
    return () => {
      cancelled = true;
    };
  }, [token, load, refreshKey, tick]);

  const reload = useCallback(() => setTick((value) => value + 1), []);
  return { data, error, reload };
}

/** A titled card frame for one overview section. */
function Section({
  title,
  linkHref,
  linkLabel,
  children,
}: {
  title: string;
  linkHref?: string;
  linkLabel?: string;
  children: ReactNode;
}) {
  return (
    <section className="card admin-section">
      <SectionHeader
        title={title}
        linkHref={linkHref}
        linkLabel={linkLabel}
      />
      {children}
    </section>
  );
}

/** The per-section failure state: named, with a retry. */
function SectionError({ onRetry }: { onRetry: () => void }) {
  return (
    <div className="admin-error">
      <p>তথ্য লোড করা যায়নি — Couldn&apos;t load this section.</p>
      <Button size="sm" onClick={onRetry}>
        আবার চেষ্টা করুন — Retry
      </Button>
    </div>
  );
}

/** Loading placeholder rows in the section's own shape. */
function SectionSkeleton({ rows = 3 }: { rows?: number }) {
  return (
    <div className="admin-skel" aria-hidden="true">
      {Array.from({ length: rows }, (_, i) => (
        <Skeleton key={i} width="100%" height={18} />
      ))}
    </div>
  );
}

/** A today-vs-yesterday delta chip for a KPI tile. */
function DeltaBadge({
  today,
  yesterday,
}: {
  today: number;
  yesterday: number;
}) {
  if (today === yesterday) {
    return <Badge tone="muted">গতকালের সমান</Badge>;
  }
  const up = today > yesterday;
  return (
    <Badge tone={up ? "brand" : "danger"}>
      {up ? "▲" : "▼"} {formatCount(Math.abs(today - yesterday))}
    </Badge>
  );
}

/** The GMV delta as a percentage of yesterday (a raw poisha
 * difference would read as a taka figure and mislead). */
function GmvDeltaBadge({
  today,
  yesterday,
}: {
  today: number;
  yesterday: number;
}) {
  if (yesterday === 0) {
    return today === 0 ? (
      <Badge tone="muted">গতকালের সমান</Badge>
    ) : (
      <Badge tone="brand">গতকাল ৳০ ছিল</Badge>
    );
  }
  const pct = ((today - yesterday) / yesterday) * 100;
  if (pct === 0) return <Badge tone="muted">গতকালের সমান</Badge>;
  const up = pct > 0;
  return (
    <Badge tone={up ? "brand" : "danger"}>
      {up ? "▲" : "▼"} {Math.abs(pct).toFixed(1)}%
    </Badge>
  );
}

// ---------------------------------------------------------------------------
// Dashboard: KPIs, the attention queue, pipeline, geography, platform
// ---------------------------------------------------------------------------

interface DashboardData {
  overview: AdminOverview;
  metrics: AdminMetrics;
  settings: AdminSettings | null;
}

function Dashboard({ refreshKey }: { refreshKey: number }) {
  const load = useCallback(async (token: string): Promise<DashboardData> => {
    const [overview, metrics, settings] = await Promise.all([
      getAdminOverview(token),
      getAdminMetrics(token),
      // Settings are a caption on this page, not its
      // subject: a settings failure hides the line, never
      // the whole dashboard.
      getAdminSettings(token).catch(() => null),
    ]);
    return { overview, metrics, settings };
  }, []);
  const { data, error, reload } = useAdminData(load, refreshKey);

  if (error && !data) {
    return (
      <Section title="আজকের চিত্র — Today">
        <SectionError onRetry={reload} />
      </Section>
    );
  }
  if (!data) {
    return (
      <Section title="আজকের চিত্র — Today">
        <div className="admin-kpi-grid" aria-hidden="true">
          {Array.from({ length: 6 }, (_, i) => (
            <div key={i} className="kpi-tile">
              <Skeleton width="60%" height={14} />
              <Skeleton width="80%" height={28} />
              <Skeleton width="70%" height={14} />
            </div>
          ))}
        </div>
        <SectionSkeleton rows={4} />
      </Section>
    );
  }

  const { overview, metrics, settings } = data;
  const today = metrics.orders_today;
  const yesterday = metrics.orders_yesterday;

  return (
    <>
      <Section title="আজকের চিত্র — Today">
        <div className="admin-kpi-grid">
          <div className="kpi-tile">
            <p className="kpi-label">আজকের অর্ডার — Orders today</p>
            <p className="kpi-value">{formatCount(today.count)}</p>
            <p className="kpi-sub">
              গতকাল {formatCount(yesterday.count)}টি{" "}
              <DeltaBadge today={today.count} yesterday={yesterday.count} />
            </p>
          </div>
          <div className="kpi-tile">
            <p className="kpi-label">আজকের বিক্রি (GMV)</p>
            <p className="kpi-value">
              <PriceTag amountPoisha={today.gmv_poisha} />
            </p>
            <p className="kpi-sub">
              গতকাল <PriceTag amountPoisha={yesterday.gmv_poisha} />{" "}
              <GmvDeltaBadge
                today={today.gmv_poisha}
                yesterday={yesterday.gmv_poisha}
              />
            </p>
          </div>
          <div className="kpi-tile">
            <p className="kpi-label">মোট বিক্রি (GMV) — সর্বকালীন</p>
            <p className="kpi-value">
              <PriceTag amountPoisha={overview.gmv_poisha} />
            </p>
            <p className="kpi-sub">ছাড় বাদ · ডেলিভারিসহ · বাতিল বাদে</p>
          </div>
          <div className="kpi-tile">
            <p className="kpi-label">কমিশন আয় — সর্বকালীন</p>
            <p className="kpi-value">
              <PriceTag amountPoisha={overview.commission_earned_poisha} />
            </p>
            <p className="kpi-sub">শুধু ডেলিভারি হওয়া অর্ডার থেকে</p>
          </div>
          <Link href="/admin/payouts" className="kpi-tile kpi-tile--link">
            <p className="kpi-label">অপেক্ষমাণ পে-আউট — Payouts pending</p>
            <p className="kpi-value">
              {formatCount(metrics.pending_payouts.count)}টি ·{" "}
              <PriceTag amountPoisha={metrics.pending_payouts.amount_poisha} />
            </p>
            <p className="kpi-sub">
              {metrics.pending_payouts.oldest_age_secs !== null
                ? `সবচেয়ে পুরনো: ${formatAgeSeconds(metrics.pending_payouts.oldest_age_secs)} ধরে`
                : "কোনো অনুরোধ বাকি নেই"}
            </p>
          </Link>
          <div className="kpi-tile">
            <p className="kpi-label">মোট অর্ডার — All orders</p>
            <p className="kpi-value">{formatCount(overview.orders)}</p>
            <p className="kpi-sub">
              বাতিলসহ · আজ নতুন ব্যবহারকারী{" "}
              {formatCount(metrics.growth_today.new_users)} জন
            </p>
          </div>
        </div>
      </Section>

      <AttentionQueue metrics={metrics} />
      <Pipeline metrics={metrics} overview={overview} />
      <Geography metrics={metrics} totalOrders={overview.orders} />
      <PlatformBlocks
        metrics={metrics}
        overview={overview}
        settings={settings}
      />
    </>
  );
}

// ---------------------------------------------------------------------------
// Needs attention — the operator's work queue
// ---------------------------------------------------------------------------

interface QueueRow {
  key: string;
  label: string;
  count: number;
  tone: "brand" | "accent" | "danger" | "muted";
  meta: ReactNode;
  href?: string;
}

function AttentionQueue({ metrics }: { metrics: AdminMetrics }) {
  const nb = metrics.notification_backlog;
  const rows: QueueRow[] = [];

  if (metrics.pending_payouts.count > 0) {
    rows.push({
      key: "payouts",
      label: "পে-আউটের সিদ্ধান্ত বাকি — Payout decisions",
      count: metrics.pending_payouts.count,
      tone: "danger",
      href: "/admin/payouts",
      meta: (
        <>
          <PriceTag amountPoisha={metrics.pending_payouts.amount_poisha} />
          {metrics.pending_payouts.oldest_age_secs !== null
            ? ` · সবচেয়ে পুরনো ${formatAgeSeconds(metrics.pending_payouts.oldest_age_secs)} ধরে`
            : ""}
        </>
      ),
    });
  }
  if (metrics.verifications_pending.count > 0) {
    rows.push({
      key: "verifications",
      label: "দোকান যাচাইয়ের আবেদন — Verification reviews",
      count: metrics.verifications_pending.count,
      tone: "accent",
      href: "/admin/verifications?status=pending",
      meta:
        metrics.verifications_pending.oldest_age_secs !== null
          ? `সবচেয়ে পুরনো আবেদন ${formatAgeSeconds(metrics.verifications_pending.oldest_age_secs)} ধরে`
          : "",
    });
  }
  if (metrics.open_returns > 0) {
    rows.push({
      key: "returns",
      label: "রিটার্নের সিদ্ধান্ত বাকি — Return decisions",
      count: metrics.open_returns,
      tone: "accent",
      href: "/admin/returns",
      meta: "অনুরোধ বা অনুমোদিত — সিদ্ধান্ত বাকি",
    });
  }
  if (metrics.refunds_pending.count > 0) {
    rows.push({
      key: "refunds",
      label: "রিফান্ড সম্পন্ন করা বাকি — Refunds to complete",
      count: metrics.refunds_pending.count,
      tone: "danger",
      href: "/admin/returns",
      meta: <PriceTag amountPoisha={metrics.refunds_pending.amount_poisha} />,
    });
  }
  if (metrics.advance_pending.count > 0) {
    const ap = metrics.advance_pending;
    rows.push({
      key: "advance",
      label: "অগ্রিম পেমেন্ট বাকি — Advances unpaid",
      count: ap.count,
      tone: ap.over_24h > 0 ? "danger" : "accent",
      href: "/admin/orders",
      meta: (
        <>
          বাকি <PriceTag amountPoisha={ap.outstanding_poisha} /> · আটকে থাকা
          অর্ডারমূল্য <PriceTag amountPoisha={ap.blocked_total_poisha} />
          {ap.over_24h > 0
            ? ` · ২৪ ঘণ্টার বেশি অপেক্ষায় ${formatCount(ap.over_24h)}টি`
            : ""}
        </>
      ),
    });
  }
  if (metrics.advance_unfulfilled.count > 0) {
    rows.push({
      key: "advance-unfulfilled",
      label: "অগ্রিম ছাড়াই এগিয়ে গেছে — Advances not collected",
      count: metrics.advance_unfulfilled.count,
      tone: "muted",
      href: "/admin/orders",
      meta: (
        <>
          গেটওয়ে না পেয়ে অগ্রিম ছাড়া চলমান · নেওয়ার কথা ছিল{" "}
          <PriceTag amountPoisha={metrics.advance_unfulfilled.required_poisha} />
        </>
      ),
    });
  }
  if (metrics.stale_shipments > 0) {
    rows.push({
      key: "shipments",
      label: "আটকে থাকা শিপমেন্ট — Shipments not moving",
      count: metrics.stale_shipments,
      tone: "accent",
      href: "/admin/shipments",
      meta: "১৫ মিনিটের বেশি হালনাগাদ হয়নি (ম্যানুয়াল ছাড়া)",
    });
  }
  if (nb.stale_sending_count > 0) {
    rows.push({
      key: "stuck-sends",
      label: "আটকে থাকা পাঠানো — Stuck notification sends",
      count: nb.stale_sending_count,
      tone: "danger",
      href: "/admin/notifications",
      meta:
        nb.oldest_sending_claim_age_secs !== null
          ? `সবচেয়ে পুরনো দাবি ${formatAgeSeconds(nb.oldest_sending_claim_age_secs)} ধরে`
          : "দাবির সময়ের ছাপ নেই এমন সারি আছে",
    });
  }
  if (nb.failed > 0) {
    rows.push({
      key: "failed-notifications",
      label: "ব্যর্থ নোটিফিকেশন — Failed notifications",
      count: nb.failed,
      tone: "muted",
      href: "/admin/notifications",
      meta: "সর্বকালীন হিসাব",
    });
  }
  const stockTotal =
    metrics.stock_alerts.out_of_stock + metrics.stock_alerts.low_stock;
  if (stockTotal > 0) {
    rows.push({
      key: "stock",
      label: "স্টক সতর্কতা — Stock alerts",
      count: stockTotal,
      tone: "accent",
      // No admin inventory page exists yet: the counts are
      // shown unlinked rather than linked to a 404.
      meta: `স্টক শেষ ${formatCount(metrics.stock_alerts.out_of_stock)}টি · স্টক কম ${formatCount(metrics.stock_alerts.low_stock)}টি সক্রিয় পণ্য`,
    });
  }

  return (
    <Section title="মনোযোগ দরকার — Needs attention">
      {rows.length === 0 ? (
        <EmptyState title="সব ঠিক আছে — কোনো কাজ বাকি নেই">
          <p>
            পে-আউট, যাচাই, রিটার্ন, অগ্রিম, শিপমেন্ট আর নোটিফিকেশনের সব কিউ
            এখন খালি।
          </p>
        </EmptyState>
      ) : (
        <div className="queue-list">
          {rows.map((row) => {
            const inner = (
              <>
                <span className="queue-row-main">
                  <Badge tone={row.tone}>{formatCount(row.count)}</Badge>
                  <span>{row.label}</span>
                </span>
                <span className="queue-row-meta">{row.meta}</span>
                {row.href ? (
                  <span className="queue-row-arrow" aria-hidden="true">
                    →
                  </span>
                ) : null}
              </>
            );
            return row.href ? (
              <Link key={row.key} href={row.href} className="queue-row">
                {inner}
              </Link>
            ) : (
              <div key={row.key} className="queue-row">
                {inner}
              </div>
            );
          })}
        </div>
      )}
      <p className="admin-muted">
        নোটিফিকেশন: পাঠানো হয়েছে {formatCount(nb.sent)}টি (গত ২৪ ঘণ্টায়{" "}
        {formatCount(nb.sent_last_24h)}টি) · অপেক্ষমাণ{" "}
        {formatCount(nb.pending)}টি
        {nb.oldest_pending_age_secs !== null
          ? `, সবচেয়ে পুরনো ${formatAgeSeconds(nb.oldest_pending_age_secs)} ধরে`
          : ""}
        ।
      </p>
    </Section>
  );
}

// ---------------------------------------------------------------------------
// Order pipeline — sub-order fulfilment + payment status
// ---------------------------------------------------------------------------

const SUB_ORDER_STATUSES = [
  "placed",
  "confirmed",
  "shipped",
  "delivered",
  "returned",
  "cancelled",
] as const;

const PAYMENT_STATUSES = [
  "pay_on_delivery",
  "pending",
  "advance_pending",
  "advance_paid",
  "paid",
  "failed",
  "refunded",
] as const;

function Pipeline({
  metrics,
  overview,
}: {
  metrics: AdminMetrics;
  overview: AdminOverview;
}) {
  const sub = metrics.sub_orders_by_status;
  const pay = metrics.orders_by_payment_status;
  return (
    <Section title="অর্ডারের ধাপ — Order pipeline">
      <p className="admin-muted">সাব-অর্ডারের অবস্থা (দোকানভিত্তিক ধাপ):</p>
      <div className="pipeline-row">
        {SUB_ORDER_STATUSES.map((status) => (
          <span key={status} className="pipeline-chip">
            <Badge
              tone={
                status === "delivered"
                  ? "brand"
                  : status === "shipped"
                    ? "accent"
                    : "muted"
              }
            >
              {orderStatusLabel(status)}
            </Badge>
            <strong>{formatCount(sub[status] ?? 0)}</strong>
          </span>
        ))}
      </div>
      <p className="admin-muted">
        প্যারেন্ট অর্ডার: দেওয়া হয়েছে{" "}
        {formatCount(metrics.orders_by_status["placed"] ?? 0)}টি · বাতিল{" "}
        {formatCount(metrics.orders_by_status["cancelled"] ?? 0)}টি —
        প্যারেন্ট অবস্থা শুধু অর্ডার দেওয়া আর বাতিল পর্যন্তই বদলায়; বাকি
        ধাপ সাব-অর্ডারে চলে।
      </p>
      <p className="admin-muted">পেমেন্টের অবস্থা (প্যারেন্ট অর্ডার):</p>
      <div className="pipeline-row">
        {PAYMENT_STATUSES.map((status) => (
          <span key={status} className="pipeline-chip">
            <Badge
              tone={
                status === "advance_pending" && (pay[status] ?? 0) > 0
                  ? "accent"
                  : "muted"
              }
            >
              {paymentStatusLabel(status)}
            </Badge>
            <strong>{formatCount(pay[status] ?? 0)}</strong>
          </span>
        ))}
      </div>
      <p className="admin-muted">
        অনলাইন পেমেন্ট অপেক্ষমাণ (গেটওয়ে):{" "}
        {formatCount(overview.payments_pending_count)}টি
      </p>
    </Section>
  );
}

// ---------------------------------------------------------------------------
// Geography — zones (complete) + districts (named-district orders only)
// ---------------------------------------------------------------------------

const ZONE_LABELS: Record<string, string> = {
  inside_dhaka: "ঢাকার ভিতরে — Inside Dhaka",
  dhaka_suburb: "ঢাকার উপশহর — Dhaka Suburb",
  outside_dhaka: "ঢাকার বাইরে — Outside Dhaka",
};

function Geography({
  metrics,
  totalOrders,
}: {
  metrics: AdminMetrics;
  totalOrders: number;
}) {
  const zoneMax = Math.max(1, ...metrics.orders_by_zone.map((z) => z.orders));
  const districtRows = metrics.orders_by_district.slice(0, 5);
  const districtMax = Math.max(1, ...districtRows.map((d) => d.orders));
  const districtTotal = metrics.orders_by_district.reduce(
    (sum, row) => sum + row.orders,
    0,
  );
  return (
    <Section title="এলাকা অনুযায়ী — Geography">
      {metrics.orders_by_zone.length === 0 ? (
        <EmptyState title="এখনো কোনো অর্ডার নেই">
          <p>অর্ডার এলেই জোন আর জেলার হিসাব এখানে দেখা যাবে।</p>
        </EmptyState>
      ) : (
        <>
          {metrics.orders_by_zone.map((row) => (
            <div key={row.zone} className="admin-bar-row">
              <span className="admin-bar-label">
                {ZONE_LABELS[row.zone] ?? row.zone}
              </span>
              <span className="admin-bar">
                <span
                  className="admin-bar-fill"
                  style={{ width: `${(row.orders / zoneMax) * 100}%` }}
                />
              </span>
              <strong>{formatCount(row.orders)}</strong>
            </div>
          ))}
          {districtRows.length > 0 ? (
            <>
              <p className="admin-muted">শীর্ষ জেলা — Top districts:</p>
              {districtRows.map((row) => (
                <div key={row.district} className="admin-bar-row">
                  <span className="admin-bar-label">{row.district}</span>
                  <span className="admin-bar">
                    <span
                      className="admin-bar-fill"
                      style={{ width: `${(row.orders / districtMax) * 100}%` }}
                    />
                  </span>
                  <strong>{formatCount(row.orders)}</strong>
                </div>
              ))}
            </>
          ) : null}
          <p className="admin-muted">
            জেলার হিসাব শুধু যেসব অর্ডারে জেলা দেওয়া হয়েছে (
            {formatCount(districtTotal)} / {formatCount(totalOrders)}টি) —
            জোন-মাত্র অর্ডারে জেলা দাবি করা হয় না।
          </p>
        </>
      )}
      <p className="admin-muted">
        ঠিকানা বই — Address book: সংরক্ষিত ঠিকানা{" "}
        {formatCount(metrics.address_book.total_addresses)}টি · ঠিকানা আছে এমন
        ক্রেতা {formatCount(metrics.address_book.buyers_with_addresses)} জন ·
        ডিফল্ট ঠিকানা {formatCount(metrics.address_book.defaults_count)}টি
      </p>
    </Section>
  );
}

// ---------------------------------------------------------------------------
// Platform blocks — totals, settings, media, merchandising, gift cards
// ---------------------------------------------------------------------------

function PlatformBlocks({
  metrics,
  overview,
  settings,
}: {
  metrics: AdminMetrics;
  overview: AdminOverview;
  settings: AdminSettings | null;
}) {
  const gc = metrics.gift_cards;
  const topMedia = metrics.media.top_stores[0];
  const live = metrics.live_flash_sale;
  return (
    <Section
      title="প্ল্যাটফর্ম — Platform"
      linkHref="/admin/settings"
      linkLabel="সেটিংস — Settings"
    >
      <p>
        ব্যবহারকারী {formatCount(metrics.users_total)} (আজ +{" "}
        {formatCount(metrics.growth_today.new_users)}) · দোকান{" "}
        {formatCount(metrics.stores_total)} (আজ +{" "}
        {formatCount(metrics.growth_today.new_stores)}, স্থগিত{" "}
        {formatCount(metrics.stores_suspended)}টি) · সক্রিয় পণ্য{" "}
        {formatCount(metrics.products_active)} / মোট{" "}
        {formatCount(overview.products)} · লাইভ প্রমোশন{" "}
        {formatCount(metrics.active_promotions)}টি
      </p>
      {settings ? (
        <p className="admin-muted">
          বর্তমান বেস কমিশন {Number((settings.commission_bps / 100).toFixed(2))}
          % · সর্বনিম্ন পে-আউট{" "}
          <PriceTag amountPoisha={settings.min_payout_poisha} /> · COD অগ্রিম{" "}
          {Number((settings.advance_percent_bps / 100).toFixed(2))}% (
          <PriceTag amountPoisha={settings.advance_threshold_poisha} />
          -এর বেশি অর্ডারে)
        </p>
      ) : null}
      {live ? (
        <p className="admin-flash-line">
          <Badge tone="flash">লাইভ ফ্ল্যাশ সেল — Live</Badge>{" "}
          <strong>{live.title}</strong> · শেষ হতে বাকি{" "}
          <Countdown endsAt={live.ends_at} /> ·{" "}
          <Link href="/admin/flash-sales">ফ্ল্যাশ সেল — Flash sales</Link>
        </p>
      ) : null}
      <p>
        মার্চেন্ডাইজিং: ব্যানার ক্লিক{" "}
        {formatCount(metrics.merchandising.banner_clicks_total)} (আজ{" "}
        {formatCount(metrics.merchandising.banner_clicks_today)}) · কালেকশন
        ভিউ {formatCount(metrics.merchandising.collection_views_total)} (আজ{" "}
        {formatCount(metrics.merchandising.collection_views_today)}) ·{" "}
        <Link href="/admin/content">কনটেন্ট — Content</Link>
      </p>
      <p>
        মিডিয়া: ছবি {formatCount(metrics.media.images_total)}টি (আপলোড{" "}
        {formatCount(metrics.media.stored_images)} · বাইরের লিংক{" "}
        {formatCount(metrics.media.external_images)}) · আসল আপলোডের আকার{" "}
        {formatBytes(metrics.media.stored_bytes)} (ছোট সংস্করণ ছাড়া)
        {topMedia
          ? ` · সবচেয়ে বেশি: ${topMedia.store_name} (${formatBytes(topMedia.stored_bytes)})`
          : ""}{" "}
        · <Link href="/admin/media">মিডিয়া — Media</Link>
      </p>
      <p>
        গিফট কার্ড: সক্রিয় {formatCount(gc.active_cards)} · নিষ্ক্রিয়{" "}
        {formatCount(gc.disabled_cards)} · মেয়াদ শেষ{" "}
        {formatCount(gc.expired_cards)} · ব্যালেন্স শেষ{" "}
        {formatCount(gc.depleted_cards)} · মোট {formatCount(gc.cards_total)}টি
      </p>
      <p>
        ব্যবহারযোগ্য ব্যালেন্স (বিক্রেতাদের দায়):{" "}
        <PriceTag amountPoisha={gc.outstanding_poisha} /> · নিষ্ক্রিয় কার্ডে
        জমে আছে <PriceTag amountPoisha={gc.frozen_disabled_poisha} /> ·
        মেয়াদ-শেষ কার্ডে <PriceTag amountPoisha={gc.expired_unredeemed_poisha} />
      </p>
      <p className="admin-muted">
        মোট ইস্যু <PriceTag amountPoisha={gc.initial_total_poisha} /> · রিডিম{" "}
        <PriceTag amountPoisha={gc.redeemed_total_poisha} /> · ফেরত-পুনরুদ্ধার{" "}
        <PriceTag amountPoisha={gc.restored_total_poisha} /> · অ্যাডমিন সমন্বয়{" "}
        {formatBdt(gc.adjusted_net_poisha)} · গিফট কার্ড বিক্রেতার অর্থায়নে —
        প্ল্যাটফর্মের কাছে কার্ডের টাকা জমা থাকে না।{" "}
        <Link href="/admin/gift-cards">গিফট কার্ড — Gift cards</Link>
      </p>
    </Section>
  );
}

// ---------------------------------------------------------------------------
// Last 7 days — orders trend + top stores (Wave 27 analytics)
// ---------------------------------------------------------------------------

function TrendSection({ refreshKey }: { refreshKey: number }) {
  const load = useCallback(
    (token: string): Promise<AdminAnalytics> => getAdminAnalytics(token, 7),
    [],
  );
  const { data, error, reload } = useAdminData(load, refreshKey);

  return (
    <Section
      title="গত ৭ দিন — Last 7 days"
      linkHref="/admin/analytics"
      linkLabel="বিশ্লেষণ — Analytics"
    >
      {error && !data ? (
        <SectionError onRetry={reload} />
      ) : !data ? (
        <SectionSkeleton rows={4} />
      ) : (
        <>
          <DailyBars
            rows={data.daily.map((row) => ({
              date: row.date,
              value: row.orders,
              title: `${row.date} — ${formatBdt(row.gmv_poisha)} · ${row.orders}টি অর্ডার (বাতিল বাদে)`,
            }))}
          />
          <p className="admin-muted">
            প্রতিদিনের অর্ডার (বাতিল বাদে) — বারে হোভার করলে দিনের GMV দেখা
            যাবে।
          </p>
          {data.top_stores.length > 0 ? (
            <>
              <p className="admin-muted">শীর্ষ দোকান (GMV) — Top stores:</p>
              <ul className="admin-plain-list">
                {data.top_stores.slice(0, 5).map((store) => (
                  <li key={store.store_id}>
                    {store.name} — <PriceTag amountPoisha={store.gmv_poisha} />{" "}
                    · {formatCount(store.sub_orders)}টি সাব-অর্ডার
                  </li>
                ))}
              </ul>
            </>
          ) : (
            <p className="admin-muted">
              এই ৭ দিনে কোনো দোকানের বিক্রি হয়নি।
            </p>
          )}
        </>
      )}
    </Section>
  );
}

// ---------------------------------------------------------------------------
// Search analytics — catalogue gaps first
// ---------------------------------------------------------------------------

function SearchSection({ refreshKey }: { refreshKey: number }) {
  const load = useCallback(
    (token: string): Promise<SearchAnalytics> => getSearchAnalytics(token),
    [],
  );
  const { data, error, reload } = useAdminData(load, refreshKey);

  return (
    <Section
      title="খোঁজের হিসাব — Search analytics"
      linkHref="/admin/search-analytics"
      linkLabel="পুরো হিসাব — Full report"
    >
      {error && !data ? (
        <SectionError onRetry={reload} />
      ) : !data ? (
        <SectionSkeleton rows={3} />
      ) : data.total_searches === 0 ? (
        <EmptyState title="এখনো কোনো খোঁজ হয়নি — No searches yet">
          <p>ক্রেতারা খোঁজ শুরু করলেই হিসাব এখানে জমবে।</p>
        </EmptyState>
      ) : (
        <>
          <p>
            গত {data.window_days} দিনে (rolling) মোট খোঁজ:{" "}
            <strong>{formatCount(data.total_searches)}</strong>টি
          </p>
          {data.zero_result_searches.length > 0 ? (
            <>
              <p className="admin-muted">
                যা খুঁজে পাওয়া যায়নি — Zero-result searches (ক্যাটালগের
                ঘাটতি):
              </p>
              <ul className="admin-plain-list">
                {data.zero_result_searches.slice(0, 5).map((row) => (
                  <li key={row.query_text}>
                    “{row.query_text}” — {formatCount(row.searches)} বার, শেষ{" "}
                    {formatDateTime(row.last_searched_at)}
                  </li>
                ))}
              </ul>
            </>
          ) : null}
          {data.top_searches.length > 0 ? (
            <>
              <p className="admin-muted">সবচেয়ে বেশি খোঁজা — Top searches:</p>
              <ul className="admin-plain-list">
                {data.top_searches.slice(0, 5).map((row) => (
                  <li key={row.query_text}>
                    “{row.query_text}” — {formatCount(row.searches)} বার
                  </li>
                ))}
              </ul>
            </>
          ) : null}
        </>
      )}
    </Section>
  );
}

// ---------------------------------------------------------------------------
// Recent merchandising events
// ---------------------------------------------------------------------------

function MerchEventsSection({ refreshKey }: { refreshKey: number }) {
  const load = useCallback(
    (token: string): Promise<MerchEventView[]> =>
      listAdminMerchEvents(token, 10),
    [],
  );
  const { data, error, reload } = useAdminData(load, refreshKey);

  return (
    <Section
      title="সাম্প্রতিক মার্চেন্ডাইজিং ইভেন্ট — Recent events"
      linkHref="/admin/content"
      linkLabel="কনটেন্ট — Content"
    >
      {error && !data ? (
        <SectionError onRetry={reload} />
      ) : !data ? (
        <SectionSkeleton rows={3} />
      ) : data.length === 0 ? (
        <EmptyState title="এখনো কোনো মার্চেন্ডাইজিং ইভেন্ট নেই">
          <p>ব্যানার ক্লিক আর কালেকশন ভিউ এখানে দেখা যাবে।</p>
        </EmptyState>
      ) : (
        <ul className="admin-plain-list">
          {data.map((event) => (
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
      )}
    </Section>
  );
}

// ---------------------------------------------------------------------------
// Latest admin actions — the audit trail's head
// ---------------------------------------------------------------------------

function AuditSection({ refreshKey }: { refreshKey: number }) {
  const load = useCallback(
    async (token: string): Promise<AdminAuditEntry[]> => {
      const list = await getAdminAuditLog(token, { perPage: 5 });
      return list.items;
    },
    [],
  );
  const { data, error, reload } = useAdminData(load, refreshKey);

  return (
    <Section
      title="সর্বশেষ অ্যাডমিন কাজ — Latest admin actions"
      linkHref="/admin/audit"
      linkLabel="অডিট লগ — Audit log"
    >
      {error && !data ? (
        <SectionError onRetry={reload} />
      ) : !data ? (
        <SectionSkeleton rows={3} />
      ) : data.length === 0 ? (
        <EmptyState title="এখনো কোনো অ্যাডমিন কাজের রেকর্ড নেই">
          <p>গুরুত্বপূর্ণ অ্যাডমিন কাজগুলো এখানে দেখা যাবে।</p>
        </EmptyState>
      ) : (
        <ul className="admin-plain-list">
          {data.map((entry) => (
            <li key={entry.id}>
              {entry.action} · {entry.admin_phone_masked} ·{" "}
              {formatDhakaDateTime(entry.created_at)}
            </li>
          ))}
        </ul>
      )}
    </Section>
  );
}
