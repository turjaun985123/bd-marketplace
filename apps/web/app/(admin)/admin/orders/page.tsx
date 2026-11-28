"use client";

import {
  Suspense,
  useCallback,
  useEffect,
  useRef,
  useState,
  type ReactNode,
} from "react";
import Link from "next/link";
import { useSearchParams } from "next/navigation";
import AdminShell from "@/components/AdminShell";
import OrderTimeline from "@/components/OrderTimeline";
import PriceTag from "@/components/PriceTag";
import { Badge, Button, EmptyState, Skeleton } from "@/components/ui";
import {
  ApiCallError,
  getAdminInvoice,
  getAdminOrder,
  listAdminOrders,
  orderStatusLabel,
  paymentStatusLabel,
  type AdminOrder,
  type AdminOrderDetail,
  type AdminOrderList,
  type AdminOrderSort,
  type AdminOrderSummary,
  type Invoice,
  type SubOrder,
} from "@/lib/api";
import { formatCount, formatDhakaDateTime } from "@/lib/format";
import { useAuth } from "@/hooks/useAuth";

const PAGE_LIMIT = 25;

const STATUS_FILTERS: { value: string; label: string }[] = [
  { value: "", label: "সব — All" },
  { value: "placed", label: "অর্ডার হয়েছে — Placed" },
  { value: "cancelled", label: "বাতিল — Cancelled" },
];

const PAYMENT_FILTERS: { value: string; label: string }[] = [
  { value: "", label: "সব পেমেন্ট — All payments" },
  { value: "pay_on_delivery", label: "ডেলিভারিতে পেমেন্ট — COD" },
  { value: "advance_pending", label: "অগ্রিম বাকি — Advance pending" },
  { value: "advance_paid", label: "অগ্রিম দেওয়া — Advance paid" },
  { value: "paid", label: "পরিশোধিত — Paid" },
  { value: "pending", label: "পেমেন্ট বাকি — Pending" },
  { value: "failed", label: "ব্যর্থ — Failed" },
  { value: "refunded", label: "ফেরত — Refunded" },
];

const SUB_STATUS_FILTERS: { value: string; label: string }[] = [
  { value: "", label: "সব ধাপ — All stages" },
  { value: "placed", label: "অর্ডার দেওয়া হয়েছে" },
  { value: "confirmed", label: "নিশ্চিত হয়েছে" },
  { value: "shipped", label: "পাঠানো হয়েছে" },
  { value: "delivered", label: "ডেলিভারি হয়েছে" },
  { value: "returned", label: "ফেরত গেছে" },
  { value: "cancelled", label: "বাতিল হয়েছে" },
];

const ADVANCE_FILTERS: { value: string; label: string }[] = [
  { value: "", label: "সব অগ্রিম — All advances" },
  { value: "pending", label: "অগ্রিম বাকি আছে — Awaiting advance" },
  { value: "unfulfilled", label: "অগ্রিম নেওয়া যায়নি — Advance unfulfilled" },
];

const SORT_OPTIONS: { value: AdminOrderSort; label: string }[] = [
  { value: "newest", label: "সবচেয়ে নতুন আগে — Newest first" },
  { value: "oldest", label: "সবচেয়ে পুরনো আগে — Oldest first" },
  { value: "amount_desc", label: "বড় অঙ্ক আগে — Amount: high to low" },
  { value: "amount_asc", label: "ছোট অঙ্ক আগে — Amount: low to high" },
];

const VALID_STATUSES = ["placed", "cancelled"];
const VALID_PAYMENTS = [
  "pending",
  "paid",
  "failed",
  "refunded",
  "pay_on_delivery",
  "advance_pending",
  "advance_paid",
];
const VALID_SUB_STATUSES = [
  "placed",
  "confirmed",
  "shipped",
  "delivered",
  "cancelled",
  "returned",
];
const VALID_ADVANCE = ["pending", "unfulfilled"];
const VALID_SORTS = ["newest", "oldest", "amount_desc", "amount_asc"];

function zoneLabel(zone: string): string {
  switch (zone) {
    case "inside_dhaka":
      return "ঢাকার ভিতরে";
    case "dhaka_suburb":
      return "ঢাকার উপশহর";
    case "outside_dhaka":
      return "ঢাকার বাইরে";
    default:
      return zone;
  }
}

