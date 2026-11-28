"use client";

import {
  Suspense,
  useCallback,
  useEffect,
  useRef,
  useState,
} from "react";
import { useSearchParams } from "next/navigation";
import AdminShell from "@/components/AdminShell";
import PriceTag from "@/components/PriceTag";
import { Badge, Button, Dialog, EmptyState, Skeleton } from "@/components/ui";
import {
  ApiCallError,
  approvePayout,
  getAdminPayout,
  getAdminSettings,
  isPayoutCoverage,
  listAdminPayouts,
  markPayoutPaid,
  payoutMethodLabel,
  payoutStatusLabel,
  rejectPayout,
  type AdminPayout,
  type AdminPayoutDetail,
  type AdminPayoutList,
  type AdminPayoutSort,
  type AdminSettings,
  type PayoutCoverageDetails,
  type PayoutStatusBucket,
} from "@/lib/api";
import {
  formatAgeSeconds,
  formatBdt,
  formatCount,
  formatDhakaDateTime,
} from "@/lib/format";
import { useAuth } from "@/hooks/useAuth";

const PAGE_LIMIT = 25;
/** Queue-age thresholds (seconds): past a day a waiting
 * payout is highlighted; past three it is urgent. Product
 * constants, not settings — changing them is a code change,
 * on purpose. */
const AGE_AMBER_SECS = 24 * 3600;
const AGE_DANGER_SECS = 72 * 3600;
const MAX_REASON_CHARS = 500;

const STATUS_FILTERS: { value: string; label: string }[] = [
  { value: "", label: "সব — All" },
  { value: "requested", label: "অনুরোধ করা হয়েছে — Requested" },
  { value: "approved", label: "অনুমোদিত — Approved" },
  { value: "paid", label: "পরিশোধিত — Paid" },
  { value: "rejected", label: "প্রত্যাখ্যাত — Rejected" },
];

const SORT_OPTIONS: { value: AdminPayoutSort; label: string }[] = [
  { value: "oldest", label: "সবচেয়ে পুরনো আগে — Oldest first" },
  { value: "newest", label: "সবচেয়ে নতুন আগে — Newest first" },
  { value: "amount_desc", label: "বড় অঙ্ক আগে — Amount: high to low" },
  { value: "amount_asc", label: "ছোট অঙ্ক আগে — Amount: low to high" },
];

const VALID_STATUSES = ["requested", "approved", "paid", "rejected"];

/**
 * Admin payouts — the money-out queue, remade after the
 * ~66-agent surface analysis (the payouts surface of the
 * admin remake program). What an admin can do here, and
 * what the API guarantees behind it:
 *
 * - Decide with the numbers visible: every in-flight row
 *   carries the store's coverage at read time; the detail
 *   shows the earnings behind the amount, the charges, the
 *   returns still in flight, the KYC state, and whether
 *   the store's account changed since the request.
 * - Approve / reject / mark-paid all run through dialogs
 *   that restate the amount and the destination. Reject
 *   REQUIRES a reason (the seller sees it and is
 *   notified); mark-paid REQUIRES the payment reference
 *   (recorded on the payout, sent to the seller). The API
 *   enforces both, re-checks coverage inside the
 *   transition transaction, freezes suspended stores, and
 *   re-checks the KYC gate at approval — this page only
 *   presents those outcomes honestly.
 * - Identifiers are masked in the queue (Wave 28 canon);
 *   the full destination exists only in the detail, which
 *   is also where the printable payment instruction lives.
 *
 * Payouts always pay the account SNAPSHOT on the payout,
 * never the store's current account — the detail warns
 * when the two have diverged, and the warning says so.
 */
export default function AdminPayoutsPage() {
  return (
    <AdminShell>
      <Suspense fallback={<p>লোড হচ্ছে…</p>}>
        <Payouts />
      </Suspense>
    </AdminShell>
  );
}

type DialogState =
  | { kind: "approve"; payout: AdminPayout }
  | { kind: "reject"; payout: AdminPayout }
  | { kind: "paid"; payout: AdminPayout }
  | null;

function ageSeconds(iso: string): number {
  const ms = Date.now() - new Date(iso).getTime();
  return Number.isFinite(ms) ? Math.max(0, Math.floor(ms / 1000)) : 0;
}

function statusTone(payout: AdminPayout): "brand" | "accent" | "danger" | "muted" {
  switch (payout.status) {
    case "paid":
      return "brand";
    case "rejected":
      return "danger";
    case "requested":
    case "approved":
      return ageSeconds(payout.created_at) >= AGE_DANGER_SECS ? "danger" : "accent";
    default:
      return "muted";
  }
}

