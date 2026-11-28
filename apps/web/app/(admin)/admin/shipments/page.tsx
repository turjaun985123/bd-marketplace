"use client";

import {
  Suspense,
  useCallback,
  useEffect,
  useRef,
  useState,
} from "react";
import Link from "next/link";
import { useSearchParams } from "next/navigation";
import AdminShell from "@/components/AdminShell";
import PriceTag from "@/components/PriceTag";
import { Badge, Button, EmptyState, Skeleton } from "@/components/ui";
import {
  ApiCallError,
  courierDisplayName,
  getAdminShipmentDetail,
  listAdminShipments,
  refreshAdminShipment,
  shipmentStatusLabel,
  type AdminShipment,
  type AdminShipmentDetail,
  type AdminShipmentList,
  type AdminShipmentSort,
} from "@/lib/api";
import {
  formatAgeSeconds,
  formatCount,
  formatDhakaDateTime,
} from "@/lib/format";
import { useAuth } from "@/hooks/useAuth";

const PAGE_LIMIT = 25;

const STATUS_FILTERS: { value: string; label: string }[] = [
  { value: "", label: "সব অবস্থা — All statuses" },
  { value: "created", label: "তৈরি হয়েছে — Created" },
  { value: "picked_up", label: "পিক হয়েছে — Picked up" },
  { value: "in_transit", label: "পথে আছে — In transit" },
  { value: "out_for_delivery", label: "ডেলিভারির জন্য বেরিয়েছে — Out for delivery" },
  { value: "delivered", label: "ডেলিভারি হয়েছে — Delivered" },
  { value: "partial_delivered", label: "আংশিক ডেলিভারি — Partially delivered" },
  { value: "return_in_transit", label: "ফেরত আসছে — Return in transit" },
  { value: "returned", label: "ফেরত হয়েছে — Returned" },
  { value: "cancelled", label: "বাতিল — Cancelled" },
  { value: "unknown", label: "অজানা — Unknown" },
];

const COURIER_FILTERS: { value: string; label: string }[] = [
  { value: "", label: "সব কুরিয়ার — All couriers" },
  { value: "steadfast", label: "Steadfast" },
  { value: "pathao", label: "Pathao" },
  { value: "redx", label: "RedX" },
  { value: "ecourier", label: "eCourier" },
  { value: "paperfly", label: "Paperfly" },
  { value: "manual", label: "ম্যানুয়াল — Manual" },
];

const SORT_OPTIONS: { value: AdminShipmentSort; label: string }[] = [
  { value: "newest", label: "সবচেয়ে নতুন আগে — Newest first" },
  { value: "oldest", label: "সবচেয়ে পুরনো আগে — Oldest first" },
  { value: "stale_first", label: "সবচেয়ে পুরনো হালনাগাদ আগে — Stalest first" },
  { value: "cod_desc", label: "COD বেশি আগে — COD: high to low" },
];

const VALID_QUEUES = ["unrefreshed", "poll_overdue", "status_aged"];

function statusTone(
  status: string
): "brand" | "accent" | "danger" | "muted" {
  switch (status) {
    case "delivered":
      return "brand";
    case "returned":
    case "cancelled":
    case "unknown":
      return "danger";
    case "partial_delivered":
    case "return_in_transit":
      return "accent";
    default:
      return "muted";
  }
}

function eventSourceLabel(source: string): string {
  switch (source) {
    case "create":
      return "বুকিং — Booking";
    case "refresh":
      return "পল — Poll";
    case "manual":
      return "ম্যানুয়াল (বিক্রেতা) — Manual (seller)";
    default:
      return source;
  }
}

/**
 * Admin shipments — the cross-store, cross-courier
 * oversight queue, built by the admin remake program
 * (seventh surface). Before this surface the admin had
 * NO shipment view at all, and the overview's
 * Needs-attention queue linked here to a dead page.
 *
 * - Two different "stuck" facts are kept apart: POLL
 *   OVERDUE (no courier poll attempted in 15 minutes —
 *   a platform/worker fact) vs STATUS AGED (in one
 *   status past its per-status threshold — a
 *   courier/seller fact). The overview's count is the
 *   third, older fact: UNREFRESHED (no status change in
 *   15 minutes), which this queue reproduces exactly.
 * - The one action is a single-parcel refresh through
 *   the SAME shared core the worker poller runs — no
 *   bulk variant, no status editing. An advance is
 *   audited server-side; a manual shipment answers an
 *   honest "unchanged" (nothing exists to poll) and the
 *   button says so instead of faking a success.
 * - COD figures are the stored booking snapshots.
 *   "Delivered COD held" is an UNRECONCILED upper bound:
 *   no remittance tracking exists anywhere in the
 *   platform, so the page never claims a courier has
 *   settled — and advance-paid parcels that came back
 *   with no return request are flagged as what they
 *   are: buyer money with no refund path yet.
 */