function paymentBadgeTone(
  order: AdminOrder
): "brand" | "accent" | "danger" | "muted" | "flash" {
  if (order.advance_unfulfilled) return "danger";
  switch (order.payment_status) {
    case "paid":
    case "advance_paid":
      return "brand";
    case "advance_pending":
    case "pending":
      return "flash";
    case "failed":
      return "danger";
    default:
      return "muted";
  }
}

/** The whole-queue summary as chips (admin orders remake):
 * the overview's own advance populations and today
 * figures, so the queue header and the overview agree. */
function SummaryChips({ summary }: { summary: AdminOrderSummary }) {
  return (
    <div className="summary-chips">
      <div className="chip">
        <span className="chip-label">মোট অর্ডার — Orders</span>
        <span className="chip-value">{formatCount(summary.total)}</span>
        <span className="chip-sub">
          বাতিল {formatCount(summary.cancelled)}টি
        </span>
      </div>
      <div className="chip">
        <span className="chip-label">আজকের অর্ডার — Today</span>
        <span className="chip-value">
          {formatCount(summary.orders_today)}
        </span>
        <span className="chip-sub">
          GMV <PriceTag amountPoisha={summary.gmv_today_poisha} />
        </span>
      </div>
      <div className="chip chip-warning">
        <span className="chip-label">অগ্রিম বাকি — Advance pending</span>
        <span className="chip-value">
          {formatCount(summary.advance_pending_count)}
        </span>
        <span className="chip-sub">
          বাকি <PriceTag amountPoisha={summary.advance_outstanding_poisha} />
        </span>
      </div>
      <div className="chip chip-danger">
        <span className="chip-label">
          অগ্রিম নেওয়া যায়নি — Unfulfilled
        </span>
        <span className="chip-value">
          {formatCount(summary.advance_unfulfilled_count)}
        </span>
        <span className="chip-sub">
          ঝুঁকি <PriceTag amountPoisha={summary.advance_unfulfilled_poisha} />
        </span>
      </div>
      <div className="chip">
        <span className="chip-label">COD চলমান — On delivery</span>
        <span className="chip-value">
          {formatCount(summary.payment_pay_on_delivery)}
        </span>
        <span className="chip-sub">
          পরিশোধিত {formatCount(summary.payment_paid)}টি
        </span>
      </div>
    </div>
  );
}

function OrderCard({
  order,
  expanded,
  detail,
  detailError,
  detailLoading,
  onToggleDetail,
  onRetryDetail,
  renderDetail,
}: {
  order: AdminOrder;
  expanded: boolean;
  detail: AdminOrderDetail | null;
  detailError: boolean;
  detailLoading: boolean;
  onToggleDetail: () => void;
  onRetryDetail: () => void;
  renderDetail: (detail: AdminOrderDetail) => ReactNode;
}) {
  const outstanding =
    order.advance_required_poisha - order.advance_paid_poisha;
  return (
    <article className="payout-card">
      <div className="payout-card-head">
        <div>
          <div className="order-title-row">
            <strong>#{order.id.slice(0, 8)}</strong>
            <Badge tone={order.status === "cancelled" ? "danger" : "muted"}>
              {orderStatusLabel(order.status)}
            </Badge>
            <Badge tone={paymentBadgeTone(order)}>
              {paymentStatusLabel(order.payment_status)}
            </Badge>
            {order.advance_unfulfilled ? (
              <Badge tone="danger">অগ্রিম নেওয়া যায়নি</Badge>
            ) : null}
            {order.payment_status === "advance_pending" && outstanding > 0 ? (
              <Badge tone="flash">
                অগ্রিম বাকি <PriceTag amountPoisha={outstanding} />
              </Badge>
            ) : null}
          </div>
          <p className="payout-card-meta">
            {order.delivery_name} · {order.delivery_phone_masked} ·{" "}
            {order.delivery_district ?? zoneLabel(order.delivery_zone)}
          </p>
        </div>
        <div className="payout-card-actions">
          <span className="payout-amount">
            <PriceTag amountPoisha={order.grand_total_poisha} />
          </span>
          <Button size="sm" variant="ghost" onClick={onToggleDetail}>
            {expanded ? "বন্ধ করুন — Close" : "বিস্তারিত — Details"}
          </Button>
        </div>
      </div>
      <div className="pipeline-row">
        {order.sub_orders.map((sub) => (
          <span key={sub.id} className="tag-chip">
            {sub.store_name} — {orderStatusLabel(sub.status)} ·{" "}
            <PriceTag amountPoisha={sub.goods_subtotal_poisha} />
          </span>
        ))}
      </div>
      <p className="payout-card-meta">
        {formatDhakaDateTime(order.created_at)} ·{" "}
        {order.payment_method === "cod"
          ? "ক্যাশ অন ডেলিভারি"
          : order.payment_method}
      </p>
      {expanded ? (
        <div className="payout-detail">
          {detailLoading ? (
            <>
              <Skeleton width="100%" height={18} />
              <Skeleton width="100%" height={18} />
              <Skeleton width="100%" height={18} />
            </>
          ) : detailError ? (
            <div className="admin-error">
              <p>বিস্তারিত লোড করা যায়নি — Couldn&apos;t load the detail.</p>
              <Button size="sm" onClick={onRetryDetail}>
                আবার চেষ্টা করুন — Retry
              </Button>
            </div>
          ) : detail ? (
            renderDetail(detail)
          ) : null}
        </div>
      ) : null}
    </article>
  );
}