function Payouts() {
  const { token } = useAuth();
  const searchParams = useSearchParams();
  const initialStatus = (() => {
    const param = searchParams.get("status") ?? "";
    return VALID_STATUSES.includes(param) ? param : "requested";
  })();

  const [status, setStatus] = useState(initialStatus);
  const [sort, setSort] = useState<AdminPayoutSort>("oldest");
  const [qInput, setQInput] = useState("");
  const [q, setQ] = useState("");
  const [page, setPage] = useState(1);

  const [list, setList] = useState<AdminPayoutList | null>(null);
  const [settings, setSettings] = useState<AdminSettings | null>(null);
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
  const [detail, setDetail] = useState<AdminPayoutDetail | null>(null);
  const [detailError, setDetailError] = useState(false);
  const [loadingDetailId, setLoadingDetailId] = useState<string | null>(null);

  const [dialog, setDialog] = useState<DialogState>(null);
  const [rejectReason, setRejectReason] = useState("");
  const [paymentReference, setPaymentReference] = useState("");
  const [dialogError, setDialogError] = useState<string | null>(null);
  const [dialogCoverage, setDialogCoverage] =
    useState<PayoutCoverageDetails | null>(null);
  const [formBusy, setFormBusy] = useState(false);

  const [printDetail, setPrintDetail] = useState<AdminPayoutDetail | null>(null);

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
      const next = await listAdminPayouts(token, {
        status: status || undefined,
        q: q || undefined,
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
  }, [token, status, q, sort, page, tick]);

  useEffect(() => {
    void load();
  }, [load]);

  useEffect(() => {
    if (!token) return;
    let cancelled = false;
    getAdminSettings(token)
      .then((next) => {
        if (!cancelled) setSettings(next);
      })
      .catch(() => {
        // The settings strip is context, not a decision
        // input — the API enforces the values either way.
        if (!cancelled) setSettings(null);
      });
    return () => {
      cancelled = true;
    };
  }, [token]);

  const reload = useCallback(() => setTick((value) => value + 1), []);

  async function loadDetail(id: string) {
    if (!token) return;
    setLoadingDetailId(id);
    setDetailError(false);
    try {
      const next = await getAdminPayout(token, id);
      setDetail(next);
    } catch {
      setDetailError(true);
    } finally {
      setLoadingDetailId(null);
    }
  }

  async function toggleDetail(payout: AdminPayout) {
    if (expandedId === payout.id) {
      setExpandedId(null);
      setDetail(null);
      return;
    }
    setExpandedId(payout.id);
    setDetail(null);
    await loadDetail(payout.id);
  }

  async function refetchDetail(id: string) {
    if (!token || expandedId !== id) return;
    try {
      setDetail(await getAdminPayout(token, id));
    } catch {
      setDetailError(true);
    }
  }

  function openDialog(kind: "approve" | "reject" | "paid", payout: AdminPayout) {
    setDialog({ kind, payout });
    setRejectReason("");
    setPaymentReference("");
    setDialogError(null);
    setDialogCoverage(null);
  }

  function closeDialog() {
    if (formBusy) return;
    setDialog(null);
    setDialogError(null);
    setDialogCoverage(null);
  }

  async function confirmDialog() {
    if (!token || !dialog) return;
    const { kind, payout } = dialog;
    // Client-side mirrors of the API's requirements: the
    // API's 400 is the authority, these only save a trip.
    if (kind === "reject" && rejectReason.trim() === "") {
      setDialogError("প্রত্যাখ্যানের কারণ লিখুন — a rejection reason is required.");
      return;
    }
    if (kind === "paid" && paymentReference.trim() === "") {
      setDialogError("পেমেন্ট রেফারেন্স লিখুন — a payment reference is required.");
      return;
    }
    setFormBusy(true);
    setDialogError(null);
    setDialogCoverage(null);
    try {
      if (kind === "approve") await approvePayout(token, payout.id);
      if (kind === "reject") await rejectPayout(token, payout.id, rejectReason.trim());
      if (kind === "paid") {
        await markPayoutPaid(token, payout.id, paymentReference.trim());
      }
      setDialog(null);
      setNotice(
        kind === "approve"
          ? "অনুমোদিত হয়েছে — বিক্রেতাকে জানানো হয়েছে। (Approved — the seller has been notified.)"
          : kind === "reject"
            ? "প্রত্যাখ্যাত হয়েছে — কারণসহ বিক্রেতাকে জানানো হয়েছে। (Rejected — the seller has been notified with the reason.)"
            : "পরিশোধিত চিহ্নিত হয়েছে — রেফারেন্সসহ বিক্রেতাকে জানানো হয়েছে। (Marked paid — the seller has been notified with the reference.)"
      );
      setPageError(null);
      await load();
      await refetchDetail(payout.id);
    } catch (err) {
      if (isPayoutCoverage(err)) {
        // The row is not stale — the store's balance moved.
        // Keep the dialog open with the full picture; the
        // admin can still reject from here.
        setDialogCoverage(err.details);
        setDialogError(err.message);
      } else if (err instanceof ApiCallError && err.status === 409) {
        // Illegal transition: someone (or something) already
        // moved this payout. The row IS stale — freeze its
        // buttons, refetch, and keep the message visible.
        setDialog(null);
        setConflictedIds((prev) => new Set(prev).add(payout.id));
        setPageError(err.message);
        await load();
      } else if (err instanceof ApiCallError && err.status === 404) {
        setDialog(null);
        setPageError("পে-আউটটি পাওয়া যায়নি — তালিকা হালনাগাদ করা হয়েছে। (The payout no longer exists — the list has been refreshed.)");
        await load();
      } else {
        setDialogError(
          err instanceof Error ? err.message : "অ্যাকশন সম্পন্ন হয়নি।"
        );
      }
    } finally {
      setFormBusy(false);
    }
  }

  function changeStatus(value: string) {
    setStatus(value);
    setPage(1);
    setExpandedId(null);
    setDetail(null);
  }

  function changeSort(value: AdminPayoutSort) {
    setSort(value);
    setPage(1);
  }

  if (printDetail) {
    return (
      <PrintView
        detail={printDetail}
        onDone={() => setPrintDetail(null)}
      />
    );
  }

  const totalPages = list ? Math.max(1, Math.ceil(list.total / PAGE_LIMIT)) : 1;

  return (
    <>
      <div className="admin-overview-head">
        <div>
          <h2>পে-আউট — Payouts</h2>
          <p className="admin-muted">
            সর্বশেষ হালনাগাদ: {asOf ? formatDhakaDateTime(asOf) : "—"} (ঢাকা সময়)
          </p>
        </div>
        <Button onClick={reload}>↻ হালনাগাদ করুন — Refresh</Button>
      </div>

      {list ? (
        <SummaryStrip
          summary={list.summary}
          settings={settings}
          activeStatus={status}
          onPick={changeStatus}
        />
      ) : null}

      <div className="payout-controls">
        <label>
          স্ট্যাটাস — Status
          <select
            value={status}
            onChange={(e) => changeStatus(e.target.value)}
          >
            {STATUS_FILTERS.map((f) => (
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
            onChange={(e) => changeSort(e.target.value as AdminPayoutSort)}
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
            placeholder="স্টোর, ফোন, অ্যাকাউন্ট নম্বর বা আইডেমপোটেন্সি কী"
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
          <p>পে-আউট লোড করা যায়নি — Couldn&apos;t load payouts.</p>
          <Button size="sm" onClick={reload}>
            আবার চেষ্টা করুন — Retry
          </Button>
        </div>
      ) : null}

      {list && list.items.length === 0 ? (
        <EmptyState title={emptyTitle(status, q)}>
          <p>{emptyBody(status, q)}</p>
        </EmptyState>
      ) : null}

      {list ? (
        <div className="queue-list">
          {list.items.map((payout) => (
            <PayoutCard
              key={payout.id}
              payout={payout}
              settings={settings}
              conflicted={conflictedIds.has(payout.id)}
              expanded={expandedId === payout.id}
              detail={expandedId === payout.id ? detail : null}
              detailError={expandedId === payout.id && detailError}
              detailLoading={loadingDetailId === payout.id}
              onToggleDetail={() => void toggleDetail(payout)}
              onRetryDetail={() => void loadDetail(payout.id)}
              onAction={(kind) => openDialog(kind, payout)}
              onPrint={(d) => setPrintDetail(d)}
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
          dialog?.kind === "approve"
            ? "পে-আউট অনুমোদন — Approve payout"
            : dialog?.kind === "reject"
              ? "পে-আউট প্রত্যাখ্যান — Reject payout"
              : "পরিশোধিত চিহ্নিত করুন — Mark payout paid"
        }
        variant={dialog?.kind === "reject" ? "danger" : "default"}
        onConfirm={() => void confirmDialog()}
        confirmLabel={
          dialog?.kind === "approve"
            ? "অনুমোদন করুন — Approve"
            : dialog?.kind === "reject"
              ? "প্রত্যাখ্যান করুন — Reject"
              : "টাকা পাঠিয়েছি — পরিশোধিত চিহ্নিত করুন"
        }
        confirmDisabled={
          dialog?.kind === "reject"
            ? rejectReason.trim() === ""
            : dialog?.kind === "paid"
              ? paymentReference.trim() === ""
              : false
        }
        busy={formBusy}
        error={dialogError}
      >
        {dialog ? (
          <DialogBody
            dialog={dialog}
            coverage={dialogCoverage}
            rejectReason={rejectReason}
            onRejectReason={setRejectReason}
            paymentReference={paymentReference}
            onPaymentReference={setPaymentReference}
            onSwitchToReject={() =>
              setDialog({ kind: "reject", payout: dialog.payout })
            }
          />
        ) : null}
      </Dialog>
    </>
  );
}

function emptyTitle(status: string, q: string): string {
  if (q) return "কিছু পাওয়া যায়নি — No matches";
  switch (status) {
    case "requested":
      return "কোনো অপেক্ষমাণ পে-আউট নেই";
    case "approved":
      return "পরিশোধের অপেক্ষায় কোনো পে-আউট নেই";
    case "paid":
      return "এখনো কোনো পে-আউট পরিশোধ হয়নি";
    case "rejected":
      return "কোনো প্রত্যাখ্যাত পে-আউট নেই";
    default:
      return "কোনো পে-আউট নেই";
  }
}

function emptyBody(status: string, q: string): string {
  if (q) {
    return "এই খোঁজে কোনো পে-আউট মেলেনি। অন্য স্টোর, ফোন নম্বর, অ্যাকাউন্ট নম্বর বা আইডেমপোটেন্সি কী দিয়ে চেষ্টা করুন।";
  }
  switch (status) {
    case "requested":
      return "নতুন পে-আউট অনুরোধ এলে এখানে দেখা যাবে। সব স্ট্যাটাস দেখতে উপরের ফিল্টার বদলান।";
    case "approved":
      return "অনুমোদিত কিন্তু এখনো টাকা পাঠানো হয়নি — এমন পে-আউট এখানে থাকে।";
    case "paid":
      return "পরিশোধিত পে-আউটের ইতিহাস এখানে থাকবে, রেফারেন্সসহ।";
    case "rejected":
      return "প্রত্যাখ্যাত পে-আউট এখানে থাকবে, কারণসহ — বিক্রেতা কারণটি দেখতে পান।";
    default:
      return "বিক্রেতারা পে-আউট অনুরোধ করলে এখানে দেখা যাবে।";
  }
}

// ---------------------------------------------------------------------------
// Summary strip: the whole queue by status + the policy values
// ---------------------------------------------------------------------------

function SummaryStrip({
  summary,
  settings,
  activeStatus,
  onPick,
}: {
  summary: AdminPayoutList["summary"];
  settings: AdminSettings | null;
  activeStatus: string;
  onPick: (status: string) => void;
}) {
  const buckets: { status: string; bucket: PayoutStatusBucket; queueAge: boolean }[] = [
    { status: "requested", bucket: summary.requested, queueAge: true },
    { status: "approved", bucket: summary.approved, queueAge: true },
    { status: "paid", bucket: summary.paid, queueAge: false },
    { status: "rejected", bucket: summary.rejected, queueAge: false },
  ];
  return (
    <div className="pipeline-row" aria-label="পে-আউট সারাংশ — Payout summary">
      {buckets.map(({ status, bucket, queueAge }) => (
        <span key={status} className="pipeline-chip">
          <Button
            size="sm"
            variant={activeStatus === status ? "primary" : "default"}
            onClick={() => onPick(status)}
          >
            {payoutStatusLabel(status)}: {formatCount(bucket.count)}টি ·{" "}
            <PriceTag amountPoisha={bucket.amount_poisha} />
          </Button>
          {queueAge && bucket.oldest_age_secs !== null ? (
            <span className="admin-muted">
              সবচেয়ে পুরনো: {formatAgeSeconds(bucket.oldest_age_secs)} ধরে
            </span>
          ) : null}
          {!queueAge && bucket.count > 0 ? (
            <span className="admin-muted">সর্বমোট — all-time</span>
          ) : null}
        </span>
      ))}
      {settings ? (
        <>
          <span className="pipeline-chip">
            <Badge tone="muted">
              সর্বনিম্ন পে-আউট: <PriceTag amountPoisha={settings.min_payout_poisha} />
            </Badge>
          </span>
          <span className="pipeline-chip">
            <Badge tone={settings.kyc_payout_gate ? "accent" : "muted"}>
              KYC গেট: {settings.kyc_payout_gate ? "চালু আছে" : "বন্ধ আছে"}
            </Badge>
          </span>
        </>
      ) : null}
    </div>
  );
}

// ---------------------------------------------------------------------------
// One queue card
// ---------------------------------------------------------------------------

function PayoutCard({
  payout,
  settings,
  conflicted,
  expanded,
  detail,
  detailError,
  detailLoading,
  onToggleDetail,
  onRetryDetail,
  onAction,
  onPrint,
}: {
  payout: AdminPayout;
  settings: AdminSettings | null;
  conflicted: boolean;
  expanded: boolean;
  detail: AdminPayoutDetail | null;
  detailError: boolean;
  detailLoading: boolean;
  onToggleDetail: () => void;
  onRetryDetail: () => void;
  onAction: (kind: "approve" | "reject" | "paid") => void;
  onPrint: (detail: AdminPayoutDetail) => void;
}) {
  const age = ageSeconds(payout.created_at);
  const inFlight = payout.status === "requested" || payout.status === "approved";
  const grandfathered =
    payout.status === "requested" &&
    settings !== null &&
    payout.amount_poisha < settings.min_payout_poisha;
  return (
    <article className="payout-card">
      <div className="payout-card-head">
        <div>
          <div className="queue-row-main">
            <span>{payout.store_name}</span>
            <Badge tone={statusTone(payout)}>{payoutStatusLabel(payout.status)}</Badge>
            {payout.store_status === "suspended" ? (
              <Badge tone="danger">স্টোর সাসপেন্ডেড</Badge>
            ) : null}
            {payout.possible_duplicate ? (
              <Badge tone="danger">সম্ভাব্য ডুপ্লিকেট — খুলে যাচাই করুন</Badge>
            ) : null}
            {grandfathered ? (
              <Badge tone="muted">বর্তমান সর্বনিম্নের নিচে</Badge>
            ) : null}
          </div>
          <p className="payout-card-meta">
            {payoutMethodLabel(payout.method)} · {payout.account_number_masked} ·{" "}
            {payout.account_name}
            <br />
            মালিক: {payout.owner_phone_masked} · অনুরোধ:{" "}
            {formatDhakaDateTime(payout.created_at)} ({formatAgeSeconds(age)} আগে)
            {payout.status === "approved" && payout.approved_at
              ? ` · অনুমোদিত: ${formatDhakaDateTime(payout.approved_at)}`
              : ""}
            {payout.status === "paid" && payout.paid_at
              ? ` · পরিশোধিত: ${formatDhakaDateTime(payout.paid_at)}`
              : ""}
          </p>
        </div>
        <div>
          <div className="payout-amount">
            <PriceTag amountPoisha={payout.amount_poisha} />
          </div>
          {inFlight && payout.covered !== null ? (
            payout.covered ? (
              <Badge tone="brand">কভারেজ আছে — Covered</Badge>
            ) : (
              <Badge tone="danger">
                ঘাটতি {formatBdt(-(payout.coverage_unclamped_poisha ?? 0))}
              </Badge>
            )
          ) : null}
        </div>
      </div>

      {payout.note ? (
        <p className="payout-card-meta">
          কারণ/নোট: {payout.note}
        </p>
      ) : null}
      {payout.status === "paid" && payout.payment_reference ? (
        <p className="payout-card-meta">রেফারেন্স: {payout.payment_reference}</p>
      ) : null}
      {conflicted ? (
        <p className="payout-warning" role="alert">
          এই পে-আউটটির অবস্থা বদলে গেছে — উপরের বার্তা দেখুন, তালিকা হালনাগাদ
          করা হয়েছে।
        </p>
      ) : null}

      <div className="payout-card-actions">
        <Button size="sm" onClick={onToggleDetail}>
          {expanded ? "বিস্তারিত লুকান — Hide" : "বিস্তারিত — Details"}
        </Button>
        {payout.status === "requested" ? (
          <>
            <Button
              size="sm"
              variant="primary"
              disabled={conflicted}
              onClick={() => onAction("approve")}
            >
              অনুমোদন করুন
            </Button>
            <Button
              size="sm"
              variant="danger"
              disabled={conflicted}
              onClick={() => onAction("reject")}
            >
              প্রত্যাখ্যান করুন
            </Button>
          </>
        ) : null}
        {payout.status === "approved" ? (
          <>
            <Button
              size="sm"
              variant="primary"
              disabled={conflicted}
              onClick={() => onAction("paid")}
            >
              পরিশোধিত চিহ্নিত করুন
            </Button>
            <Button
              size="sm"
              variant="danger"
              disabled={conflicted}
              onClick={() => onAction("reject")}
            >
              প্রত্যাখ্যান করুন
            </Button>
          </>
        ) : null}
      </div>

      {expanded ? (
        <div className="payout-detail">
          {detailLoading ? (
            <div className="admin-skel" aria-hidden="true">
              <Skeleton width="100%" height={18} />
              <Skeleton width="100%" height={18} />
              <Skeleton width="60%" height={18} />
            </div>
          ) : null}
          {detailError ? (
            <div className="admin-error">
              <p>বিস্তারিত লোড করা যায়নি — Couldn&apos;t load the detail.</p>
              <Button size="sm" onClick={onRetryDetail}>
                আবার চেষ্টা করুন — Retry
              </Button>
            </div>
          ) : null}
          {detail ? (
            <DetailBody detail={detail} settings={settings} onPrint={onPrint} />
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
  settings,
  onPrint,
}: {
  detail: AdminPayoutDetail;
  settings: AdminSettings | null;
  onPrint: (detail: AdminPayoutDetail) => void;
}) {
  const e = detail.earnings;
  const exposure = detail.returns_exposure;
  const gateBlocks =
    settings?.kyc_payout_gate && detail.store.verified_tier === "unverified";
  return (
    <div>
      {detail.store.status === "suspended" ? (
        <p className="payout-warning" role="alert">
          স্টোরটি সাসপেন্ডেড আছে — সাসপেন্ডেড থাকা অবস্থায় অনুমোদন ও
          পরিশোধিত-চিহ্নিত করা যাবে না (API 409 দেবে)। প্রত্যাখ্যান করা যাবে।
        </p>
      ) : null}
      {detail.account_diverged ? (
        <p className="payout-warning" role="alert">
          সতর্কতা: বিক্রেতা অনুরোধের পর অ্যাকাউন্ট বদলেছেন। পে-আউট সবসময়
          অনুরোধের সময়ের অ্যাকাউন্টেই (নিচের স্ন্যাপশট) যায় — বর্তমান
          অ্যাকাউন্টে পাঠাবেন না। সন্দেহ হলে আগে বিক্রেতার সঙ্গে নিশ্চিত
          করুন।
        </p>
      ) : null}
      {gateBlocks ? (
        <p className="payout-warning" role="alert">
          KYC গেট চালু আছে এবং স্টোরটি যাচাইকৃত নয় — অনুমোদন করতে হলে আগে
          স্টোর যাচাই সম্পন্ন হতে হবে (API 400 দেবে)।
        </p>
      ) : null}

      <h4>গন্তব্য — Destination (অনুরোধের স্ন্যাপশট)</h4>
      <ul className="admin-plain-list">
        <li>
          পদ্ধতি: {payoutMethodLabel(detail.method)} · নাম: {detail.account_name}
        </li>
        <li>
          অ্যাকাউন্ট নম্বর:{" "}
          <span className="payout-account-number">{detail.account_number}</span>
        </li>
        <li>মালিকের ফোন: {detail.owner_phone}</li>
        <li>
          স্টোর: {detail.store_name} (/{detail.store.slug}) · অবস্থা:{" "}
          {detail.store.status} · যাচাই স্তর: {detail.store.verified_tier}
        </li>
        {detail.current_account ? (
          <li>
            বর্তমান অ্যাকাউন্ট: {payoutMethodLabel(detail.current_account.method)}{" "}
            · <span className="payout-long-value">{detail.current_account.account_number}</span>{" "}
            · {detail.current_account.account_name}
          </li>
        ) : (
          <li>বর্তমান অ্যাকাউন্ট: সেট করা নেই</li>
        )}
      </ul>

      <h4>আয়ের হিসাব — Earnings behind this payout</h4>
      <ul className="admin-plain-list">
        <li>
          ডেলিভারি হওয়া অর্ডার থেকে আয়: <PriceTag amountPoisha={e.earned_poisha} />
        </li>
        <li>
          পরিশোধিত ও অনুমোদিত মিলিয়ে (committed):{" "}
          <PriceTag amountPoisha={e.paid_out_poisha} />
        </li>
        <li>
          অনুরোধে আটকে আছে (requested):{" "}
          <PriceTag amountPoisha={e.pending_poisha} />
        </li>
        <li>
          চার্জ (প্রমোশন/সাবস্ক্রিপশন, মালিকের সব স্টোর মিলিয়ে):{" "}
          <PriceTag amountPoisha={e.charges_total_poisha} />
        </li>
        <li>
          উপলব্ধ ব্যালেন্স: <PriceTag amountPoisha={e.available_poisha} />
          {detail.covered
            ? " — এই স্টোরের সব চলমান পে-আউটের কভারেজ আছে।"
            : ` — ঘাটতি: প্রকৃত হিসাব ${formatBdt(detail.unclamped_available_poisha)} (ঋণাত্মক মানে কভারেজ নেই)।`}
        </li>
      </ul>

      <h4>রিটার্ন ঝুঁকি — Returns exposure</h4>
      {exposure.in_flight_count === 0 &&
      exposure.received_refund_pending_count === 0 ? (
        <p className="payout-card-meta">কোনো রিটার্ন ঝুঁকি নেই।</p>
      ) : (
        <ul className="admin-plain-list">
          {exposure.in_flight_count > 0 ? (
            <li>
              প্রক্রিয়াধীন রিটার্ন: {formatCount(exposure.in_flight_count)}টি —
              গৃহীত হলে আয় থেকে{" "}
              <PriceTag amountPoisha={exposure.at_risk_payout_poisha} /> কমে যাবে
            </li>
          ) : null}
          {exposure.received_refund_pending_count > 0 ? (
            <li>
              গৃহীত রিটার্নের বাকি রিফান্ড:{" "}
              {formatCount(exposure.received_refund_pending_count)}টি —{" "}
              <PriceTag amountPoisha={exposure.received_refund_pending_poisha} />{" "}
              (এই টাকা ইতিমধ্যে আয়ের বাইরে; ক্রেতাকে ফেরত বাকি)
            </li>
          ) : null}
        </ul>
      )}

      <h4>সাম্প্রতিক চার্জ — Recent charges</h4>
      {detail.charges_recent.length === 0 ? (
        <p className="payout-card-meta">কোনো চার্জ নেই।</p>
      ) : (
        <>
          <ul className="admin-plain-list">
            {detail.charges_recent.map((charge) => (
              <li key={charge.id}>
                {charge.label}
                {charge.caption ? ` — ${charge.caption}` : ""}:{" "}
                <PriceTag amountPoisha={charge.amount_poisha} /> ·{" "}
                {formatDhakaDateTime(charge.created_at)}
              </li>
            ))}
          </ul>
          <p className="payout-card-meta">
            মোট চার্জ: <PriceTag amountPoisha={detail.charges_total_poisha} /> (
            {formatCount(detail.charges_count)}টি)
          </p>
        </>
      )}

      <h4>যাচাইকরণ — Verification</h4>
      <ul className="admin-plain-list">
        <li>
          অবস্থা: {detail.verification.status} · স্তর: {detail.verification.tier}
          {detail.verification.nid_masked
            ? ` · NID: ${detail.verification.nid_masked}`
            : ""}
        </li>
        {detail.verification.reviewer_note ? (
          <li>পর্যালোচকের নোট: {detail.verification.reviewer_note}</li>
        ) : null}
      </ul>

      <h4>সিদ্ধান্তের ইতিহাস — Decision record</h4>
      <ul className="admin-plain-list">
        <li>অনুরোধ: {formatDhakaDateTime(detail.created_at)}</li>
        {detail.approved_at ? (
          <li>অনুমোদিত: {formatDhakaDateTime(detail.approved_at)}</li>
        ) : null}
        {detail.paid_at ? (
          <li>পরিশোধিত: {formatDhakaDateTime(detail.paid_at)}</li>
        ) : null}
        {detail.rejected_at ? (
          <li>প্রত্যাখ্যাত: {formatDhakaDateTime(detail.rejected_at)}</li>
        ) : null}
        {detail.payment_reference ? (
          <li>পেমেন্ট রেফারেন্স: {detail.payment_reference}</li>
        ) : null}
        {detail.note ? <li>কারণ/নোট: {detail.note}</li> : null}
        {detail.idempotency_key ? (
          <li>
            আইডেমপোটেন্সি কী:{" "}
            <span className="payout-long-value">{detail.idempotency_key}</span>
          </li>
        ) : null}
        {detail.status === "requested" &&
        settings &&
        detail.amount_poisha < settings.min_payout_poisha ? (
          <li>
            এই অনুরোধটি বর্তমান সর্বনিম্নের (
            <PriceTag amountPoisha={settings.min_payout_poisha} />) নিচে —
            অনুরোধের সময়ের সর্বনিম্ন প্রযোজ্য।
          </li>
        ) : null}
      </ul>

      {detail.audit_entries.length > 0 ? (
        <>
          <h4>অডিট — Audit trail</h4>
          <ul className="admin-plain-list">
            {detail.audit_entries.map((entry) => (
              <li key={entry.id}>
                {entry.action} · {entry.admin_phone_masked} ·{" "}
                {formatDhakaDateTime(entry.created_at)}
                {typeof entry.detail?.note === "string"
                  ? ` · কারণ: ${entry.detail.note}`
                  : ""}
                {typeof entry.detail?.payment_reference === "string"
                  ? ` · রেফারেন্স: ${entry.detail.payment_reference}`
                  : ""}
              </li>
            ))}
          </ul>
        </>
      ) : null}

      {detail.store_payout_history.length > 0 ? (
        <>
          <h4>এই স্টোরের অন্য পে-আউট — Store history</h4>
          <ul className="admin-plain-list">
            {detail.store_payout_history.map((other) => (
              <li key={other.id}>
                <PriceTag amountPoisha={other.amount_poisha} /> ·{" "}
                {payoutStatusLabel(other.status)} ·{" "}
                {formatDhakaDateTime(other.created_at)}
                {detail.duplicate_candidate_ids.includes(other.id)
                  ? " · সম্ভাব্য ডুপ্লিকেট"
                  : ""}
              </li>
            ))}
          </ul>
        </>
      ) : null}

      {detail.status === "approved" || detail.status === "paid" ? (
        <Button size="sm" onClick={() => onPrint(detail)}>
          {detail.status === "approved"
            ? "পেমেন্ট নির্দেশনা প্রিন্ট করুন — Print instruction"
            : "পেমেন্ট রেকর্ড প্রিন্ট করুন — Print record"}
        </Button>
      ) : null}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Dialog bodies
// ---------------------------------------------------------------------------

function DialogBody({
  dialog,
  coverage,
  rejectReason,
  onRejectReason,
  paymentReference,
  onPaymentReference,
  onSwitchToReject,
}: {
  dialog: NonNullable<DialogState>;
  coverage: PayoutCoverageDetails | null;
  rejectReason: string;
  onRejectReason: (value: string) => void;
  paymentReference: string;
  onPaymentReference: (value: string) => void;
  onSwitchToReject: () => void;
}) {
  const { payout } = dialog;
  return (
    <div>
      <ul className="admin-plain-list">
        <li>
          পরিমাণ:{" "}
          <strong>
            <PriceTag amountPoisha={payout.amount_poisha} />
          </strong>
        </li>
        <li>স্টোর: {payout.store_name}</li>
        <li>
          গন্তব্য: {payoutMethodLabel(payout.method)} ·{" "}
          {payout.account_number_masked} · {payout.account_name}
        </li>
      </ul>

      {coverage ? (
        <div className="payout-warning">
          <p>
            ঘাটতি: <PriceTag amountPoisha={coverage.shortfall_poisha} /> — আয়{" "}
            <PriceTag amountPoisha={coverage.earned_poisha} />, পরিশোধিত{" "}
            <PriceTag amountPoisha={coverage.paid_out_poisha} />, অনুরোধে আটকে{" "}
            <PriceTag amountPoisha={coverage.pending_poisha} />, চার্জ{" "}
            <PriceTag amountPoisha={coverage.charges_total_poisha} />।
          </p>
          {dialog.kind !== "reject" ? (
            <Button size="sm" variant="danger" onClick={onSwitchToReject}>
              প্রত্যাখ্যানে যান — Reject instead
            </Button>
          ) : null}
        </div>
      ) : null}

      {dialog.kind === "approve" ? (
        <p>
          {payout.covered === false
            ? "সতর্কতা: বর্তমান হিসাবে এই স্টোরের কভারেজ নেই — অনুমোদন চেষ্টা করলে API ঘাটতির বিবরণসহ 409 দেবে।"
            : "অনুমোদন মানে শুধু অনুমোদন — টাকা এখনো পাঠানো হয়নি। বিক্রেতাকে অনুমোদনের জানানো হবে।"}
        </p>
      ) : null}

      {dialog.kind === "reject" ? (
        <div>
          <label htmlFor="payout-reject-reason">
            প্রত্যাখ্যানের কারণ (আবশ্যক) — Reason (required)
          </label>
          <textarea
            id="payout-reject-reason"
            value={rejectReason}
            onChange={(e) => onRejectReason(e.target.value)}
            rows={3}
            maxLength={MAX_REASON_CHARS}
            placeholder="যেমন: অ্যাকাউন্ট নম্বর মিলছে না — account details mismatch"
          />
          <p className="char-count">
            {rejectReason.trim().length} / {MAX_REASON_CHARS}
          </p>
          <p className="payout-card-meta">
            কারণটি বিক্রেতা দেখতে পাবেন এবং তাকে জানানো হবে। প্রত্যাখ্যান
            করলে টাকাটি বিক্রেতার উপলব্ধ ব্যালেন্সে ফিরে যাবে।
          </p>
        </div>
      ) : null}

      {dialog.kind === "paid" ? (
        <div>
          <p>
            আগে বিকাশ/ব্যাংকে টাকা পাঠান, তারপর রেফারেন্স দিয়ে চিহ্নিত
            করুন। ভুল করে পরিশোধিত চিহ্নিত করলে বিক্রেতা টাকা পেয়েছেন বলে
            জানানো হবে এবং রেকর্ড বদলানো যাবে না।
          </p>
          <label htmlFor="payout-payment-reference">
            পেমেন্ট রেফারেন্স (আবশ্যক) — Payment reference (required)
          </label>
          <input
            id="payout-payment-reference"
            type="text"
            value={paymentReference}
            onChange={(e) => onPaymentReference(e.target.value)}
            placeholder="বিকাশ/নগদ ট্রান্সাকশন নম্বর বা ব্যাংক রেফারেন্স"
          />
          <p className="payout-card-meta">
            রেফারেন্সটি পে-আউটে রেকর্ড হবে এবং বিক্রেতাকে জানানো হবে।
          </p>
        </div>
      ) : null}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Print view: the payment instruction / record
// ---------------------------------------------------------------------------

function PrintView({
  detail,
  onDone,
}: {
  detail: AdminPayoutDetail;
  onDone: () => void;
}) {
  useEffect(() => {
    const previousTitle = document.title;
    document.title = `payout-${detail.id.slice(0, 8)}`;
    window.print();
    return () => {
      document.title = previousTitle;
    };
  }, [detail.id]);

  const isPaid = detail.status === "paid";
  return (
    <div>
      <p className="no-print">
        <Button onClick={onDone}>← তালিকায় ফিরুন — Back to the queue</Button>
      </p>
      <article className="payout-instruction">
        <h2>
          {isPaid
            ? "পেমেন্ট রেকর্ড — Payment record"
            : "পেমেন্ট নির্দেশনা — Payment instruction"}
        </h2>
        <p>BD Marketplace — পে-আউট</p>
        <ul className="admin-plain-list">
          <li>স্টোর: {detail.store_name}</li>
          <li>
            পরিমাণ:{" "}
            <strong>
              <PriceTag amountPoisha={detail.amount_poisha} />
            </strong>
          </li>
          <li>পদ্ধতি: {payoutMethodLabel(detail.method)}</li>
          <li>অ্যাকাউন্টের নাম: {detail.account_name}</li>
          <li>
            অ্যাকাউন্ট নম্বর:{" "}
            <span className="payout-account-number">{detail.account_number}</span>
          </li>
          {isPaid ? (
            <>
              <li>পরিশোধিত: {formatDhakaDateTime(detail.paid_at)}</li>
              <li>রেফারেন্স: {detail.payment_reference ?? "—"}</li>
            </>
          ) : (
            <>
              <li>অনুমোদিত: {formatDhakaDateTime(detail.approved_at)}</li>
              <li>রেফারেন্স:</li>
              <li className="sign-line" aria-hidden="true" />
              <li>পাঠানোর সময়:</li>
              <li className="sign-line" aria-hidden="true" />
              <li>স্বাক্ষর:</li>
              <li className="sign-line" aria-hidden="true" />
            </>
          )}
        </ul>
      </article>
    </div>
  );
}
