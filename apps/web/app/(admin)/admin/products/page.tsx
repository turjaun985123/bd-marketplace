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
import { Badge, Button, Dialog, EmptyState, Skeleton } from "@/components/ui";
import {
  ApiCallError,
  getAdminProductDetail,
  listAdminProducts,
  productSourceLabel,
  productStatusLabel,
  restoreAdminProduct,
  storeStatusLabel,
  takedownAdminProduct,
  type AdminProduct,
  type AdminProductDetail,
  type AdminProductList,
  type AdminProductSort,
} from "@/lib/api";
import { formatBdt, formatCount, formatDhakaDateTime } from "@/lib/format";
import { useAuth } from "@/hooks/useAuth";

const PAGE_LIMIT = 25;
const MAX_NOTE_CHARS = 500;

const STATUS_FILTERS: { value: string; label: string }[] = [
  { value: "", label: "সব — All" },
  { value: "active", label: "সক্রিয় — Active" },
  { value: "draft", label: "খসড়া — Draft" },
  { value: "archived", label: "আর্কাইভ — Archived" },
];

const SOURCE_FILTERS: { value: string; label: string }[] = [
  { value: "", label: "সব উৎস — All sources" },
  { value: "manual", label: "ম্যানুয়াল — Manual" },
  { value: "facebook", label: "ফেসবুক — Facebook" },
  { value: "bulk", label: "বাল্ক — Bulk" },
];

const VISIBILITY_FILTERS: { value: string; label: string }[] = [
  { value: "", label: "সব — All" },
  { value: "visible", label: "দৃশ্যমান — Visible" },
  { value: "taken_down", label: "সরানো হয়েছে — Taken down" },
];

const STOCK_FILTERS: { value: string; label: string }[] = [
  { value: "", label: "সব স্টক — All stock" },
  { value: "in_stock", label: "স্টক আছে — In stock" },
  { value: "low", label: "স্টক কম — Low" },
  { value: "out_of_stock", label: "স্টক শেষ — Out of stock" },
];

const IMAGE_FILTERS: { value: string; label: string }[] = [
  { value: "", label: "ছবি: সব — Images: all" },
  { value: "true", label: "ছবি আছে — Has images" },
  { value: "false", label: "ছবি নেই — No images" },
];

const SORT_OPTIONS: { value: AdminProductSort; label: string }[] = [
  { value: "newest", label: "সবচেয়ে নতুন আগে — Newest first" },
  { value: "oldest", label: "সবচেয়ে পুরনো আগে — Oldest first" },
  { value: "price_desc", label: "দাম বেশি আগে — Price: high to low" },
  { value: "price_asc", label: "দাম কম আগে — Price: low to high" },
  { value: "sold_desc", label: "বেশি বিক্রি আগে — Most sold" },
  { value: "rating_desc", label: "রেটিং বেশি আগে — Highest rated" },
];

const VALID_STATUSES = ["draft", "active", "archived"];
const VALID_VISIBILITIES = ["visible", "taken_down"];

function statusTone(status: string): "brand" | "muted" {
  return status === "active" ? "brand" : "muted";
}

function stockTone(stockStatus: string): "brand" | "accent" | "danger" {
  switch (stockStatus) {
    case "out_of_stock":
      return "danger";
    case "low":
      return "accent";
    default:
      return "brand";
  }
}

function stockLabel(stockStatus: string): string {
  switch (stockStatus) {
    case "out_of_stock":
      return "স্টক শেষ";
    case "low":
      return "স্টক কম";
    default:
      return "স্টক আছে";
  }
}

/**
 * Admin products — the cross-store catalogue queue, built
 * by the admin remake program (sixth surface). Before this
 * surface an admin could not see the catalogue across
 * stores at all, and the only lever on a bad listing was
 * suspending the seller's WHOLE store.
 *
 * - The queue is inspection-based: there is NO product
 *   report/flag signal anywhere in the platform (no report
 *   table, no endpoint), so the queue is built for search
 *   and triage — status / source / visibility / stock /
 *   image filters, sales and rating sorts — not around a
 *   fabricated reports inbox.
 * - The one power is takedown / restore: a platform FLAG
 *   on the product, orthogonal to the seller's own status.
 *   Takedown REQUIRES a reason and restore REQUIRES a
 *   note; both are written into the audit detail, the
 *   reason is stored on the product where the seller sees
 *   it, and the seller is notified in the same transaction
 *   the API writes the change. A no-op transition is a 409
 *   — this page freezes the row and reloads.
 * - Nothing is ever deleted here. A running promotion is
 *   NOT paused or refunded by a takedown (its placement
 *   simply stops rendering), and product images keep
 *   serving by URL — removing an image itself is the media
 *   moderation surface's job, linked from the detail.
 */