/** The expanded order detail: the buyer's full view
 * (delivery block, items, timelines) plus the admin
 * extras — masked account phone, COD due, per-store
 * invoices fetched through the ADMIN invoice endpoint. */
function OrderDetailView({
  detail,
  token,
}: {
  detail: AdminOrderDetail;
  token: string | null;
}) {
  const [invoices, setInvoices] = useState<Record<string, Invoice>>({});
  const [invoiceBusy, setInvoiceBusy] = useState<string | null>(null);
  const [invoiceError, setInvoiceError] = useState<string | null>(null);

  const loadInvoice = async (sub: SubOrder) => {
    if (!token) return;
    setInvoiceBusy(sub.id);
    setInvoiceError(null);
    try {
      const invoice = await getAdminInvoice(token, sub.id);
      setInvoices((prev) => ({ ...prev, [sub.id]: invoice }));
    } catch {
      setInvoiceError(sub.id);
    } finally {
      setInvoiceBusy(null);
    }
  };

  return (
    <div className="detail-grid">
      <section>
        <h4>ডেলিভারি — Delivery</h4>
        <p>
          {detail.delivery_name}
          <br />
          {detail.delivery_phone}
          <br />
          {detail.delivery_address}
          <br />
          {zoneLabel(detail.delivery_zone)}
          {detail.delivery_district
            ? ` · ${detail.delivery_district}`
            : ""}
        </p>
        {detail.order_note ? (
          <p className="admin-muted">নোট: {detail.order_note}</p>
        ) : null}
        <h4>ক্রেতা — Buyer</h4>
        <p className="admin-muted">
          অ্যাকাউন্ট ফোন (মাস্ক করা): {detail.buyer_phone_masked}
        </p>
      </section>

      <section>
        <h4>পেমেন্ট — Payment</h4>
        <p>
          {detail.payment_method === "cod"
            ? "ক্যাশ অন ডেলিভারি"
            : detail.payment_method}{" "}
          · {paymentStatusLabel(detail.payment_status)}
        </p>
        {detail.advance_required_poisha > 0 ? (
          <p className="admin-muted">
            অগ্রিম প্রয়োজন{" "}
            <PriceTag amountPoisha={detail.advance_required_poisha} /> · দেওয়া
            হয়েছে <PriceTag amountPoisha={detail.advance_paid_poisha} />
          </p>
        ) : null}
        {detail.cod_amount_due_poisha > 0 ? (
          <p>
            ডেলিভারিতে বাকি — Due on delivery:{" "}
            <PriceTag amountPoisha={detail.cod_amount_due_poisha} />
          </p>
        ) : null}
        {detail.payment ? (
          <p className="admin-muted">
            সর্বশেষ চেষ্টা: {detail.payment.method} · {detail.payment.status}
            {detail.payment.gateway_ref
              ? ` · ${detail.payment.gateway_ref}`
              : ""}
          </p>
        ) : null}
        <h4>টাকার হিসাব — Money</h4>
        <ul className="money-lines">
          <li>
            <span>পণ্য — Goods</span>
            <PriceTag amountPoisha={detail.goods_total_poisha} />
          </li>
          {detail.discount_total_poisha > 0 ? (
            <li>
              <span>
                কুপন{detail.coupon_code ? ` (${detail.coupon_code})` : ""}
              </span>
              <span>
                −<PriceTag amountPoisha={detail.discount_total_poisha} />
              </span>
            </li>
          ) : null}
          {detail.loyalty_discount_poisha > 0 ? (
            <li>
              <span>লয়্যালটি — Loyalty</span>
              <span>
                −<PriceTag amountPoisha={detail.loyalty_discount_poisha} />
              </span>
            </li>
          ) : null}
          {detail.gift_card_discount_poisha > 0 ? (
            <li>
              <span>গিফট কার্ড — Gift card</span>
              <span>
                −<PriceTag amountPoisha={detail.gift_card_discount_poisha} />
              </span>
            </li>
          ) : null}
          <li>
            <span>ডেলিভারি — Delivery</span>
            <PriceTag amountPoisha={detail.delivery_total_poisha} />
          </li>
          <li className="money-total">
            <span>সর্বমোট — Grand total</span>
            <PriceTag amountPoisha={detail.grand_total_poisha} />
          </li>
        </ul>
      </section>

      <section className="detail-wide">
        <h4>স্টোর অনুযায়ী — By store</h4>
        {detail.sub_orders.map((sub) => (
          <div key={sub.id} className="sub-detail">
            <p>
              <Link href={`/${sub.store_slug}`}>{sub.store_slug}</Link>{" "}
              <Badge tone={sub.status === "cancelled" ? "danger" : "muted"}>
                {orderStatusLabel(sub.status)}
              </Badge>{" "}
              <PriceTag amountPoisha={sub.goods_subtotal_poisha} />
              {sub.return_status ? (
                <>
                  {" "}
                  <Badge tone="flash">
                    রিটার্ন: {sub.return_status}
                  </Badge>{" "}
                  <Link href="/admin/returns">রিটার্ন কিউ — Returns</Link>
                </>
              ) : null}
            </p>
            <ul className="item-lines">
              {sub.items.map((item) => (
                <li key={item.id}>
                  {item.title_snapshot}
                  {item.variant_name ? ` — ${item.variant_name}` : ""} ×{" "}
                  {formatCount(item.quantity)} ·{" "}
                  <PriceTag amountPoisha={item.line_total_poisha} />
                </li>
              ))}
            </ul>
            {sub.shipment ? (
              <p className="admin-muted">
                কুরিয়ার: {sub.shipment.courier} ·{" "}
                {sub.shipment.consignment_id} · {sub.shipment.status}
              </p>
            ) : null}
            <OrderTimeline
              status={sub.status}
              shipmentStatus={sub.shipment?.status ?? null}
              events={sub.timeline}
              steps={sub.steps}
            />
            {sub.status === "delivered" || sub.status === "returned" ? (
              invoices[sub.id] ? (
                <p className="admin-muted">
                  ইনভয়েস {invoices[sub.id].invoice_number} · মোট{" "}
                  <PriceTag
                    amountPoisha={invoices[sub.id].money.grand_total_poisha}
                  />{" "}
                  · ডেলিভারিতে বাকি{" "}
                  <PriceTag
                    amountPoisha={invoices[sub.id].money.cod_amount_due_poisha}
                  />
                </p>
              ) : (
                <Button
                  size="sm"
                  variant="ghost"
                  disabled={invoiceBusy === sub.id}
                  onClick={() => void loadInvoice(sub)}
                >
                  {invoiceBusy === sub.id
                    ? "লোড হচ্ছে…"
                    : "ইনভয়েস দেখুন — View invoice"}
                </Button>
              )
            ) : null}
            {invoiceError === sub.id ? (
              <p className="admin-error-text">
                ইনভয়েস লোড করা যায়নি — Couldn&apos;t load the invoice.
              </p>
            ) : null}
          </div>
        ))}
      </section>
    </div>
  );
}