export default function AdminShipmentsPage() {
  return (
    <AdminShell>
      <Suspense fallback={<p>লোড হচ্ছে…</p>}>
        <Shipments />
      </Suspense>
    </AdminShell>
  );
}

type RefreshMsg = { kind: "advanced" | "unchanged" | "error"; text: string };

function Shipments() {
  const { token } = useAuth();
  const searchParams = useSearchParams();
  const initialQueue = (() => {
    const param = searchParams.get("queue") ?? "";
    return VALID_QUEUES.includes(param) ? param : "";
  })();
  const storeId = searchParams.get("store_id") ?? "";

  const [status, setStatus] = useState("");
  const [courier, setCourier] = useState("");
  const [queue, setQueue] = useState(initialQueue);
  const [sort, setSort] = useState<AdminShipmentSort>(
    initialQueue ? "stale_first" : "newest"
  );
  const [qInput, setQInput] = useState("");
  const [q, setQ] = useState("");
  const [page, setPage] = useState(1);

  const [list, setList] = useState<AdminShipmentList | null>(null);
  const [loadError, setLoadError] = useState(false);
  const [stale, setStale] = useState(false);
  const [asOf, setAsOf] = useState<string | null>(null);
  const [tick, setTick] = useState(0);

  const [notice, setNotice] = useState<string | null>(null);

  const [expandedId, setExpandedId] = useState<string | null>(null);
  const [detail, setDetail] = useState<AdminShipmentDetail | null>(null);
  const [detailError, setDetailError] = useState(false);
  const [loadingDetailId, setLoadingDetailId] = useState<string | null>(null);

  const [refreshBusyId, setRefreshBusyId] = useState<string | null>(null);
  const [refreshMsg, setRefreshMsg] = useState<RefreshMsg | null>(null);

  // Debounced search: typing filters after a short pause;
  // the input itself never waits on the network.
  useEffect(() => {
    const timer = setTimeout(() => {
      setQ(qInput.trim());
      setPage(1);
    }, 300);
    return () => clearTimeout(timer);
  }, [qInput]);

  /** Whether any list load has ever succeeded — decides
   * stale-banner vs full error on a failed load. */
  const hasDataRef = useRef(false);

  const load = useCallback(async () => {
    if (!token) return;
    try {
      const next = await listAdminShipments(token, {
        q: q || undefined,
        status: status || undefined,
        courier: courier || undefined,
        store_id: storeId || undefined,
        queue: queue || undefined,
        sort,
        page,
        limit: PAGE_LIMIT,
      });
      hasDataRef.current = true;
      setList(next);
      setLoadError(false);
      setStale(false);
      setAsOf(new Date().toISOString());
    } catch {
      if (hasDataRef.current) setStale(true);
      else setLoadError(true);
    }
  }, [token, status, courier, storeId, queue, q, sort, page, tick]);

  useEffect(() => {
    void load();
  }, [load]);

  const reload = useCallback(() => setTick((value) => value + 1), []);

  function changeStatus(value: string) {
    setStatus(value);
    setPage(1);
  }

  function changeCourier(value: string) {
    setCourier(value);
    setPage(1);
  }

  function changeQueue(value: string) {
    setQueue(value);
    setPage(1);
  }

  function changeSort(value: AdminShipmentSort) {
    setSort(value);
    setPage(1);
  }

  async function loadDetail(id: string) {
    if (!token) return;
    setLoadingDetailId(id);
    setDetailError(false);
    try {
      const next = await getAdminShipmentDetail(token, id);
      setDetail(next);
    } catch {
      setDetailError(true);
    } finally {
      setLoadingDetailId(null);
    }
  }

  async function toggleDetail(shipment: AdminShipment) {
    if (expandedId === shipment.id) {
      setExpandedId(null);
      setDetail(null);
      setRefreshMsg(null);
      return;
    }
    setExpandedId(shipment.id);
    setDetail(null);
    setRefreshMsg(null);
    await loadDetail(shipment.id);
  }

  async function doRefresh(shipment: AdminShipment) {
    if (!token || refreshBusyId) return;
    setRefreshBusyId(shipment.id);
    setRefreshMsg(null);
    try {
      const result = await refreshAdminShipment(token, shipment.id);
      if (result.outcome === "advanced") {
        setRefreshMsg({
          kind: "advanced",
          text: `হালনাগাদ হয়েছে — কুরিয়ার নতুন অবস্থা জানিয়েছে: ${shipmentStatusLabel(
            result.shipment.status
          )}। (The courier reported a new status; the advance is recorded in the audit log.)`,
        });
        setNotice(null);
        reload();
        await loadDetail(shipment.id);
      } else if (shipment.courier === "manual") {
        setRefreshMsg({
          kind: "unchanged",
          text: "ম্যানুয়াল কুরিয়ার — পল করার কিছু নেই। অবস্থা বিক্রেতা নিজে লিখেন। (Manual courier: there is nothing to poll — the seller enters the status.)",
        });
      } else {
        setRefreshMsg({
          kind: "unchanged",
          text: "কোনো পরিবর্তন হয়নি — কুরিয়ার বর্তমান অবস্থাই জানিয়েছে। (No change — the courier confirms the current status.)",
        });
        await loadDetail(shipment.id);
      }
    } catch (error) {
      setRefreshMsg({
        kind: "error",
        text:
          error instanceof ApiCallError
            ? `রিফ্রেশ ব্যর্থ হয়েছে — ${error.message}`
            : "রিফ্রেশ ব্যর্থ হয়েছে — Refresh failed.",
      });
    } finally {
      setRefreshBusyId(null);
    }
  }

  const totalPages = list ? Math.max(1, Math.ceil(list.total / PAGE_LIMIT)) : 1;
  const summary = list?.summary ?? null;

  return (
    <>
      <div className="admin-overview-head">
        <h1>শিপমেন্ট তদারকি — Shipment oversight</h1>
        {asOf ? (
          <p className="admin-muted">
            সর্বশেষ লোড: {formatDhakaDateTime(asOf)} — Loaded{" "}
            {formatDhakaDateTime(asOf)}
          </p>
        ) : null}
      </div>

      {summary ? (
        <SummaryStrip
          summary={summary}
          activeQueue={queue}
          onPickQueue={changeQueue}
          activeStatus={status}
          onPickStatus={changeStatus}
        />
      ) : null}

      {summary ? <CodStrip summary={summary} /> : null}

      {summary ? <CourierRollup summary={summary} /> : null}

      <div className="payout-controls">
        <label>
          অবস্থা — Status
          <select value={status} onChange={(e) => changeStatus(e.target.value)}>
            {STATUS_FILTERS.map((f) => (
              <option key={f.value} value={f.value}>
                {f.label}
              </option>
            ))}
          </select>
        </label>
        <label>
          কুরিয়ার — Courier
          <select
            value={courier}
            onChange={(e) => changeCourier(e.target.value)}
          >
            {COURIER_FILTERS.map((f) => (
              <option key={f.value} value={f.value}>
                {f.label}
              </option>
            ))}
          </select>
        </label>
        <label>
          সাজানো — Sort
          <select
            value={sort}
            onChange={(e) => changeSort(e.target.value as AdminShipmentSort)}
          >
            {SORT_OPTIONS.map((o) => (
              <option key={o.value} value={o.value}>
                {o.label}
              </option>
            ))}
          </select>
        </label>
        <label>
          খুঁজুন — Search
          <input
            type="search"
            value={qInput}
            onChange={(e) => setQInput(e.target.value)}
            placeholder="কনসাইনমেন্ট, নাম, ফোন বা আইডি"
          />
        </label>
      </div>

      {storeId ? (
        <p className="admin-flash-line" role="status">
          একটি স্টোরের শিপমেন্ট দেখানো হচ্ছে —{" "}
          <Link href="/admin/shipments">সব স্টোরের শিপমেন্ট দেখুন</Link>
        </p>
      ) : null}

      {notice ? (
        <p role="status" className="admin-flash-line">
          {notice}
        </p>
      ) : null}
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

      {!list && !loadError ? (
        <div className="admin-skel" aria-hidden="true">
          {Array.from({ length: 4 }, (_, i) => (
            <Skeleton key={i} width="100%" height={72} />
          ))}
        </div>
      ) : null}

      {loadError ? (
        <div className="admin-error">
          <p>শিপমেন্ট লোড করা যায়নি — Couldn&apos;t load shipments.</p>
          <Button size="sm" onClick={reload}>
            আবার চেষ্টা করুন — Retry
          </Button>
        </div>
      ) : null}

      {list && list.items.length === 0 ? (
        <EmptyState title={emptyTitle(queue, status, q)}>
          <p>{emptyBody(queue, status, q)}</p>
        </EmptyState>
      ) : null}

      {list ? (
        <div className="queue-list">
          {list.items.map((shipment) => (
            <ShipmentCard
              key={shipment.id}
              shipment={shipment}
              expanded={expandedId === shipment.id}
              detail={expandedId === shipment.id ? detail : null}
              detailError={expandedId === shipment.id && detailError}
              detailLoading={loadingDetailId === shipment.id}
              refreshBusy={refreshBusyId === shipment.id}
              refreshMsg={expandedId === shipment.id ? refreshMsg : null}
              onToggleDetail={() => void toggleDetail(shipment)}
              onRetryDetail={() => void loadDetail(shipment.id)}
              onRefresh={() => void doRefresh(shipment)}
            />
          ))}
        </div>
      ) : null}

      {list && list.total > PAGE_LIMIT ? (
        <div className="payout-controls">
          <Button
            size="sm"
            disabled={page <= 1}
            onClick={() => setPage((p) => Math.max(1, p - 1))}
          >
            ← আগের — Previous
          </Button>
          <span className="admin-muted">
            পেজ {formatCount(page)} / {formatCount(totalPages)} — মোট{" "}
            {formatCount(list.total)}টি
          </span>
          <Button
            size="sm"
            disabled={page >= totalPages}
            onClick={() => setPage((p) => Math.min(totalPages, p + 1))}
          >
            পরের — Next →
          </Button>
        </div>
      ) : null}
    </>
  );
}

