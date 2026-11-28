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
  getAdminStoreDetail,
  listAdminStores,
  planLabel,
  setAdminStoreStatus,
  storeStatusLabel,
  type AdminStore,
  type AdminStoreDetail,
  type AdminStoreList,
  type AdminStoreSort,
} from "@/lib/api";
import { formatBdt, formatCount, formatDhakaDateTime } from "@/lib/format";
import { useAuth } from "@/hooks/useAuth";

const PAGE_LIMIT = 25;
const MAX_NOTE_CHARS = 500;

const STATUS_FILTERS: { value: string; label: string }[] = [
  { value: "", label: "সব — All" },
  { value: "active", label: "চালু — Active" },
  { value: "suspended", label: "স্থগিত — Suspended" },
];

const TIER_FILTERS: { value: string; label: string }[] = [
  { value: "", label: "সব টায়ার — All tiers" },
  { value: "unverified", label: "যাচাই হয়নি — Unverified" },
  { value: "basic", label: "যাচাইকৃত বিক্রেতা — Basic" },
  { value: "verified", label: "যাচাইকৃত ব্যবসা — Verified" },
];

const SORT_OPTIONS: { value: AdminStoreSort; label: string }[] = [
  { value: "newest", label: "সবচেয়ে নতুন আগে — Newest first" },
  { value: "oldest", label: "সবচেয়ে পুরনো আগে — Oldest first" },
  { value: "name_asc", label: "নাম অনুযায়ী — Name A–Z" },
  { value: "gmv_desc", label: "বড় GMV আগে — GMV: high to low" },
  { value: "followers_desc", label: "বেশি ফলোয়ার আগে — Most followers" },
];

const VALID_STATUSES = ["active", "suspended"];

function tierLabel(tier: string): string {
  switch (tier) {
    case "basic":
      return "যাচাইকৃত বিক্রেতা";
    case "verified":
      return "যাচাইকৃত ব্যবসা";
    case "unverified":
      return "যাচাই হয়নি";
    default:
      return tier;
  }
}

function tierTone(tier: string): "brand" | "accent" | "muted" {
  switch (tier) {
    case "verified":
      return "brand";
    case "basic":
      return "accent";
    default:
      return "muted";
  }
}

/**
 * Admin stores — the store moderation queue, remade after
 * the surface analysis of the admin remake program (fourth
 * surface). What an admin can do here, and what the API
 * guarantees behind it:
 *
 * - Decide with the numbers visible: every row carries the
 *   tier, the effective plan, delivered GMV, the pending
 *   payout exposure and open returns; the detail composes
 *   the canonical earnings, returns exposure, verification
 *   state, the owner's other stores and the audit history.
 * - Suspend / reactivate run through dialogs that state
 *   exactly what the move does and does not do. Suspend
 *   REQUIRES a note: it is stored on the store, written
 *   into the audit detail, shown to the seller on their
 *   dashboard, and quoted in the notice the API enqueues
 *   in the same transaction. A no-op transition is a 409
 *   from the API — this page freezes the row and reloads.
 * - The owner's phone is MASKED everywhere on this surface
 *   (the pre-remake list leaked the full number). No store
 *   decision is executed by phone, so there is no reveal.
 *
 * Suspension never deletes anything, never cancels a
 * payout, and does NOT pause subscription billing — the
 * dialog copy says all three, because each has surprised
 * an admin before.
 */
export default function AdminStoresPage() {
  return (
    <AdminShell>
      <Suspense fallback={<p>লোড হচ্ছে…</p>}>
        <Stores />
      </Suspense>
    </AdminShell>
  );
}

type DialogState =
  | { kind: "suspend"; store: AdminStore }
  | { kind: "reactivate"; store: AdminStore }
  | null;