export default function AdminProductsPage() {
  return (
    <AdminShell>
      <Suspense fallback={<p>লোড হচ্ছে…</p>}>
        <Products />
      </Suspense>
    </AdminShell>
  );
}

type DialogState =
  | { kind: "takedown"; product: AdminProduct }
  | { kind: "restore"; product: AdminProduct }
  | null;

function Products() {
  const { token } = useAuth();
  const searchParams = useSearchParams();
  const initialStatus = (() => {
    const param = searchParams.get("status") ?? "";
    return VALID_STATUSES.includes(param) ? param : "";
  })();
  const initialVisibility = (() => {
    const param = searchParams.get("visibility") ?? "";
    return VALID_VISIBILITIES.includes(param) ? param : "";
  })();
  const storeId = searchParams.get("store_id") ?? "";

  const [status, setStatus] = useState(initialStatus);
  const [visibility, setVisibility] = useState(initialVisibility);
  const [source, setSource] = useState("");
  const [stockState, setStockState] = useState("");
  const [hasImages, setHasImages] = useState("");
  const [sort, setSort] = useState<AdminProductSort>("newest");
  const [qInput, setQInput] = useState("");
  const [q, setQ] = useState("");
  const [page, setPage] = useState(1);

  const [list, setList] = useState<AdminProductList | null>(null);
  const [loadError, setLoadError] = useState(false);
  const [stale, setStale] = useState(false);
  const [asOf, setAsOf] = useState<string | null>(null);
  const [tick, setTick] = useState(0);

  const [notice, setNotice] = useState<string | null>(null);
  const [pageError, setPageError] = useState<string | null>(null);
  const [conflictedIds, setConflictedIds] = useState<ReadonlySet<string>>(
    new Set()
  );

  const [expandedId, setExpandedId] = useState<string | null>(null);
  const [detail, setDetail] = useState<AdminProductDetail | null>(null);
  const [detailError, setDetailError] = useState(false);
  const [loadingDetailId, setLoadingDetailId] = useState<string | null>(null);

  const [dialog, setDialog] = useState<DialogState>(null);
  const [dialogNote, setDialogNote] = useState("");
  const [dialogError, setDialogError] = useState<string | null>(null);
  const [formBusy, setFormBusy] = useState(false);

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
      const next = await listAdminProducts(token, {
        q: q || undefined,
        status: status || undefined,
        store_id: storeId || undefined,
        source: source || undefined,
        visibility: visibility || undefined,
        stock_state: stockState || undefined,
        has_images: hasImages || undefined,
        sort,
        page,
        limit: PAGE_LIMIT,
      });
      hasDataRef.current = true;
      setList(next);
      setLoadError(false);
      setStale(false);
      setConflictedIds(new Set());
      setAsOf(new Date().toISOString());
    } catch {
      if (hasDataRef.current) setStale(true);
      else setLoadError(true);
    }
  }, [token, status, storeId, source, visibility, stockState, hasImages, q, sort, page, tick]);

  useEffect(() => {
    void load();
  }, [load]);

  const reload = useCallback(() => setTick((value) => value + 1), []);

  function changeStatus(value: string) {
    setStatus(value);
    setPage(1);
  }

  function changeVisibility(value: string) {
    setVisibility(value);
    setPage(1);
  }

  function changeSource(value: string) {
    setSource(value);
    setPage(1);
  }

  function changeStockState(value: string) {
    setStockState(value);
    setPage(1);
  }

  function changeHasImages(value: string) {
    setHasImages(value);
    setPage(1);
  }

  function changeSort(value: AdminProductSort) {
    setSort(value);
    setPage(1);
  }

  async function loadDetail(id: string) {
    if (!token) return;
    setLoadingDetailId(id);
    setDetailError(false);
    try {
      const next = await getAdminProductDetail(token, id);
      setDetail(next);
    } catch {
      setDetailError(true);
    } finally {
      setLoadingDetailId(null);
    }
  }

  async function toggleDetail(product: AdminProduct) {
    if (expandedId === product.id) {
      setExpandedId(null);
      setDetail(null);
      return;
    }
    setExpandedId(product.id);
    setDetail(null);
    await loadDetail(product.id);
  }

  async function refetchDetail(id: string) {
    if (!token || expandedId !== id) return;
    try {
      setDetail(await getAdminProductDetail(token, id));
    } catch {
      setDetailError(true);
    }
  }

  function openDialog(kind: "takedown" | "restore", product: AdminProduct) {
    setDialog({ kind, product });
    setDialogNote("");
    setDialogError(null);
  }

  function closeDialog() {
    if (formBusy) return;
    setDialog(null);
    setDialogError(null);
  }

  async function confirmDialog() {
    if (!token || !dialog) return;
    const { kind, product } = dialog;
    // Client-side mirror of the API's requirement: the
    // API's 400 is the authority, this only saves a trip.
    if (dialogNote.trim() === "") {
      setDialogError(
        kind === "takedown"
          ? "অপসারণের কারণ লিখুন — a takedown reason is required."
          : "ফেরত দেওয়ার নোট লিখুন — a restore note is required."
      );
      return;
    }
    setFormBusy(true);
    setDialogError(null);
    try {
      if (kind === "takedown") {
        await takedownAdminProduct(token, product.id, dialogNote.trim());
      } else {
        await restoreAdminProduct(token, product.id, dialogNote.trim());
      }
      setDialog(null);
      setNotice(
        kind === "takedown"
          ? "পণ্যটি সরিয়ে নেওয়া হয়েছে — কারণসহ বিক্রেতাকে জানানো হয়েছে। (Product taken down — the seller has been notified with the reason.)"
          : "পণ্যটি ফেরত দেওয়া হয়েছে — বিক্রেতাকে জানানো হয়েছে। (Product restored — the seller has been notified.)"
      );
      setPageError(null);
      await load();
      await refetchDetail(product.id);
    } catch (err) {
      if (err instanceof ApiCallError && err.status === 409) {
        // No-op transition: the product already holds the
        // target state (someone else moved it, or this row
        // went stale). Freeze the row, refetch, and keep
        // the server's message visible.
        setDialog(null);
        setConflictedIds((prev) => new Set(prev).add(product.id));
        setPageError(err.message);
        await load();
      } else {
        setDialogError(
          err instanceof Error ? err.message : "সিদ্ধান্ত কার্যকর করা যায়নি।"
        );
      }
    } finally {
      setFormBusy(false);
    }
  }

  const totalPages = list ? Math.max(1, Math.ceil(list.total / PAGE_LIMIT)) : 1;

  return (
    <>
      <div className="admin-overview-head">
        <div>
          <h2>পণ্য — Products</h2>
          <p className="admin-muted">
            সর্বশেষ হালনাগাদ: {asOf ? formatDhakaDateTime(asOf) : "—"} (ঢাকা সময়)
          </p>
        </div>
        <Button onClick={reload}>↻ হালনাগাদ করুন — Refresh</Button>
      </div>

      {list ? (
        <SummaryStrip
          summary={list.summary}
          activeStatus={status}
          onPickStatus={changeStatus}
          visibility={visibility}
          onPickVisibility={changeVisibility}
        />
      ) : null}

      <div className="payout-controls">
        <label>
          স্ট্যাটাস — Status
          <select value={status} onChange={(e) => changeStatus(e.target.value)}>
            {STATUS_FILTERS.map((f) => (
              <option key={f.value} value={f.value}>
                {f.label}
              </option>
            ))}
          </select>
        </label>
        <label>
          উৎস — Source
          <select value={source} onChange={(e) => changeSource(e.target.value)}>
            {SOURCE_FILTERS.map((f) => (
              <option key={f.value} value={f.value}>
                {f.label}
              </option>
            ))}
          </select>
        </label>
        <label>
          দৃশ্যমানতা — Visibility
          <select
            value={visibility}
            onChange={(e) => changeVisibility(e.target.value)}
          >
            {VISIBILITY_FILTERS.map((f) => (
              <option key={f.value} value={f.value}>
                {f.label}
              </option>
            ))}
          </select>
        </label>
        <label>
          স্টক — Stock
          <select
            value={stockState}
            onChange={(e) => changeStockState(e.target.value)}
          >
            {STOCK_FILTERS.map((f) => (
              <option key={f.value} value={f.value}>
                {f.label}
              </option>
            ))}
          </select>
        </label>
        <label>
          ছবি — Images
          <select
            value={hasImages}
            onChange={(e) => changeHasImages(e.target.value)}
          >
            {IMAGE_FILTERS.map((f) => (
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
            onChange={(e) => changeSort(e.target.value as AdminProductSort)}
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
            placeholder="পণ্য বা স্টোরের নাম"
          />
        </label>
      </div>

      {storeId ? (
        <p className="admin-flash-line" role="status">
          একটি স্টোরের পণ্য দেখানো হচ্ছে —{" "}
          <Link href="/admin/products">সব স্টোরের পণ্য দেখুন</Link>
        </p>
      ) : null}

      {notice ? (
        <p role="status" className="admin-flash-line">
          {notice}
        </p>
      ) : null}
      {pageError ? (
        <p role="alert" className="payout-warning">
          {pageError}
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
          <p>পণ্য লোড করা যায়নি — Couldn&apos;t load products.</p>
          <Button size="sm" onClick={reload}>
            আবার চেষ্টা করুন — Retry
          </Button>
        </div>
      ) : null}

      {list && list.items.length === 0 ? (
        <EmptyState title={emptyTitle(status, visibility, q)}>
          <p>{emptyBody(status, visibility, q)}</p>
        </EmptyState>
      ) : null}

      {list ? (
        <div className="queue-list">
          {list.items.map((product) => (
            <ProductCard
              key={product.id}
              product={product}
              conflicted={conflictedIds.has(product.id)}
              expanded={expandedId === product.id}
              detail={expandedId === product.id ? detail : null}
              detailError={expandedId === product.id && detailError}
              detailLoading={loadingDetailId === product.id}
              onToggleDetail={() => void toggleDetail(product)}
              onRetryDetail={() => void loadDetail(product.id)}
              onAction={(kind) => openDialog(kind, product)}
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

      <Dialog
        open={dialog !== null}
        onClose={closeDialog}
        title={
          dialog?.kind === "takedown"
            ? "পণ্য সরিয়ে নিন — Take product down"
            : "পণ্য ফেরত দিন — Restore product"
        }
        variant={dialog?.kind === "takedown" ? "danger" : "default"}
        onConfirm={() => void confirmDialog()}
        confirmLabel={
          dialog?.kind === "takedown"
            ? "সরিয়ে নিন — Take down"
            : "ফেরত দিন — Restore"
        }
        confirmDisabled={dialogNote.trim() === ""}
        busy={formBusy}
        error={dialogError}
      >
        {dialog ? (
          <ProductDialogBody
            kind={dialog.kind}
            product={dialog.product}
            note={dialogNote}
            onNoteChange={setDialogNote}
          />
        ) : null}
      </Dialog>
    </>
  );
}

function emptyTitle(status: string, visibility: string, q: string): string {
  if (q) return "খোঁজে কোনো পণ্য মেলেনি — No products match the search";
  if (visibility === "taken_down")
    return "কোনো পণ্য সরানো হয়নি — No products are taken down";
  if (status === "draft") return "কোনো খসড়া পণ্য নেই — No draft products";
  if (status === "archived")
    return "কোনো আর্কাইভ পণ্য নেই — No archived products";
  return "কোনো পণ্য নেই — No products yet";
}

function emptyBody(status: string, visibility: string, q: string): string {
  if (q)
    return "পণ্য বা স্টোরের নামের অন্য অংশ দিয়ে খুঁজে দেখুন। (Try another part of the product or store name.)";
  if (visibility === "taken_down")
    return "প্ল্যাটফর্ম এখন পর্যন্ত কোনো পণ্য সরায়নি। (The platform has not taken any product down.)";
  if (status)
    return "ফিল্টার বদলে আবার দেখুন। (Change the filter to see more.)";
  return "প্রথম পণ্য যোগ হলেই এখানে দেখা যাবে। (Products appear here once created.)";
}

// ---------------------------------------------------------------------------
// Summary strip: the whole platform by status + the worklist counts
// ---------------------------------------------------------------------------

function SummaryStrip({
  summary,
  activeStatus,
  onPickStatus,
  visibility,
  onPickVisibility,
}: {
  summary: AdminProductList["summary"];
  activeStatus: string;
  onPickStatus: (status: string) => void;
  visibility: string;
  onPickVisibility: (visibility: string) => void;
}) {
  const chips: { status: string; label: string; count: number }[] = [
    { status: "", label: "সব পণ্য", count: summary.total },
    { status: "active", label: productStatusLabel("active"), count: summary.active },
    { status: "draft", label: productStatusLabel("draft"), count: summary.draft },
    {
      status: "archived",
      label: productStatusLabel("archived"),
      count: summary.archived,
    },
  ];
  return (
    <div className="pipeline-row" aria-label="পণ্য সারাংশ — Product summary">
      {chips.map((chip) => (
        <span key={chip.status || "all"} className="pipeline-chip">
          <Button
            size="sm"
            variant={activeStatus === chip.status ? "primary" : "default"}
            onClick={() => onPickStatus(chip.status)}
          >
            {chip.label}: {formatCount(chip.count)}টি
          </Button>
        </span>
      ))}
      <span className="pipeline-chip">
        <Button
          size="sm"
          variant={visibility === "taken_down" ? "primary" : "default"}
          onClick={() =>
            onPickVisibility(visibility === "taken_down" ? "" : "taken_down")
          }
        >
          সরানো হয়েছে: {formatCount(summary.taken_down)}টি
        </Button>
      </span>
      <span className="pipeline-chip">
        <Badge tone="muted">
          স্টক শেষ: {formatCount(summary.out_of_stock)}টি · ছবি নেই:{" "}
          {formatCount(summary.no_image)}টি
        </Badge>
      </span>
    </div>
  );
}

// ---------------------------------------------------------------------------
// One queue card
// ---------------------------------------------------------------------------

function ProductCard({
  product,
  conflicted,
  expanded,
  detail,
  detailError,
  detailLoading,
  onToggleDetail,
  onRetryDetail,
  onAction,
}: {
  product: AdminProduct;
  conflicted: boolean;
  expanded: boolean;
  detail: AdminProductDetail | null;
  detailError: boolean;
  detailLoading: boolean;
  onToggleDetail: () => void;
  onRetryDetail: () => void;
  onAction: (kind: "takedown" | "restore") => void;
}) {
  const takenDown = product.admin_hidden_at !== null;
  return (
    <article className="payout-card">
      <div className="payout-card-head">
        <div>
          <p>
            {product.image_url ? (
              <>
                {/* eslint-disable-next-line @next/next/no-img-element */}
                <img
                  src={product.image_url}
                  alt=""
                  style={{
                    height: 44,
                    width: 44,
                    objectFit: "cover",
                    borderRadius: 6,
                    verticalAlign: "middle",
                    marginRight: 8,
                  }}
                />
              </>
            ) : null}
            <strong>{product.title}</strong>{" "}
            {product.title_bn ? (
              <span className="admin-muted">({product.title_bn})</span>
            ) : null}
          </p>
          <p className="payout-card-meta">
            স্টোর: {product.store_name} ({product.store_slug})
            {product.store_status === "suspended"
              ? " · স্টোর স্থগিত আছে"
              : ""}{" "}
            · তৈরি: {formatDhakaDateTime(product.created_at)}
          </p>
        </div>
        <div className="payout-card-actions">
          {takenDown ? (
            <Badge tone="danger">সরানো হয়েছে — Taken down</Badge>
          ) : null}
          <Badge tone={statusTone(product.status)}>
            {productStatusLabel(product.status)}
          </Badge>
          <Badge tone="muted">{productSourceLabel(product.source)}</Badge>
          {product.is_promoted ? <Badge tone="accent">স্পন্সরড</Badge> : null}
        </div>
      </div>

      {takenDown ? (
        <p className="payout-card-meta">
          অপসারণের কারণ: {product.admin_hidden_reason ?? "—"}
          {product.admin_hidden_at
            ? ` · ${formatDhakaDateTime(product.admin_hidden_at)} থেকে`
            : ""}
        </p>
      ) : null}

      <p className="payout-card-meta">
        দাম <PriceTag amountPoisha={product.price_poisha} />
        {product.flash_price_poisha !== null ? (
          <>
            {" "}
            · ফ্ল্যাশ দাম <PriceTag amountPoisha={product.flash_price_poisha} />
          </>
        ) : null}{" "}
        · স্টক {formatCount(product.stock)}টি ({stockLabel(product.stock_status)})
        · বিক্রি {formatCount(product.sold_count)}টি · রেটিং{" "}
        {product.rating_avg !== null ? product.rating_avg.toFixed(1) : "—"} (
        {formatCount(product.rating_count)}টি রিভিউ)
      </p>

      <div className="payout-card-actions">
        <Button size="sm" onClick={onToggleDetail}>
          {expanded ? "বিস্তারিত লুকান — Hide detail" : "বিস্তারিত — Detail"}
        </Button>
        {takenDown ? (
          <Button
            size="sm"
            disabled={conflicted}
            onClick={() => onAction("restore")}
          >
            ফেরত দিন — Restore
          </Button>
        ) : (
          <Button
            size="sm"
            variant="danger"
            disabled={conflicted}
            onClick={() => onAction("takedown")}
          >
            সরিয়ে নিন — Take down
          </Button>
        )}
      </div>

      {expanded ? (
        <div className="payout-detail">
          {detailLoading ? (
            <>
              <Skeleton width="100%" height={18} />
              <Skeleton width="100%" height={18} />
              <Skeleton width="100%" height={18} />
            </>
          ) : detailError ? (
            <div>
              <p role="alert" className="admin-error">
                বিস্তারিত লোড করা যায়নি — Couldn&apos;t load the detail.
              </p>
              <Button size="sm" onClick={onRetryDetail}>
                আবার চেষ্টা করুন — Retry
              </Button>
            </div>
          ) : detail ? (
            <DetailBody detail={detail} />
          ) : null}
        </div>
      ) : null}
    </article>
  );
}

function DetailBody({ detail }: { detail: AdminProductDetail }) {
  return (
    <div>
      <h4>ছবি — Images ({formatCount(detail.images.length)}টি)</h4>
      {detail.images.length === 0 ? (
        <p className="payout-card-meta">এই পণ্যের কোনো ছবি নেই।</p>
      ) : (
        <p>
          {detail.images.map((image) => (
            <span key={image.id} style={{ marginRight: 6 }}>
              {/* eslint-disable-next-line @next/next/no-img-element */}
              <img
                src={image.url_card}
                alt={image.alt_text ?? ""}
                style={{
                  height: 56,
                  width: 56,
                  objectFit: "cover",
                  borderRadius: 6,
                  verticalAlign: "middle",
                }}
              />
            </span>
          ))}
        </p>
      )}
      <p className="payout-card-meta">
        ছবিটাই সমস্যা হলে (নকল/অশালীন ছবি) ছবি-ভিত্তিক ব্যবস্থা নিন:{" "}
        <Link href="/admin/media">মিডিয়া মডারেশন — Media moderation</Link>।
        পণ্য সরালেও ছবি URL দিয়ে খোলা যায় — ছবি মোছা সেখানকার কাজ।
      </p>

      <h4>ভেরিয়েন্ট — Variants ({formatCount(detail.variants.length)}টি)</h4>
      {detail.variants.length === 0 ? (
        <p className="payout-card-meta">এই পণ্যের কোনো ভেরিয়েন্ট নেই।</p>
      ) : (
        <ul className="admin-plain-list">
          {detail.variants.map((variant) => (
            <li key={variant.id}>
              {variant.name}
              {variant.sku ? ` (SKU: ${variant.sku})` : ""} ·{" "}
              {variant.is_active ? "সক্রিয়" : "নিষ্ক্রিয়"} · স্টক{" "}
              {formatCount(variant.stock)}টি · দাম{" "}
              {variant.final_price_poisha !== null ? (
                <PriceTag amountPoisha={variant.final_price_poisha} />
              ) : (
                "— (ভিত্তি দামে হিসাব হয় না)"
              )}
            </li>
          ))}
        </ul>
      )}

      <h4>বিক্রি — Sales</h4>
      <ul className="admin-plain-list">
        <li>
          অর্ডারকৃত পণ্যমূল্য (বাতিল নয় এমন সাব-অর্ডারে):{" "}
          {formatBdt(detail.sales.goods_poisha)} · পরিমাণ{" "}
          {formatCount(detail.sales.units_ordered)}টি
        </li>
        <li>
          ডেলিভার্ড বিক্রি: {formatCount(detail.sold_count)}টি — এটিই
          প্ল্যাটফর্মের &quot;বিক্রি&quot; সংজ্ঞা।
        </li>
        <li>
          দ্রষ্টব্য: পণ্যমূল্য মানে বিক্রেতার আয় নয় — কমিশন, ছাড় ও গিফট
          কার্ডের পর আয় বদলায়; আয়ের সত্য বিক্রেতার ব্যালেন্সে।
        </li>
      </ul>

      <h4>স্টোর — Store</h4>
      <ul className="admin-plain-list">
        <li>
          {detail.store.name} ({detail.store.slug}) ·{" "}
          {storeStatusLabel(detail.store.status)} · টায়ার:{" "}
          {detail.store.verified_tier}
        </li>
        <li>
          <Link href={`/admin/products?store_id=${detail.store.id}`}>
            এই স্টোরের সব পণ্য দেখুন — All products of this store
          </Link>{" "}
          · <Link href="/admin/stores">স্টোরের তালিকা — Store queue</Link>
        </li>
      </ul>

      {detail.admin_hidden_at ? (
        <>
          <h4>অপসারণ — Takedown</h4>
          <ul className="admin-plain-list">
            <li>কারণ: {detail.admin_hidden_reason ?? "—"}</li>
            <li>সময়: {formatDhakaDateTime(detail.admin_hidden_at)}</li>
          </ul>
        </>
      ) : null}

      <h4>অডিট ইতিহাস — Audit history</h4>
      {detail.audit_history.length === 0 ? (
        <p className="payout-card-meta">কোনো ইতিহাস পাওয়া যায়নি।</p>
      ) : (
        <ul className="admin-plain-list">
          {detail.audit_history.map((entry) => {
            const reason =
              typeof entry.detail?.reason === "string"
                ? entry.detail.reason
                : null;
            const note =
              typeof entry.detail?.note === "string" ? entry.detail.note : null;
            const previousReason =
              typeof entry.detail?.previous_reason === "string"
                ? entry.detail.previous_reason
                : null;
            return (
              <li key={entry.id}>
                {entry.action} · {entry.admin_phone_masked} ·{" "}
                {formatDhakaDateTime(entry.created_at)}
                {reason ? ` · কারণ: ${reason}` : ""}
                {note ? ` · নোট: ${note}` : ""}
                {previousReason ? ` · আগের কারণ: ${previousReason}` : ""}
              </li>
            );
          })}
        </ul>
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Dialog bodies: honest consequences, stated before the click
// ---------------------------------------------------------------------------

function ProductDialogBody({
  kind,
  product,
  note,
  onNoteChange,
}: {
  kind: "takedown" | "restore";
  product: AdminProduct;
  note: string;
  onNoteChange: (value: string) => void;
}) {
  if (kind === "restore") {
    return (
      <div>
        <p>
          <strong>{product.title}</strong> ({product.store_name}) আবার
          দৃশ্যমান হবে।
        </p>
        <ul className="admin-plain-list">
          <li>
            পণ্যটি বিক্রেতার নিজের স্ট্যাটাসেই ফিরবে — সক্রিয় ছিল তো সক্রিয়,
            আর্কাইভ ছিল তো আর্কাইভ। ফেরত দেওয়া মানে প্রকাশ করা নয়।
          </li>
          <li>
            স্টোর স্থগিত থাকলে পণ্যটি তখনো লুকানোই থাকবে — স্টোরের স্থগিত
            আলাদা সিদ্ধান্ত।
          </li>
          <li>বিক্রেতাকে ফেরত দেওয়ার খবর জানানো হবে।</li>
          <li>
            নোটটি বিক্রেতা দেখবেন না — এটি অডিট লগে আগের অপসারণের কারণ ও
            অপসারণকারীর পাশে থাকবে, যাতে সিদ্ধান্ত বদলের কারণ সবসময় জানা
            যায়।
          </li>
        </ul>
        <label>
          ফেরত দেওয়ার নোট — Restore note (অডিটের জন্য আবশ্যক)
          <textarea
            value={note}
            onChange={(e) => onNoteChange(e.target.value)}
            maxLength={MAX_NOTE_CHARS}
            rows={3}
            placeholder="যেমন: বিক্রেতা আসল পণ্যের প্রমাণ দিয়েছেন"
          />
        </label>
        <p className="admin-muted">
          {note.trim().length}/{MAX_NOTE_CHARS} অক্ষর
        </p>
      </div>
    );
  }
  return (
    <div>
      <p>
        <strong>{product.title}</strong> ({product.store_name})
        প্ল্যাটফর্ম থেকে সাময়িকভাবে সরিয়ে নেওয়া হবে।
      </p>
      <ul className="admin-plain-list">
        <li>
          পণ্যটি সব পাবলিক জায়গা থেকে লুকিয়ে যাবে — তালিকা, সার্চ, হোম
          ফিড, কালেকশন, ফ্ল্যাশ সেল, স্টোরফ্রন্ট ও ফলোয়ার ফিড —{" "}
          <strong>কিছুই মুছে যাবে না।</strong>
        </li>
        <li>
          যেসব কার্টে পণ্যটি ইতিমধ্যে আছে, সেগুলোর চেকআউট এই পণ্যের জন্য
          আটকে যাবে; নতুন করে কার্টেও যোগ করা যাবে না।
        </li>
        <li>
          ইতিমধ্যে হয়ে যাওয়া অর্ডার অপরিবর্তিত থাকবে — ডেলিভারি ও রিটার্ন
          স্বাভাবিকভাবে চলবে।
        </li>
        <li>
          চলমান প্রমোশন বা ফ্ল্যাশ সেল <strong>থামবে না</strong> এবং
          প্রমোশনের টাকা ফেরত হয় না — শুধু প্লেসমেন্ট দেখা যাবে না।
        </li>
        <li>
          ছবি URL দিয়ে সরাসরি খোলা যাবে — ছবিটাই সমস্যা হলে মিডিয়া
          মডারেশন থেকে ছবিটি মুছুন।
        </li>
        <li>
          বিক্রেতা পণ্যটি সম্পাদনা করতে পারবেন, কিন্তু নিজে ফেরত আনতে
          পারবেন না — ফেরত শুধু অ্যাডমিন দিতে পারবেন।
        </li>
      </ul>
      <label>
        অপসারণের কারণ — Reason (বিক্রেতা দেখবেন)
        <textarea
          value={note}
          onChange={(e) => onNoteChange(e.target.value)}
          maxLength={MAX_NOTE_CHARS}
          rows={3}
          placeholder="যেমন: নকল পণ্য বিক্রির প্রমাণ মিলেছে"
        />
      </label>
      <p className="admin-muted">
        {note.trim().length}/{MAX_NOTE_CHARS} অক্ষর — কারণটি পণ্যে সংরক্ষিত
        হবে, অডিট লগে লেখা হবে, আর বিক্রেতার পণ্যের তালিকা ও নোটিশে যাবে।
      </p>
      {product.store_status === "suspended" ? (
        <p className="payout-warning" role="alert">
          সতর্কতা: এই পণ্যের স্টোর ইতিমধ্যে স্থগিত আছে — পণ্যটি এমনিতেই
          লুকানো। তবু সরিয়ে রাখলে স্টোর চালু হলেও পণ্যটি লুকানোই থাকবে।
        </p>
      ) : null}
    </div>
  );
}