function emptyTitle(queue: string, status: string, q: string): string {
  if (q) return "খোঁজে কোনো শিপমেন্ট মেলেনি — No shipments match the search";
  if (queue === "unrefreshed")
    return "কোনো শিপমেন্ট আটকে নেই — No shipments are unrefreshed";
  if (queue === "poll_overdue")
    return "কোনো পল বাকি নেই — No polls are overdue";
  if (queue === "status_aged")
    return "কোনো শিপমেন্ট মেয়াদ পেরোয়নি — No shipments are aged";
  if (status) return "এই অবস্থায় কোনো শিপমেন্ট নেই — No shipments in this status";
  return "কোনো শিপমেন্ট নেই — No shipments yet";
}

function emptyBody(queue: string, status: string, q: string): string {
  if (q)
    return "কনসাইনমেন্ট নম্বর, নাম বা আইডির অন্য অংশ দিয়ে খুঁজে দেখুন। (Try another part of the consignment id, name or id.)";
  if (queue)
    return "এই সারিতে এখন কোনো কাজ বাকি নেই। (Nothing is waiting in this queue right now.)";
  if (status) return "ফিল্টার বদলে আবার দেখুন। (Change the filter to see more.)";
  return "প্রথম বুকিং হলেই এখানে দেখা যাবে। (Shipments appear here once booked.)";
}