function Stores() {
  const { token } = useAuth();
  const searchParams = useSearchParams();
  const initialStatus = (() => {
    const param = searchParams.get("status") ?? "";
    return VALID_STATUSES.includes(param) ? param : "";
  })();

  const [status, setStatus] = useState(initialStatus);
  const [tier, setTier] = useState("");
  const [sort, setSort] = useState<AdminStoreSort>("newest");
  const [qInput, setQInput] = useState("");
  const [q, setQ] = useState("");
  const [page, setPage] = useState(1);

  const [list, setList] = useState<AdminStoreList | null>(null);
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
  const [detail, setDetail] = useState<AdminStoreDetail | null>(null);
  const [detailError, setDetailError] = useState(false);
  const [loadingDetailId, setLoadingDetailId] = useState<string | null>(null);

  const [dialog, setDialog] = useState<DialogState>(null);
  const [suspendNote, setSuspendNote] = useState("");
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
      const next = await listAdminStores(token, {
        q: q || undefined,
        status: status || undefined,
        tier: tier || undefined,
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
  }, [token, status, tier, q, sort, page, tick]);

  useEffect(() => {
    void load();
  }, [load]);

  const reload = useCallback(() => setTick((value) => value + 1), []);

  function changeStatus(value: string) {
    setStatus(value);
    setPage(1);
  }

  function changeTier(value: string) {
    setTier(value);
    setPage(1);
  }

  function changeSort(value: AdminStoreSort) {
    setSort(value);
    setPage(1);
  }

  async function loadDetail(id: string) {
    if (!token) return;
    setLoadingDetailId(id);
    setDetailError(false);
    try {
      const next = await getAdminStoreDetail(token, id);
      setDetail(next);
    } catch {
      setDetailError(true);
    } finally {
      setLoadingDetailId(null);
    }
  }

  async function toggleDetail(store: AdminStore) {
    if (expandedId === store.id) {
      setExpandedId(null);
      setDetail(null);
      return;
    }
    setExpandedId(store.id);
    setDetail(null);
    await loadDetail(store.id);
  }

  async function refetchDetail(id: string) {
    if (!token || expandedId !== id) return;
    try {
      setDetail(await getAdminStoreDetail(token, id));
    } catch {
      setDetailError(true);
    }
  }

  function openDialog(kind: "suspend" | "reactivate", store: AdminStore) {
    setDialog({ kind, store });
    setSuspendNote("");
    setDialogError(null);
  }

  function closeDialog() {
    if (formBusy) return;
    setDialog(null);
    setDialogError(null);
  }

  async function confirmDialog() {
    if (!token || !dialog) return;
    const { kind, store } = dialog;
    // Client-side mirror of the API's requirement: the
    // API's 400 is the authority, this only saves a trip.
    if (kind === "suspend" && suspendNote.trim() === "") {
      setDialogError("স্থগিতের কারণ লিখুন — a suspension note is required.");
      return;
    }
    setFormBusy(true);
    setDialogError(null);
    try {
      await setAdminStoreStatus(
        token,
        store.id,
        kind === "suspend" ? "suspended" : "active",
        kind === "suspend" ? suspendNote.trim() : undefined
      );
      setDialog(null);
      setNotice(
        kind === "suspend"
          ? "স্টোর স্থগিত করা হয়েছে — কারণসহ বিক্রেতাকে জানানো হয়েছে। (Store suspended — the seller has been notified with the reason.)"
          : "স্টোর আবার চালু করা হয়েছে — বিক্রেতাকে জানানো হয়েছে। (Store reactivated — the seller has been notified.)"
      );
      setPageError(null);
      await load();
      await refetchDetail(store.id);
    } catch (err) {
      if (err instanceof ApiCallError && err.status === 409) {
        // No-op transition: the store already holds the
        // target status (someone else moved it, or this
        // row went stale). Freeze the row, refetch, and
        // keep the server's message visible.
        setDialog(null);
        setConflictedIds((prev) => new Set(prev).add(store.id));
        setPageError(err.message);
        await load();
      } else {
        setDialogError(
          err instanceof Error ? err.message : "স্ট্যাটাস বদলানো যায়নি।"
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
          <h2>স্টোর — Stores</h2>
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
          onPick={changeStatus}
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
          টায়ার — Tier
          <select value={tier} onChange={(e) => changeTier(e.target.value)}>
            {TIER_FILTERS.map((f) => (
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
            onChange={(e) => changeSort(e.target.value as AdminStoreSort)}
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
            placeholder="স্টোরের নাম বা স্লাগ"
          />
        </label>
      </div>

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
          <p>স্টোর লোড করা যায়নি — Couldn&apos;t load stores.</p>
          <Button size="sm" onClick={reload}>
            আবার চেষ্টা করুন — Retry
          </Button>
        </div>
      ) : null}

      {list && list.items.length === 0 ? (
        <EmptyState title={emptyTitle(status, tier, q)}>
          <p>{emptyBody(status, tier, q)}</p>
        </EmptyState>
      ) : null}

      {list ? (
        <div className="queue-list">
          {list.items.map((store) => (
            <StoreCard
              key={store.id}
              store={store}
              conflicted={conflictedIds.has(store.id)}
              expanded={expandedId === store.id}
              detail={expandedId === store.id ? detail : null}
              detailError={expandedId === store.id && detailError}
              detailLoading={loadingDetailId === store.id}
              onToggleDetail={() => void toggleDetail(store)}
              onRetryDetail={() => void loadDetail(store.id)}
              onAction={(kind) => openDialog(kind, store)}
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
          dialog?.kind === "suspend"
            ? "স্টোর স্থগিত করুন — Suspend store"
            : "স্টোর আবার চালু করুন — Reactivate store"
        }
        variant={dialog?.kind === "suspend" ? "danger" : "default"}
        onConfirm={() => void confirmDialog()}
        confirmLabel={
          dialog?.kind === "suspend"
            ? "স্থগিত করুন — Suspend"
            : "চালু করুন — Reactivate"
        }
        confirmDisabled={
          dialog?.kind === "suspend" ? suspendNote.trim() === "" : false
        }
        busy={formBusy}
        error={dialogError}
      >
        {dialog ? (
          <StoreDialogBody
            kind={dialog.kind}
            store={dialog.store}
            note={suspendNote}
            onNoteChange={setSuspendNote}
          />
        ) : null}
      </Dialog>
    </>
  );
}

function emptyTitle(status: string, tier: string, q: string): string {
  if (q) return "খোঁজে কোনো স্টোর মেলেনি — No stores match the search";
  if (status === "suspended") return "কোনো স্টোর স্থগিত নেই — No suspended stores";
  if (tier) return "এই টায়ারে কোনো স্টোর নেই — No stores in this tier";
  return "কোনো স্টোর নেই — No stores yet";
}

function emptyBody(status: string, tier: string, q: string): string {
  if (q)
    return "নাম বা স্লাগের অন্য অংশ দিয়ে খুঁজে দেখুন। (Try another part of the name or slug.)";
  if (status === "suspended")
    return "সব স্টোর চালু আছে। (Every store is active.)";
  if (tier)
    return "ফিল্টার বদলে আবার দেখুন। (Change the filter to see more.)";
  return "প্রথম স্টোর তৈরি হলেই এখানে দেখা যাবে। (Stores appear here once created.)";
}

// ---------------------------------------------------------------------------
// Summary strip: the whole platform by status + tier
// ---------------------------------------------------------------------------

function SummaryStrip({
  summary,
  activeStatus,
  onPick,
}: {
  summary: AdminStoreList["summary"];
  activeStatus: string;
  onPick: (status: string) => void;
}) {
  const chips: { status: string; label: string; count: number }[] = [
    { status: "", label: "সব স্টোর", count: summary.total },
    { status: "active", label: storeStatusLabel("active"), count: summary.active },
    {
      status: "suspended",
      label: storeStatusLabel("suspended"),
      count: summary.suspended,
    },
  ];
  return (
    <div className="pipeline-row" aria-label="স্টোর সারাংশ — Store summary">
      {chips.map((chip) => (
        <span key={chip.status || "all"} className="pipeline-chip">
          <Button
            size="sm"
            variant={activeStatus === chip.status ? "primary" : "default"}
            onClick={() => onPick(chip.status)}
          >
            {chip.label}: {formatCount(chip.count)}টি
          </Button>
        </span>
      ))}
      <span className="pipeline-chip">
        <Badge tone="muted">
          যাচাই হয়নি: {formatCount(summary.unverified)}টি · যাচাইকৃত বিক্রেতা:{" "}
          {formatCount(summary.basic)}টি · যাচাইকৃত ব্যবসা:{" "}
          {formatCount(summary.verified)}টি
        </Badge>
      </span>
    </div>
  );
}

// ---------------------------------------------------------------------------
// One queue card
// ---------------------------------------------------------------------------

function StoreCard({
  store,
  conflicted,
  expanded,
  detail,
  detailError,
  detailLoading,
  onToggleDetail,
  onRetryDetail,
  onAction,
}: {
  store: AdminStore;
  conflicted: boolean;
  expanded: boolean;
  detail: AdminStoreDetail | null;
  detailError: boolean;
  detailLoading: boolean;
  onToggleDetail: () => void;
  onRetryDetail: () => void;
  onAction: (kind: "suspend" | "reactivate") => void;
}) {
  const suspended = store.status === "suspended";
  return (
    <article className="payout-card">
      <div className="payout-card-head">
        <div>
          <p>
            {store.logo_url ? (
              <>
                {/* eslint-disable-next-line @next/next/no-img-element */}
                <img
                  src={store.logo_url}
                  alt=""
                  style={{
                    height: 28,
                    width: 28,
                    objectFit: "cover",
                    borderRadius: 6,
                    verticalAlign: "middle",
                    marginRight: 8,
                  }}
                />
              </>
            ) : null}
            <strong>{store.name}</strong>{" "}
            <span className="admin-muted">({store.slug})</span>
          </p>
          <p className="payout-card-meta">
            মালিক: {store.owner_name ?? "—"} · {store.owner_phone_masked} · তৈরি:{" "}
            {formatDhakaDateTime(store.created_at)}
          </p>
        </div>
        <div className="payout-card-actions">
          <Badge tone={suspended ? "danger" : "brand"}>
            {storeStatusLabel(store.status)}
          </Badge>
          <Badge tone={tierTone(store.verified_tier)}>
            {tierLabel(store.verified_tier)}
          </Badge>
          <Badge tone="muted">{planLabel(store.plan)}</Badge>
        </div>
      </div>

      {suspended ? (
        <p className="payout-card-meta">
          স্থগিতের কারণ: {store.suspension_note ?? "— (আগের সিদ্ধান্তে কারণ লেখা হয়নি)"}
          {store.status_changed_at
            ? ` · ${formatDhakaDateTime(store.status_changed_at)} থেকে`
            : ""}
        </p>
      ) : null}

      <p className="payout-card-meta">
        পণ্য {formatCount(store.product_count)}টি · অর্ডার {formatCount(store.order_count)}টি
        · ডেলিভার্ড GMV {formatBdt(store.delivered_gmv_poisha)} ·{" "}
        <Link href={`/admin/stores/${store.id}/followers`}>
          ফলোয়ার {formatCount(store.follower_count)}জন
        </Link>
        {store.pending_payout_count > 0 ? (
          <>
            {" "}
            ·{" "}
            <strong>
              পে-আউট অপেক্ষমাণ {formatCount(store.pending_payout_count)}টি (
              <PriceTag amountPoisha={store.pending_payout_poisha} />)
            </strong>
          </>
        ) : null}
        {store.open_returns_count > 0 ? (
          <> · রিটার্ন খোলা {formatCount(store.open_returns_count)}টি</>
        ) : null}
      </p>

      <div className="payout-card-actions">
        <Button size="sm" onClick={onToggleDetail}>
          {expanded ? "বিস্তারিত লুকান — Hide detail" : "বিস্তারিত — Detail"}
        </Button>
        {suspended ? (
          <Button
            size="sm"
            disabled={conflicted}
            onClick={() => onAction("reactivate")}
          >
            সক্রিয় করুন — Reactivate
          </Button>
        ) : (
          <Button
            size="sm"
            variant="danger"
            disabled={conflicted}
            onClick={() => onAction("suspend")}
          >
            সাসপেন্ড করুন — Suspend
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

function DetailBody({ detail }: { detail: AdminStoreDetail }) {
  const earnings = detail.earnings;
  const exposure = detail.returns_exposure;
  return (
    <div>
      <h4>আয় — Earnings</h4>
      <ul className="admin-plain-list">
        <li>মোট আয় (ডেলিভার্ড): {formatBdt(earnings.earned_poisha)}</li>
        <li>পরিশোধিত + অনুমোদিত: {formatBdt(earnings.paid_out_poisha)}</li>
        <li>অপেক্ষমাণ পে-আউট: {formatBdt(earnings.pending_poisha)}</li>
        <li>চার্জ মোট: {formatBdt(earnings.charges_total_poisha)}</li>
        <li>
          <strong>তোলা যাবে: {formatBdt(earnings.available_poisha)}</strong>
        </li>
      </ul>

      <h4>রিটার্ন ঝুঁকি — Returns exposure</h4>
      <ul className="admin-plain-list">
        <li>
          খোলা রিটার্ন: {formatCount(exposure.in_flight_count)}টি · ঝুঁকিতে থাকা
          পে-আউট: {formatBdt(exposure.at_risk_payout_poisha)}
        </li>
        <li>
          রিফান্ড অপেক্ষমাণ (গৃহীত রিটার্ন):{" "}
          {formatCount(exposure.received_refund_pending_count)}টি ·{" "}
          {formatBdt(exposure.received_refund_pending_poisha)}
        </li>
      </ul>

      <h4>সাবস্ক্রিপশন — Subscription</h4>
      <ul className="admin-plain-list">
        <li>
          কার্যকর প্ল্যান: {planLabel(detail.subscription.plan)}
          {detail.subscription.expires_at
            ? ` · মেয়াদ শেষ: ${formatDhakaDateTime(detail.subscription.expires_at)}`
            : ""}
        </li>
        <li>কমিশন ছাড়: {detail.subscription.commission_discount_bps / 100}%</li>
      </ul>

      <h4>যাচাইকরণ — Verification</h4>
      <ul className="admin-plain-list">
        <li>
          আবেদনের অবস্থা: {detail.verification.status} · টায়ার:{" "}
          {tierLabel(detail.verified_tier)}
        </li>
        {detail.verification.reviewer_note ? (
          <li>পর্যালোচকের নোট: {detail.verification.reviewer_note}</li>
        ) : null}
      </ul>

      <h4>মালিকের স্টোরসমূহ — Owner&apos;s stores</h4>
      {detail.owner_stores.length <= 1 ? (
        <p className="payout-card-meta">এই মালিকের আর কোনো স্টোর নেই।</p>
      ) : (
        <ul className="admin-plain-list">
          {detail.owner_stores.map((sibling) => (
            <li key={sibling.store_id}>
              {sibling.store_name} ({sibling.store_slug}) —{" "}
              {storeStatusLabel(sibling.status)} · {tierLabel(sibling.tier)}
              {sibling.store_id === detail.id ? " · এই স্টোর" : ""}
            </li>
          ))}
        </ul>
      )}

      <h4>অডিট ইতিহাস — Audit history</h4>
      {detail.audit_history.length === 0 ? (
        <p className="payout-card-meta">কোনো ইতিহাস পাওয়া যায়নি।</p>
      ) : (
        <ul className="admin-plain-list">
          {detail.audit_history.map((entry) => {
            const note =
              typeof entry.detail?.note === "string" ? entry.detail.note : null;
            return (
              <li key={entry.id}>
                {entry.action} · {entry.admin_phone_masked} ·{" "}
                {formatDhakaDateTime(entry.created_at)}
                {note ? ` · কারণ: ${note}` : ""}
              </li>
            );
          })}
        </ul>
      )}

      <p className="payout-card-meta">
        <Link href={`/admin/stores/${detail.id}/followers`}>
          ফলোয়ার তালিকা দেখুন — View followers ({formatCount(detail.follower_count)})
        </Link>
      </p>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Dialog bodies: honest consequences, stated before the click
// ---------------------------------------------------------------------------

function StoreDialogBody({
  kind,
  store,
  note,
  onNoteChange,
}: {
  kind: "suspend" | "reactivate";
  store: AdminStore;
  note: string;
  onNoteChange: (value: string) => void;
}) {
  if (kind === "reactivate") {
    return (
      <div>
        <p>
          <strong>{store.name}</strong> ({store.slug}) আবার চালু হবে।
        </p>
        <ul className="admin-plain-list">
          <li>সব পণ্য আবার পাবলিক তালিকা, সার্চ ও স্টোরফ্রন্টে দেখা যাবে।</li>
          <li>চেকআউট ও কার্টে যোগ করা আবার চালু হবে।</li>
          <li>পে-আউটের সিদ্ধান্ত (অনুমোদন / পরিশোধ) আবার নেওয়া যাবে।</li>
          <li>বিক্রেতাকে চালু হওয়ার খবর জানানো হবে।</li>
        </ul>
      </div>
    );
  }
  return (
    <div>
      <p>
        <strong>{store.name}</strong> ({store.slug}) সাময়িকভাবে স্থগিত হবে।
      </p>
      <ul className="admin-plain-list">
        <li>
          সব পণ্য পাবলিক তালিকা, সার্চ ও স্টোরফ্রন্ট থেকে লুকিয়ে যাবে —{" "}
          <strong>কিছুই মুছে যাবে না।</strong>
        </li>
        <li>নতুন চেকআউট ও কার্টে যোগ করা বন্ধ থাকবে (চেকআউট 409 দেবে)।</li>
        <li>বিক্রেতা সাইন ইন করে ক্যাটালগ সামলাতে পারবেন।</li>
        <li>
          অপেক্ষমাণ পে-আউট <strong>বাতিল হবে না</strong> — তবে অনুমোদন/পরিশোধের
          সিদ্ধান্ত স্থগিত থাকবে; প্রত্যাখ্যান করা যাবে।
        </li>
        <li>
          সাবস্ক্রিপশন বিলিং <strong>চলতে থাকবে</strong> — স্থগিত মানে বিলিং বন্ধ নয়।
        </li>
        <li>
          ফলোয়ারদের স্থগিতের কথা জানানো হবে না এবং ফলো অপরিবর্তিত থাকবে;
          স্থগিত অবস্থায় নতুন পণ্যের নোটিশও যাবে না।
        </li>
      </ul>
      <label>
        স্থগিতের কারণ — Reason (বিক্রেতা দেখবেন)
        <textarea
          value={note}
          onChange={(e) => onNoteChange(e.target.value)}
          maxLength={MAX_NOTE_CHARS}
          rows={3}
          placeholder="যেমন: জালিয়াতির অভিযোগ তদন্তাধীন"
        />
      </label>
      <p className="admin-muted">
        {note.trim().length}/{MAX_NOTE_CHARS} অক্ষর — কারণটি স্টোরে সংরক্ষিত হবে,
        অডিট লগে লেখা হবে, আর বিক্রেতার ড্যাশবোর্ড ও নোটিশে যাবে।
      </p>
      {store.pending_payout_count > 0 ? (
        <p className="payout-warning" role="alert">
          সতর্কতা: এই স্টোরের {formatCount(store.pending_payout_count)}টি পে-আউট
          অপেক্ষমাণ আছে (<PriceTag amountPoisha={store.pending_payout_poisha} />) —
          স্থগিত হলে সেগুলোর অনুমোদন/পরিশোধ জমে যাবে।
        </p>
      ) : null}
    </div>
  );
}