function emptyTitle(
  status: string,
  payment: string,
  advance: string,
  q: string
): string {
  if (q) return "কিছু পাওয়া যায়নি — Nothing matches";
  if (advance === "pending")
    return "অগ্রিম বাকি কোনো অর্ডার নেই — No advances pending";
  if (advance === "unfulfilled")
    return "অগ্রিম অপূর্ণ কোনো অর্ডার নেই — No unfulfilled advances";
  if (payment) return "এই পেমেন্ট অবস্থায় অর্ডার নেই — No orders";
  if (status === "cancelled") return "বাতিল অর্ডার নেই — No cancelled orders";
  return "কোনো অর্ডার নেই — No orders yet";
}

function AdminOrders() {
  const { token, user, ready } = useAuth();
  const searchParams = useSearchParams();
  const [list, setList] = useState<AdminOrderList | null>(null);
  const [loadError, setLoadError] = useState(false);
  const [q, setQ] = useState(() => searchParams.get("q") ?? "");
  const [status, setStatus] = useState(() => {
    const param = searchParams.get("status") ?? "";
    return VALID_STATUSES.includes(param) ? param : "";
  });
  const [payment, setPayment] = useState(() => {
    const param = searchParams.get("payment_status") ?? "";
    return VALID_PAYMENTS.includes(param) ? param : "";
  });
  const [subStatus, setSubStatus] = useState(() => {
    const param = searchParams.get("sub_status") ?? "";
    return VALID_SUB_STATUSES.includes(param) ? param : "";
  });
  const [advance, setAdvance] = useState(() => {
    const param = searchParams.get("advance_state") ?? "";
    return VALID_ADVANCE.includes(param) ? param : "";
  });
  const [storeId] = useState(() => searchParams.get("store_id") ?? "");
  const [sort, setSort] = useState<AdminOrderSort>(() => {
    const param = searchParams.get("sort") ?? "";
    return VALID_SORTS.includes(param)
      ? (param as AdminOrderSort)
      : "newest";
  });
  const [page, setPage] = useState(1);
  const [expandedId, setExpandedId] = useState<string | null>(null);
  const [detail, setDetail] = useState<AdminOrderDetail | null>(null);
  const [loadingDetailId, setLoadingDetailId] = useState<string | null>(
    null
  );
  const [detailError, setDetailError] = useState(false);
  const debounceRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const [debouncedQ, setDebouncedQ] = useState(q);

  useEffect(() => {
    if (debounceRef.current) clearTimeout(debounceRef.current);
    debounceRef.current = setTimeout(() => {
      setDebouncedQ(q);
      setPage(1);
    }, 300);
    return () => {
      if (debounceRef.current) clearTimeout(debounceRef.current);
    };
  }, [q]);

  const load = useCallback(async () => {
    if (!token) return;
    try {
      const result = await listAdminOrders(token, {
        q: debouncedQ.trim() || undefined,
        status: status || undefined,
        payment_status: payment || undefined,
        sub_status: subStatus || undefined,
        store_id: storeId || undefined,
        advance_state: advance || undefined,
        sort,
        page,
        limit: PAGE_LIMIT,
      });
      setList(result);
      setLoadError(false);
    } catch (err) {
      if (err instanceof ApiCallError && err.status === 401) {
        window.location.href = "/login";
        return;
      }
      setLoadError(true);
    }
  }, [token, debouncedQ, status, payment, subStatus, storeId, advance, sort, page]);

  useEffect(() => {
    void load();
  }, [load]);

  const reload = useCallback(() => {
    setList(null);
    setLoadError(false);
    void load();
  }, [load]);

  const loadDetail = useCallback(
    async (orderId: string) => {
      if (!token) return;
      setLoadingDetailId(orderId);
      setDetailError(false);
      try {
        setDetail(await getAdminOrder(token, orderId));
      } catch {
        setDetail(null);
        setDetailError(true);
      } finally {
        setLoadingDetailId(null);
      }
    },
    [token]
  );

  const toggleDetail = (order: AdminOrder) => {
    if (expandedId === order.id) {
      setExpandedId(null);
      setDetail(null);
      return;
    }
    setExpandedId(order.id);
    setDetail(null);
    void loadDetail(order.id);
  };

  if (!ready) return <p>লোড হচ্ছে…</p>;
  if (!user) {
    return (
      <p>
        অর্ডার দেখতে প্রথমে <Link href="/login">লগ ইন</Link> করুন।
      </p>
    );
  }
  if (!user.is_admin) {
    return (
      <p>
        এই পেজটি শুধু অ্যাডমিনদের জন্য। (This page is for platform admins
        only.)
      </p>
    );
  }

  const totalPages = list ? Math.max(1, Math.ceil(list.total / PAGE_LIMIT)) : 1;

  return (
    <>
      <div className="admin-overview-head">
        <div>
          <h1>অর্ডার — Orders</h1>
        <p className="admin-muted">
          সব স্টোরের অর্ডার এক জায়গায়: পেমেন্ট ও অগ্রিমের অবস্থা,
          স্টোরভিত্তিক ধাপ আর টাকার হিসাব। অর্ডারের ধাপ বদলানোর ক্ষমতা
          এখানে নেই — তা ক্রেতা ও বিক্রেতার ফ্লোতেই থাকে। (Every order
          across stores: payment and advance state, per-store progress
          and the money. This surface observes — order transitions stay
          in the buyer and seller flows.)
          </p>
        </div>
      </div>

      {list ? <SummaryChips summary={list.summary} /> : null}

      <div className="payout-controls">
        <input
          type="search"
          value={q}
          onChange={(e) => setQ(e.target.value)}
          placeholder="ফোন, অর্ডার আইডি, নাম বা স্টোর — Phone, order id, name or store"
          aria-label="অর্ডার খুঁজুন"
        />
        <select
          value={sort}
          onChange={(e) => {
            setSort(e.target.value as AdminOrderSort);
            setPage(1);
          }}
          aria-label="সাজান — Sort"
        >
          {SORT_OPTIONS.map((option) => (
            <option key={option.value} value={option.value}>
              {option.label}
            </option>
          ))}
        </select>
      </div>

      <div className="chip-row" role="group" aria-label="অবস্থা — Status">
        {STATUS_FILTERS.map((option) => (
          <Button
            key={option.value || "all"}
            size="sm"
            variant={status === option.value ? "primary" : "ghost"}
            onClick={() => {
              setStatus(option.value);
              setPage(1);
            }}
          >
            {option.label}
          </Button>
        ))}
      </div>

      <div className="chip-row" role="group" aria-label="পেমেন্ট — Payment">
        {PAYMENT_FILTERS.map((option) => (
          <Button
            key={option.value || "all"}
            size="sm"
            variant={payment === option.value ? "primary" : "ghost"}
            onClick={() => {
              setPayment(option.value);
              setPage(1);
            }}
          >
            {option.label}
          </Button>
        ))}
      </div>

      <div className="chip-row" role="group" aria-label="অগ্রিম — Advance">
        {ADVANCE_FILTERS.map((option) => (
          <Button
            key={option.value || "all"}
            size="sm"
            variant={advance === option.value ? "primary" : "ghost"}
            onClick={() => {
              setAdvance(option.value);
              setPage(1);
            }}
          >
            {option.label}
          </Button>
        ))}
        {SUB_STATUS_FILTERS.map((option) => (
          <Button
            key={`sub-${option.value || "all"}`}
            size="sm"
            variant={subStatus === option.value ? "primary" : "ghost"}
            onClick={() => {
              setSubStatus(option.value);
              setPage(1);
            }}
          >
            ধাপ: {option.label}
          </Button>
        ))}
      </div>

      {!list && !loadError ? (
        <div className="admin-skel" aria-hidden="true">
          {Array.from({ length: 4 }, (_, i) => (
            <Skeleton key={i} width="100%" height={72} />
          ))}
        </div>
      ) : null}

      {loadError ? (
        <div className="admin-error">
          <p>অর্ডার লোড করা যায়নি — Couldn&apos;t load orders.</p>
          <Button size="sm" onClick={reload}>
            আবার চেষ্টা করুন — Retry
          </Button>
        </div>
      ) : null}

      {list && list.items.length === 0 ? (
        <EmptyState title={emptyTitle(status, payment, advance, debouncedQ)}>
          <p>
            ফিল্টার বদলে আবার দেখুন। (Try a different filter — the summary
            above always shows the whole platform.)
          </p>
        </EmptyState>
      ) : null}

      {list ? (
        <div className="queue-list">
          {list.items.map((order) => (
            <OrderCard
              key={order.id}
              order={order}
              expanded={expandedId === order.id}
              detail={expandedId === order.id ? detail : null}
              detailError={expandedId === order.id && detailError}
              detailLoading={loadingDetailId === order.id}
              onToggleDetail={() => toggleDetail(order)}
              onRetryDetail={() => void loadDetail(order.id)}
              renderDetail={(d) => (
                <OrderDetailView detail={d} token={token} />
              )}
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

export default function AdminOrdersPage() {
  return (
    <AdminShell>
      <Suspense fallback={<p>লোড হচ্ছে…</p>}>
        <AdminOrders />
      </Suspense>
    </AdminShell>
  );
}