// ---------------------------------------------------------------------------
// Summary strip: the work queues + the whole-platform counts
// ---------------------------------------------------------------------------

function SummaryStrip({
  summary,
  activeQueue,
  onPickQueue,
  activeStatus,
  onPickStatus,
}: {
  summary: AdminShipmentList["summary"];
  activeQueue: string;
  onPickQueue: (queue: string) => void;
  activeStatus: string;
  onPickStatus: (status: string) => void;
}) {
  const chips: { queue: string; label: string; count: number }[] = [
    { queue: "", label: "সব শিপমেন্ট", count: summary.total },
    { queue: "unrefreshed", label: "হালনাগাদ হয়নি", count: summary.unrefreshed },
    { queue: "poll_overdue", label: "পল বাকি", count: summary.poll_overdue },
    { queue: "status_aged", label: "মেয়াদ পেরিয়েছে", count: summary.status_aged },
  ];
  return (
    <div className="pipeline-row" aria-label="শিপমেন্ট সারাংশ — Shipment summary">
      {chips.map((chip) => (
        <span key={chip.queue || "all"} className="pipeline-chip">
          <Button
            size="sm"
            variant={activeQueue === chip.queue ? "primary" : "default"}
            onClick={() => onPickQueue(chip.queue)}
          >
            {chip.label}: {formatCount(chip.count)}টি
          </Button>
        </span>
      ))}
      <span className="pipeline-chip">
        <Button
          size="sm"
          variant={activeStatus === "unknown" ? "primary" : "default"}
          onClick={() =>
            onPickStatus(activeStatus === "unknown" ? "" : "unknown")
          }
        >
          অজানা অবস্থা: {formatCount(summary.by_status.unknown)}টি
        </Button>
      </span>
      <span className="pipeline-chip">
        <Badge tone="muted">
          চলমান: {formatCount(summary.non_terminal)}টি · ডেলিভারি হয়েছে:{" "}
          {formatCount(summary.by_status.delivered)}টি · ফেরত:{" "}
          {formatCount(summary.by_status.returned)}টি
        </Badge>
      </span>
    </div>
  );
}

// ---------------------------------------------------------------------------
// COD exposure: stored snapshots, honestly labelled
// ---------------------------------------------------------------------------

function CodStrip({ summary }: { summary: AdminShipmentList["summary"] }) {
  const stranded = summary.advance_stranded;
  return (
    <div className="pipeline-row" aria-label="COD সারাংশ — COD summary">
      <span className="pipeline-chip">
        <Badge tone="brand">
          ডেলিভারি হওয়া COD কুরিয়ারের কাছে:{" "}
          <PriceTag amountPoisha={summary.cod_delivered_held_poisha} />
        </Badge>
      </span>
      <span className="pipeline-chip">
        <Badge tone="muted">
          পথে সংগ্রহ বাকি:{" "}
          <PriceTag amountPoisha={summary.cod_in_flight_expected_poisha} /> ·
          আংশিক (সর্বোচ্চ): <PriceTag amountPoisha={summary.cod_partial_poisha} />
        </Badge>
      </span>
      {summary.cod_delivered_over_7d_poisha > 0 ? (
        <span className="pipeline-chip">
          <Badge tone="accent">
            ৭ দিনের পুরনো ধরা COD:{" "}
            <PriceTag amountPoisha={summary.cod_delivered_over_7d_poisha} /> · ৩০
            দিনের পুরনো:{" "}
            <PriceTag amountPoisha={summary.cod_delivered_over_30d_poisha} />
          </Badge>
        </span>
      ) : null}
      {stranded.shipments > 0 ? (
        <span className="pipeline-chip">
          <Badge tone="danger">
            ফেরত আসা পার্সেলে আটকে থাকা অগ্রিম: {formatCount(stranded.shipments)}
            টি শিপমেন্ট, <PriceTag amountPoisha={stranded.advance_paid_poisha} />{" "}
            — রিফান্ডের পথ নেই
          </Badge>
        </span>
      ) : null}
      <span className="pipeline-chip">
        <span className="admin-muted">
          COD অঙ্কগুলো বুকিংয়ের সময়ের সংরক্ষিত হিসাব; কুরিয়ারের সঙ্গে মিল
          (রেমিট্যান্স) ট্র্যাক করা হয় না — তাই এগুলো সর্বোচ্চ হিসাব, প্রাপ্ত
          অর্থ নয়। (Stored booking snapshots, unreconciled — upper bounds, not
          received money.)
        </span>
      </span>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Per-courier rollup
// ---------------------------------------------------------------------------

function CourierRollup({ summary }: { summary: AdminShipmentList["summary"] }) {
  return (
    <section aria-label="কুরিয়ার অনুযায়ী — By courier">
      <h2>কুরিয়ার অনুযায়ী — By courier</h2>
      {summary.couriers_dev_mode ? (
        <p className="payout-warning" role="alert">
          ডেভ মোড চালু আছে — API কুরিয়ারদের বুকিং ও পল সিমুলেটেড। ম্যানুয়াল
          কুরিয়ারের হিসাব সব মোডেই আসল। (DEV mode is on: API-courier bookings
          and polls are simulated. Manual couriers are real in every mode.)
        </p>
      ) : null}
      <table className="merch-table">
        <thead>
          <tr>
            <th>কুরিয়ার — Courier</th>
            <th>মোট — Total</th>
            <th>তৈরি / পথে — Created / in flight</th>
            <th>ডেলিভারি — Delivered</th>
            <th>ফেরত / বাতিল — Returned / cancelled</th>
            <th>অজানা — Unknown</th>
            <th>ধরা COD — COD held</th>
            <th>সবচেয়ে পুরনো চলমান — Oldest live</th>
          </tr>
        </thead>
        <tbody>
          {summary.couriers.map((row) => (
            <tr key={`${row.courier}-${row.display_name ?? ""}`}>
              <td>
                {courierDisplayName(row.courier, row.display_name)}
                {row.mode ? (
                  <>
                    {" "}
                    <Badge
                      tone={
                        row.mode === "live"
                          ? "brand"
                          : row.mode === "dev"
                            ? "accent"
                            : "muted"
                      }
                    >
                      {row.mode === "live"
                        ? "লাইভ"
                        : row.mode === "dev"
                          ? "ডেভ"
                          : "ম্যানুয়াল"}
                    </Badge>
                  </>
                ) : null}
                {!row.available && row.reason ? (
                  <>
                    {" "}
                    <Badge tone="danger">অনুপলব্ধ — {row.reason}</Badge>
                  </>
                ) : null}
                {row.dev_shipment_count > 0 ? (
                  <>
                    {" "}
                    <Badge tone="accent">
                      ডেভ বুকিং: {formatCount(row.dev_shipment_count)}টি
                    </Badge>
                  </>
                ) : null}
              </td>
              <td>{formatCount(row.total)}</td>
              <td>
                {formatCount(row.created)} / {formatCount(row.in_flight)}
              </td>
              <td>{formatCount(row.delivered)}</td>
              <td>
                {formatCount(row.returned)} / {formatCount(row.cancelled)}
              </td>
              <td>{formatCount(row.unknown)}</td>
              <td>
                <PriceTag amountPoisha={row.cod_delivered_held_poisha} />
              </td>
              <td>
                {row.oldest_non_terminal_at
                  ? formatDhakaDateTime(row.oldest_non_terminal_at)
                  : "—"}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </section>
  );
}

// ---------------------------------------------------------------------------
// One queue card
// ---------------------------------------------------------------------------

function ShipmentCard({
  shipment,
  expanded,
  detail,
  detailError,
  detailLoading,
  refreshBusy,
  refreshMsg,
  onToggleDetail,
  onRetryDetail,
  onRefresh,
}: {
  shipment: AdminShipment;
  expanded: boolean;
  detail: AdminShipmentDetail | null;
  detailError: boolean;
  detailLoading: boolean;
  refreshBusy: boolean;
  refreshMsg: RefreshMsg | null;
  onToggleDetail: () => void;
  onRetryDetail: () => void;
  onRefresh: () => void;
}) {
  const isDev = shipment.consignment_id.startsWith("DEV-");
  return (
    <article className="payout-card">
      <div className="payout-card-head">
        <strong>
          {courierDisplayName(shipment.courier, shipment.manual_courier_name)}
        </strong>
        <Badge tone={statusTone(shipment.status)}>
          {shipmentStatusLabel(shipment.status)}
        </Badge>
        {shipment.poll_overdue ? (
          <Badge tone="danger">পল বাকি — Poll overdue</Badge>
        ) : null}
        {shipment.aged ? (
          <Badge tone="accent">মেয়াদ পেরিয়েছে — Aged</Badge>
        ) : null}
        {!shipment.poll_overdue && shipment.unrefreshed ? (
          <Badge tone="muted">হালনাগাদ হয়নি — Unrefreshed</Badge>
        ) : null}
        {isDev ? <Badge tone="accent">ডেভ — DEV</Badge> : null}
        {shipment.has_return_request ? (
          <Badge tone="muted">রিটার্ন অনুরোধ আছে — Return filed</Badge>
        ) : null}
      </div>
      <p className="admin-muted">
        কনসাইনমেন্ট: {shipment.consignment_id}
        {shipment.manual_tracking_id
          ? ` · রিসিপ্ট: ${shipment.manual_tracking_id}`
          : ""}
        {shipment.tracking_code ? ` · ট্র্যাকিং: ${shipment.tracking_code}` : ""}
      </p>
      <p>
        {shipment.delivery_name} · {shipment.delivery_phone_masked} ·{" "}
        {shipment.delivery_zone}
        {shipment.delivery_district ? ` · ${shipment.delivery_district}` : ""}{" "}
        — {shipment.store_name}
      </p>
      <p className="admin-muted">
        এই অবস্থায় আছে: {formatAgeSeconds(shipment.status_age_secs)} · শেষ পল:{" "}
        {shipment.last_polled_at
          ? formatDhakaDateTime(shipment.last_polled_at)
          : shipment.courier === "manual"
            ? "ম্যানুয়াল — পল হয় না"
            : "এখনো পল হয়নি"}{" "}
        · COD: <PriceTag amountPoisha={shipment.cod_amount_poisha} />
      </p>
      <div className="payout-controls">
        <Button size="sm" variant="ghost" onClick={onToggleDetail}>
          {expanded ? "বন্ধ করুন — Close" : "বিস্তারিত — Details"}
        </Button>
        {shipment.courier !== "manual" ? (
          <Button size="sm" loading={refreshBusy} onClick={onRefresh}>
            এখনই পল করুন — Refresh now
          </Button>
        ) : null}
      </div>
      {expanded ? (
        <div className="payout-detail">
          {detailLoading ? (
            <>
              <Skeleton width="100%" height={24} />
              <Skeleton width="100%" height={24} />
              <Skeleton width="100%" height={24} />
            </>
          ) : detailError ? (
            <>
              <p>বিস্তারিত লোড করা যায়নি — Couldn&apos;t load the detail.</p>
              <Button size="sm" onClick={onRetryDetail}>
                আবার চেষ্টা করুন — Retry
              </Button>
            </>
          ) : detail ? (
            <DetailBody
              detail={detail}
              refreshBusy={refreshBusy}
              refreshMsg={refreshMsg}
              onRefresh={onRefresh}
            />
          ) : null}
        </div>
      ) : null}
    </article>
  );
}

// ---------------------------------------------------------------------------
// The expanded detail
// ---------------------------------------------------------------------------

function DetailBody({
  detail,
  refreshBusy,
  refreshMsg,
  onRefresh,
}: {
  detail: AdminShipmentDetail;
  refreshBusy: boolean;
  refreshMsg: RefreshMsg | null;
  onRefresh: () => void;
}) {
  const isManual = detail.courier === "manual";
  return (
    <div>
      {detail.advance_stranded ? (
        <p className="payout-warning" role="alert">
          এই পার্সেল ফেরত এসেছে, কিন্তু ক্রেতা অগ্রিম{" "}
          <PriceTag amountPoisha={detail.advance_paid_poisha} /> দিয়েছিলেন এবং
          কোনো রিটার্ন অনুরোধ নেই — এই টাকার রিফান্ডের কোনো পথ বর্তমানে নেই।
          (This parcel came back after the buyer paid an advance, and no return
          request exists — there is currently no refund path for this money.)
        </p>
      ) : null}

      <h3>ডেলিভারি — Delivery</h3>
      <p>
        {detail.delivery_name} · {detail.delivery_phone}
        <br />
        {detail.delivery_address} · {detail.delivery_zone}
        {detail.delivery_district ? ` · ${detail.delivery_district}` : ""}
        {detail.order_note ? (
          <>
            <br />
            নোট: {detail.order_note}
          </>
        ) : null}
        <br />
        <span className="admin-muted">
          ক্রেতার অ্যাকাউন্ট ফোন (মাস্ক করা): {detail.buyer_phone_masked}
        </span>
      </p>

      <h3>অর্ডার ও পেমেন্ট — Order &amp; payment</h3>
      <p>
        অর্ডার: {detail.order_id} · সাব-অর্ডার অবস্থা: {detail.sub_order_status}{" "}
        · পেমেন্ট: {detail.payment_method} / {detail.payment_status}
        <br />
        পণ্য: <PriceTag amountPoisha={detail.goods_subtotal_poisha} /> · ছাড়:{" "}
        <PriceTag amountPoisha={detail.discount_poisha} /> · ডেলিভারি চার্জ:{" "}
        <PriceTag amountPoisha={detail.delivery_charge_poisha} /> · মোট:{" "}
        <PriceTag amountPoisha={detail.grand_total_poisha} />
        <br />
        কুরিয়ারের সংগ্রহ (COD):{" "}
        <PriceTag amountPoisha={detail.cod_amount_poisha} />
        {detail.advance_paid_poisha > 0 ? (
          <>
            {" "}
            · দেওয়া অগ্রিম:{" "}
            <PriceTag amountPoisha={detail.advance_paid_poisha} />
          </>
        ) : null}
      </p>
      {detail.items.length > 0 ? (
        <ul>
          {detail.items.map((item, index) => (
            <li key={index}>
              {item.title_snapshot}
              {item.variant_name ? ` (${item.variant_name})` : ""} ×{" "}
              {formatCount(item.quantity)} —{" "}
              <PriceTag amountPoisha={item.unit_price_poisha} />
            </li>
          ))}
        </ul>
      ) : null}

      <h3>ধাপ — Steps</h3>
      <p>
        {detail.steps.map((step, index) => (
          <span key={index}>
            {index > 0 ? " → " : ""}
            {step.kind} ({step.state})
          </span>
        ))}
      </p>

      <h3>ঘটনা — Events</h3>
      <table className="merch-table">
        <thead>
          <tr>
            <th>সময় — When</th>
            <th>অবস্থা — Status</th>
            <th>উৎস — Source</th>
            <th>কুরিয়ারের ভাষ্য — Courier&apos;s words</th>
          </tr>
        </thead>
        <tbody>
          {detail.events.map((event) => (
            <tr key={event.id}>
              <td>{formatDhakaDateTime(event.at)}</td>
              <td>{shipmentStatusLabel(event.status)}</td>
              <td>{eventSourceLabel(event.source)}</td>
              <td>{event.raw_status ?? "—"}</td>
            </tr>
          ))}
        </tbody>
      </table>

      {detail.return_request ? (
        <p>
          রিটার্ন অনুরোধ: {detail.return_request.status} · রিফান্ড:{" "}
          {detail.return_request.refund_status}
          {detail.return_request.refund_poisha !== null ? (
            <>
              {" "}
              · <PriceTag amountPoisha={detail.return_request.refund_poisha} />
            </>
          ) : null}{" "}
          — <Link href="/admin/returns">রিটার্ন সারিতে দেখুন</Link>
        </p>
      ) : null}

      <h3>রিফ্রেশ — Refresh</h3>
      {isManual ? (
        <p className="admin-muted">
          ম্যানুয়াল কুরিয়ার — পল করার কিছু নেই; অবস্থা বিক্রেতা নিজে লিখেন।
          (Manual courier — there is nothing to poll; the seller enters the
          status.)
        </p>
      ) : (
        <p>
          <Button size="sm" loading={refreshBusy} onClick={onRefresh}>
            এখনই পল করুন — Refresh now
          </Button>{" "}
          <span className="admin-muted">
            শেষ পল:{" "}
            {detail.last_polled_at
              ? formatDhakaDateTime(detail.last_polled_at)
              : "এখনো পল হয়নি"}
          </span>
        </p>
      )}
      {refreshMsg ? (
        <p
          role={refreshMsg.kind === "error" ? "alert" : "status"}
          className={
            refreshMsg.kind === "error" ? "payout-warning" : "admin-flash-line"
          }
        >
          {refreshMsg.text}
        </p>
      ) : null}

      {detail.audit_entries.length > 0 ? (
        <>
          <h3>অডিট — Audit</h3>
          <ul>
            {detail.audit_entries.map((entry) => (
              <li key={entry.id}>
                {formatDhakaDateTime(entry.created_at)} · {entry.action} ·{" "}
                {entry.admin_phone_masked}
              </li>
            ))}
          </ul>
        </>
      ) : null}
    </div>
  );
}
