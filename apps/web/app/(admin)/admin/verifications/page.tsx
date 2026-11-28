"use client";

import { Suspense, useCallback, useEffect, useState } from "react";
import Link from "next/link";
import { useSearchParams } from "next/navigation";
import AdminShell from "@/components/AdminShell";
import { Badge, Button, Dialog, EmptyState, Skeleton } from "@/components/ui";
import {
  approveVerification,
  getAdminSettings,
  getAdminVerificationDetail,
  getAdminVerificationFull,
  listAdminVerifications,
  rejectVerification,
  type AdminSettings,
  type AdminVerification,
  type AdminVerificationDetail,
  type AdminVerificationFull,
  type AdminVerificationSummary,
} from "@/lib/api";
import { useAuth } from "@/hooks/useAuth";
import { formatAgeSeconds, formatBdt, formatDhakaDateTime } from "@/lib/format";

const STATUS_FILTERS: { value: string; label: string }[] = [
  { value: "pending", label: "পর্যালোচনা বাকি" },
  { value: "approved", label: "অনুমোদিত" },
  { value: "rejected", label: "প্রত্যাখ্যাত" },
  { value: "all", label: "সব" },
];

const STATUS_LABELS: Record<string, string> = {
  pending: "পর্যালোচনা বাকি",
  approved: "অনুমোদিত",
  rejected: "প্রত্যাখ্যাত",
};

const TIER_LABELS: Record<string, string> = {
  unverified: "যাচাই হয়নি",
  basic: "যাচাইকৃত বিক্রেতা",
  verified: "যাচাইকৃত ব্যবসা",
};

const HISTORY_LABELS: Record<string, string> = {
  submitted: "আবেদন জমা",
  resubmitted: "আবেদন সংশোধন",
  approved: "অনুমোদিত",
  rejected: "প্রত্যাখ্যাত",
  viewed_full_nid: "সম্পূর্ণ NID দেখা হয়েছে",
};

/** Age highlights for the pending queue: waiting time is a
 * triage signal, never an "SLA" claim — no KYC SLA exists. */
const AGE_AMBER_SECS = 24 * 3600;
const AGE_DANGER_SECS = 72 * 3600;
/** How long a revealed NID stays on screen before it is
 * deleted from state (not merely hidden). */
const REVEAL_SECONDS = 30;
const PAGE_LIMIT = 20;

type RevealDialog = {
  kind: "reveal";
  item: AdminVerification;
  reason: string;
  full: AdminVerificationFull | null;
  loadingFull: boolean;
  secondsLeft: number;
};
type ApproveDialog = {
  kind: "approve";
  item: AdminVerification;
  tier: "basic" | "verified";
  override: boolean;
};
type RejectDialog = { kind: "reject"; item: AdminVerification; note: string };
type DialogState = RevealDialog | ApproveDialog | RejectDialog | null;

/**
 * Admin verification queue (Wave 21; rebuilt in the admin
 * verifications remake to the overview/payouts standard).
 *
 * The queue list is masked — owner phone and NID — because
 * triage does not need the full number, and the list payload
 * never carries it. The full NID exists in exactly one
 * place: inside the reveal dialog, fetched by the audited
 * POST (the admin's stated reason is written to the audit
 * log before the number is served), shown next to the
 * document image for REVEAL_SECONDS, and then DELETED from
 * state — closing the dialog, switching dialogs, changing
 * filters or letting the timer run out all destroy it. The
 * detail panel never fetches it.
 *
 * Decisions go through dialogs and carry the application
 * version the reviewer loaded (`expected_version`): if the
 * seller changed the application after the review, the API
 * answers 409 and the queue reloads. Approving at `verified`
 * needs a trade license on the application; approving with
 * no document image needs the explicit audited override.
 * Rejection REQUIRES a note: the seller sees it, gets it by
 * notification, and resubmits after fixing the problem.
 * Review is MANUAL — a platform human compares the number
 * against the document; no government database check exists
 * and nothing here claims one.
 */
export default function AdminVerificationsPage() {
  return (
    <AdminShell>
      <Suspense
        fallback={
          <div aria-busy="true">
            <Skeleton width="100%" height={72} />
            <Skeleton width="100%" height={72} />
            <Skeleton width="100%" height={72} />
          </div>
        }
      >
        <Verifications />
      </Suspense>
    </AdminShell>
  );
}

function Verifications() {
  const { token } = useAuth();
  const searchParams = useSearchParams();
  const initialStatus = (() => {
    const raw = searchParams.get("status");
    return STATUS_FILTERS.some((f) => f.value === raw) ? (raw as string) : "pending";
  })();

  const [items, setItems] = useState<AdminVerification[]>([]);
  const [total, setTotal] = useState(0);
  const [summary, setSummary] = useState<AdminVerificationSummary | null>(null);
  const [status, setStatus] = useState(initialStatus);
  const [page, setPage] = useState(1);
  const [qInput, setQInput] = useState("");
  const [q, setQ] = useState("");
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [stale, setStale] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  const [settings, setSettings] = useState<AdminSettings | null>(null);

  const [expandedId, setExpandedId] = useState<string | null>(null);
  const [detail, setDetail] = useState<AdminVerificationDetail | null>(null);
  const [detailError, setDetailError] = useState<string | null>(null);
  const [loadingDetailId, setLoadingDetailId] = useState<string | null>(null);

  const [dialog, setDialog] = useState<DialogState>(null);
  const [dialogError, setDialogError] = useState<string | null>(null);
  const [dialogBusy, setDialogBusy] = useState(false);

  const load = useCallback(async () => {
    if (!token) return;
    try {
      const list = await listAdminVerifications(token, {
        status,
        q: q || undefined,
        page,
        limit: PAGE_LIMIT,
      });
      setItems(list.items);
      setTotal(list.total);
      setSummary(list.summary);
      setLoadError(null);
      setStale(false);
    } catch (err) {
      const message =
        err instanceof Error
          ? err.message
          : "আবেদন লোড করা যায়নি। (Could not load applications.)";
      // A failed refresh keeps the rows on screen (marked
      // stale); only a failed FIRST load is a full error.
      setItems((current) => {
        if (current.length > 0) setStale(true);
        else setLoadError(message);
        return current;
      });
    } finally {
      setLoading(false);
    }
  }, [token, status, q, page]);

  useEffect(() => {
    void load();
  }, [load]);

  // The KYC gate setting is context, not a decision input
  // (the payouts page's pattern): fetched once per token,
  // failure is silent.
  useEffect(() => {
    if (!token) return;
    let cancelled = false;
    getAdminSettings(token)
      .then((value) => {
        if (!cancelled) setSettings(value);
      })
      .catch(() => {
        if (!cancelled) setSettings(null);
      });
    return () => {
      cancelled = true;
    };
  }, [token]);

  // Debounced search: typing re-queries from page 1.
  useEffect(() => {
    const handle = setTimeout(() => {
      setQ(qInput.trim());
      setPage(1);
    }, 300);
    return () => clearTimeout(handle);
  }, [qInput]);

  // Reveal countdown: while a full NID is on screen it ticks
  // down, and at zero the dialog closes — which deletes the
  // number from state entirely.
  useEffect(() => {
    if (!dialog || dialog.kind !== "reveal" || !dialog.full) return;
    if (dialog.secondsLeft <= 0) {
      setDialog(null);
      setNotice(
        "সময় শেষ — সম্পূর্ণ NID আবার লুকানো হয়েছে। (Time up — the full NID has been hidden again.)"
      );
      return;
    }
    const handle = setTimeout(() => {
      setDialog((current) =>
        current && current.kind === "reveal" && current.full
          ? { ...current, secondsLeft: current.secondsLeft - 1 }
          : current
      );
    }, 1000);
    return () => clearTimeout(handle);
  }, [dialog]);

  const loadDetail = useCallback(
    async (storeId: string) => {
      if (!token) return;
      setLoadingDetailId(storeId);
      setDetailError(null);
      try {
        setDetail(await getAdminVerificationDetail(token, storeId));
      } catch (err) {
        setDetailError(
          err instanceof Error
            ? err.message
            : "বিস্তারিত লোড করা যায়নি। (Could not load details.)"
        );
      } finally {
        setLoadingDetailId(null);
      }
    },
    [token]
  );

  function toggleDetail(item: AdminVerification) {
    if (expandedId === item.store_id) {
      setExpandedId(null);
      setDetail(null);
      return;
    }
    setExpandedId(item.store_id);
    setDetail(null);
    void loadDetail(item.store_id);
  }

  function openDialog(next: DialogState) {
    // Opening any dialog destroys whatever a previous reveal
    // held — the full NID never rides along into another flow.
    setDialogError(null);
    setDialog(next);
  }

  function closeDialog() {
    setDialog(null);
    setDialogError(null);
  }

  async function confirmDialog() {
    if (!token || !dialog) return;
    if (dialog.kind === "reveal") {
      if (dialog.full) {
        closeDialog();
        return;
      }
      const target = dialog;
      setDialog({ ...target, loadingFull: true });
      setDialogError(null);
      try {
        const full = await getAdminVerificationFull(
          token,
          target.item.store_id,
          target.reason.trim()
        );
        setDialog((current) =>
          current &&
          current.kind === "reveal" &&
          current.item.store_id === target.item.store_id
            ? { ...current, full, loadingFull: false, secondsLeft: REVEAL_SECONDS }
            : current
        );
      } catch (err) {
        setDialog((current) =>
          current &&
          current.kind === "reveal" &&
          current.item.store_id === target.item.store_id
            ? { ...current, loadingFull: false }
            : current
        );
        setDialogError(
          err instanceof Error ? err.message : "NID দেখা যায়নি। (Could not reveal the NID.)"
        );
      }
      return;
    }
    setDialogBusy(true);
    setDialogError(null);
    try {
      if (dialog.kind === "approve") {
        await approveVerification(
          token,
          dialog.item.store_id,
          dialog.tier,
          dialog.item.version,
          dialog.override
        );
        setNotice(
          `“${dialog.item.store_name}” অনুমোদিত হয়েছে (${TIER_LABELS[dialog.tier]})। বিক্রেতাকে জানানো হয়েছে। (Approved — the seller has been notified.)`
        );
      } else {
        await rejectVerification(
          token,
          dialog.item.store_id,
          dialog.note.trim(),
          dialog.item.version
        );
        setNotice(
          `“${dialog.item.store_name}” প্রত্যাখ্যাত হয়েছে। কারণসহ বিক্রেতাকে জানানো হয়েছে। (Rejected — the seller has been notified with the reason.)`
        );
      }
      closeDialog();
      setExpandedId(null);
      setDetail(null);
      await load();
    } catch (err) {
      setDialogError(
        err instanceof Error
          ? err.message
          : dialog.kind === "approve"
            ? "অনুমোদন করা যায়নি। (Could not approve.)"
            : "প্রত্যাখ্যান করা যায়নি। (Could not reject.)"
      );
      // A 409 here usually means the application changed
      // under the review — refresh the queue behind the
      // dialog so the reload the error asks for is real.
      void load();
    } finally {
      setDialogBusy(false);
    }
  }

  const totalPages = Math.max(1, Math.ceil(total / PAGE_LIMIT));
  const summaryCount = (value: string): number => {
    if (!summary) return 0;
    if (value === "pending") return summary.pending;
    if (value === "approved") return summary.approved;
    if (value === "rejected") return summary.rejected;
    return summary.pending + summary.approved + summary.rejected;
  };

  return (
    <div>
      <div className="admin-overview-head">
        <h2>যাচাইকরণ — Verification queue</h2>
        <Button
          size="sm"
          onClick={() => {
            setLoading(true);
            void load();
          }}
        >
          রিফ্রেশ — Refresh
        </Button>
      </div>
      <p className="admin-muted">
        প্ল্যাটফর্ম দল ডকুমেন্ট দেখে হাতে যাচাই করে — কোনো সরকারি ডাটাবেস যাচাই নেই। (Manual
        review by the platform team against the submitted documents — there is no government
        database check.)
      </p>

      {settings?.kyc_payout_gate ? (
        <p className="payout-warning" role="alert">
          KYC গেট চালু আছে: যাচাই ছাড়া স্টোর পে-আউট নিতে পারবে না — এই কিউয়ের সিদ্ধান্ত সরাসরি
          পে-আউটে প্রভাব ফেলে। (The KYC payout gate is ON: unverified stores cannot take payouts,
          so this queue is on the money path.){" "}
          <Link href="/admin/settings">সেটিংস — Settings</Link>
        </p>
      ) : null}

      {notice ? (
        <p role="status" className="admin-muted">
          {notice}
        </p>
      ) : null}
      {stale ? (
        <p role="alert" className="admin-error">
          নতুন তথ্য লোড করা যায়নি — নিচের তালিকাটি আগের অবস্থা দেখাচ্ছে। (Refresh failed — the
          list below may be stale.)
        </p>
      ) : null}

      <div
        className="pipeline-row"
        role="group"
        aria-label="অবস্থা অনুযায়ী ছাঁকুন — Filter by status"
      >
        {STATUS_FILTERS.map((filter) => (
          <button
            key={filter.value}
            type="button"
            className="btn btn--sm"
            aria-pressed={status === filter.value}
            onClick={() => {
              setStatus(filter.value);
              setPage(1);
            }}
          >
            {filter.label} ({summaryCount(filter.value)})
          </button>
        ))}
        <input
          type="search"
          value={qInput}
          onChange={(event) => setQInput(event.target.value)}
          placeholder="স্টোর, আবেদনকারী বা ফোন খুঁজুন — Search store, applicant, phone"
          aria-label="খুঁজুন — Search"
        />
      </div>

      {loading ? (
        <div aria-busy="true">
          <Skeleton width="100%" height={72} />
          <Skeleton width="100%" height={72} />
          <Skeleton width="100%" height={72} />
        </div>
      ) : loadError ? (
        <div>
          <p role="alert" className="admin-error">
            {loadError}
          </p>
          <Button
            size="sm"
            onClick={() => {
              setLoading(true);
              void load();
            }}
          >
            আবার চেষ্টা করুন — Retry
          </Button>
        </div>
      ) : items.length === 0 ? (
        <EmptyState title="কোনো আবেদন নেই — No applications">
          <p>
            এই ছাঁকনিতে কোনো যাচাইকরণ আবেদন পাওয়া যায়নি। (No verification applications match
            this filter.)
          </p>
        </EmptyState>
      ) : (
        <>
          <div className="queue-list">
            {items.map((item) => (
              <QueueCard
                key={item.store_id}
                item={item}
                expanded={expandedId === item.store_id}
                detail={expandedId === item.store_id ? detail : null}
                detailError={expandedId === item.store_id ? detailError : null}
                detailLoading={loadingDetailId === item.store_id}
                onToggleDetail={() => toggleDetail(item)}
                onRetryDetail={() => void loadDetail(item.store_id)}
                onReveal={() =>
                  openDialog({
                    kind: "reveal",
                    item,
                    reason: "",
                    full: null,
                    loadingFull: false,
                    secondsLeft: REVEAL_SECONDS,
                  })
                }
                onApprove={() =>
                  openDialog({ kind: "approve", item, tier: "basic", override: false })
                }
                onReject={() => openDialog({ kind: "reject", item, note: "" })}
              />
            ))}
          </div>
          <div className="pipeline-row">
            <button
              type="button"
              className="btn btn--sm"
              disabled={page <= 1}
              onClick={() => setPage((current) => Math.max(1, current - 1))}
            >
              আগের পৃষ্ঠা — Previous
            </button>
            <span className="admin-muted">
              পৃষ্ঠা {page} / {totalPages} · মোট {total}
            </span>
            <button
              type="button"
              className="btn btn--sm"
              disabled={page >= totalPages}
              onClick={() => setPage((current) => Math.min(totalPages, current + 1))}
            >
              পরের পৃষ্ঠা — Next
            </button>
          </div>
        </>
      )}

      {dialog ? (
        <Dialog
          open
          onClose={closeDialog}
          title={
            dialog.kind === "reveal"
              ? `সম্পূর্ণ NID — ${dialog.item.store_name}`
              : dialog.kind === "approve"
                ? `অনুমোদন করুন — ${dialog.item.store_name}`
                : `প্রত্যাখ্যান করুন — ${dialog.item.store_name}`
          }
          variant={dialog.kind === "reject" ? "danger" : "default"}
          busy={dialogBusy}
          error={dialogError}
          onConfirm={() => void confirmDialog()}
          confirmLabel={
            dialog.kind === "reveal"
              ? dialog.full
                ? "বন্ধ করুন — Close"
                : "কারণসহ NID দেখুন — Reveal with reason"
              : dialog.kind === "approve"
                ? "অনুমোদন নিশ্চিত করুন — Confirm approval"
                : "প্রত্যাখ্যান নিশ্চিত করুন — Confirm rejection"
          }
          confirmDisabled={
            dialogBusy ||
            (dialog.kind === "reveal" &&
              !dialog.full &&
              (dialog.loadingFull || dialog.reason.trim().length < 4)) ||
            (dialog.kind === "reject" &&
              (dialog.note.trim().length === 0 ||
                Array.from(dialog.note.trim()).length > 500))
          }
          initialFocus={dialog.kind === "reveal" ? "textarea" : undefined}
        >
          {dialog.kind === "reveal" ? (
            <RevealBody
              dialog={dialog}
              onReason={(reason) =>
                setDialog((current) =>
                  current && current.kind === "reveal" ? { ...current, reason } : current
                )
              }
            />
          ) : dialog.kind === "approve" ? (
            <ApproveBody
              dialog={dialog}
              onTier={(tier) =>
                setDialog((current) =>
                  current && current.kind === "approve" ? { ...current, tier } : current
                )
              }
              onOverride={(override) =>
                setDialog((current) =>
                  current && current.kind === "approve" ? { ...current, override } : current
                )
              }
            />
          ) : (
            <RejectBody
              dialog={dialog}
              onNote={(note) =>
                setDialog((current) =>
                  current && current.kind === "reject" ? { ...current, note } : current
                )
              }
            />
          )}
        </Dialog>
      ) : null}
    </div>
  );
}

function ageSeconds(iso: string): number {
  const parsed = Date.parse(iso);
  if (Number.isNaN(parsed)) return 0;
  return Math.max(0, Math.floor((Date.now() - parsed) / 1000));
}

function QueueCard({
  item,
  expanded,
  detail,
  detailError,
  detailLoading,
  onToggleDetail,
  onRetryDetail,
  onReveal,
  onApprove,
  onReject,
}: {
  item: AdminVerification;
  expanded: boolean;
  detail: AdminVerificationDetail | null;
  detailError: string | null;
  detailLoading: boolean;
  onToggleDetail: () => void;
  onRetryDetail: () => void;
  onReveal: () => void;
  onApprove: () => void;
  onReject: () => void;
}) {
  const age = ageSeconds(item.submitted_at);
  const longWaiting = item.status === "pending" && age >= AGE_AMBER_SECS;
  const veryLongWaiting = item.status === "pending" && age >= AGE_DANGER_SECS;
  return (
    <article className="payout-card">
      <div className="queue-row-main">
        <strong>{item.store_name}</strong>{" "}
        <span className="admin-muted">
          {item.store_slug} · মালিক {item.owner_phone_masked}
        </span>
      </div>
      <div className="pipeline-row">
        <Badge tone={item.status === "pending" ? "accent" : item.status === "approved" ? "brand" : "danger"}>
          {STATUS_LABELS[item.status] ?? item.status}
        </Badge>
        <Badge tone={item.tier === "unverified" ? "muted" : "brand"}>
          {TIER_LABELS[item.tier] ?? item.tier}
        </Badge>
        {item.store_status === "suspended" ? <Badge tone="danger">স্টোর সাসপেন্ডেড</Badge> : null}
        {item.has_trade_license ? (
          <Badge tone="accent">ট্রেড লাইসেন্স আছে</Badge>
        ) : (
          <Badge tone="muted">ট্রেড লাইসেন্স নেই</Badge>
        )}
        {item.document_url ? null : <Badge tone="muted">ডকুমেন্ট নেই</Badge>}
        {longWaiting ? (
          <Badge tone={veryLongWaiting ? "danger" : "accent"}>
            অনেকক্ষণ অপেক্ষমাণ — {formatAgeSeconds(age)}
          </Badge>
        ) : null}
      </div>
      <p className="payout-card-meta">
        NID {item.nid_masked}
        {item.applicant_name ? ` · আবেদনকারী: ${item.applicant_name}` : ""}
        {" · "}জমা: {formatDhakaDateTime(item.submitted_at)} ({formatAgeSeconds(age)} আগে)
        {item.prior_rejections > 0 ? ` · আগে ${item.prior_rejections} বার প্রত্যাখ্যাত` : ""}
        {item.pending_payout_count > 0
          ? ` · পে-আউট অপেক্ষমাণ: ${item.pending_payout_count}টি (${formatBdt(item.pending_payout_poisha)})`
          : ""}
      </p>
      {item.duplicate_nid_stores > 0 || item.duplicate_license_stores > 0 ? (
        <p className="payout-warning" role="alert">
          {item.duplicate_nid_stores > 0
            ? `এই NID অন্য মালিকের ${item.duplicate_nid_stores}টি স্টোরেও আছে। `
            : ""}
          {item.duplicate_license_stores > 0
            ? `এই ট্রেড লাইসেন্স অন্য মালিকের ${item.duplicate_license_stores}টি স্টোরেও আছে।`
            : ""}
        </p>
      ) : null}
      {item.status === "rejected" && item.reviewer_note ? (
        <p className="payout-card-meta">প্রত্যাখ্যানের কারণ: {item.reviewer_note}</p>
      ) : null}
      <div className="pipeline-row">
        <button
          type="button"
          className="btn btn--sm"
          aria-label={`বিস্তারিত — ${item.store_name}`}
          aria-expanded={expanded}
          onClick={onToggleDetail}
        >
          বিস্তারিত
        </button>
        <button
          type="button"
          className="btn btn--sm"
          aria-label={`সম্পূর্ণ NID দেখুন — ${item.store_name}`}
          onClick={onReveal}
        >
          সম্পূর্ণ NID দেখুন
        </button>
        {item.status === "pending" ? (
          <>
            <button
              type="button"
              className="btn btn--sm"
              aria-label={`অনুমোদন — ${item.store_name}`}
              onClick={onApprove}
            >
              অনুমোদন
            </button>
            <button
              type="button"
              className="btn btn--sm"
              aria-label={`প্রত্যাখ্যান — ${item.store_name}`}
              onClick={onReject}
            >
              প্রত্যাখ্যান
            </button>
          </>
        ) : null}
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
                {detailError}
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

function DetailBody({ detail }: { detail: AdminVerificationDetail }) {
  return (
    <div>
      <h4>আবেদন — Application</h4>
      <ul className="admin-plain-list">
        <li>
          NID (মাস্কড): <span className="payout-long-value">{detail.nid_masked}</span>
        </li>
        <li>আবেদনকারী (NID অনুযায়ী): {detail.applicant_name ?? "—"}</li>
        <li>ট্রেড লাইসেন্স: {detail.has_trade_license ? "আছে (নম্বরটি সম্পূর্ণ NID ভিউতে)" : "নেই"}</li>
        <li>জমা: {formatDhakaDateTime(detail.submitted_at)}</li>
        {detail.reviewed_at ? <li>সিদ্ধান্ত: {formatDhakaDateTime(detail.reviewed_at)}</li> : null}
        {detail.reviewer_note ? <li>পর্যালোচকের নোট: {detail.reviewer_note}</li> : null}
      </ul>
      {detail.document_url ? (
        <>
          <p className="admin-muted">
            ডকুমেন্টের ছবি (বিক্রেতার দেওয়া বাইরের লিংক — সতর্ক হয়ে খুলুন):
          </p>
          {/* Seller-supplied external URL: no referrer, and a
              plain link fallback if the image fails. */}
          {/* eslint-disable-next-line @next/next/no-img-element */}
          <img
            src={detail.document_url}
            alt="যাচাইকরণ ডকুমেন্ট — Verification document"
            referrerPolicy="no-referrer"
            style={{ maxWidth: "100%", maxHeight: 320, objectFit: "contain" }}
            onError={(event) => {
              event.currentTarget.style.display = "none";
            }}
          />
          <p>
            <a href={detail.document_url} target="_blank" rel="noreferrer">
              ডকুমেন্ট নতুন ট্যাবে খুলুন — Open document
            </a>
          </p>
        </>
      ) : (
        <p className="payout-card-meta">কোনো ডকুমেন্টের ছবি জমা পড়েনি।</p>
      )}

      <h4>স্টোরের চিত্র — Store context</h4>
      <ul className="admin-plain-list">
        <li>স্টোর তৈরি: {formatDhakaDateTime(detail.store_created_at)}</li>
        <li>সক্রিয় পণ্য: {detail.product_count}</li>
        <li>
          ডেলিভার্ড অর্ডার: {detail.delivered_orders} · GMV {formatBdt(detail.delivered_gmv_poisha)}
        </li>
        <li>রিটার্ন গৃহীত: {detail.returns_received}</li>
        <li>
          পে-আউট অপেক্ষমাণ: {detail.pending_payout_count}টি ({formatBdt(detail.pending_payout_poisha)})
        </li>
      </ul>

      <h4>মালিকের অন্য স্টোর — Owner&apos;s stores</h4>
      {detail.owner_stores.length <= 1 ? (
        <p className="payout-card-meta">এই মালিকের আর কোনো স্টোর নেই।</p>
      ) : (
        <ul className="admin-plain-list">
          {detail.owner_stores.map((store) => (
            <li key={store.store_id}>
              {store.store_name} ({store.store_slug}) — {TIER_LABELS[store.tier] ?? store.tier} ·{" "}
              {STATUS_LABELS[store.verification_status] ?? store.verification_status}
            </li>
          ))}
        </ul>
      )}

      <h4>ইতিহাস — History</h4>
      {detail.history.length === 0 ? (
        <p className="payout-card-meta">কোনো ইতিহাস পাওয়া যায়নি।</p>
      ) : (
        <ul className="admin-plain-list">
          {detail.history.map((entry, index) => (
            <li key={`${entry.at}-${index}`}>
              {HISTORY_LABELS[entry.kind] ?? entry.kind} — {formatDhakaDateTime(entry.at)}
              {entry.actor_phone_masked ? ` · ${entry.actor_phone_masked}` : ""}
              {entry.tier ? ` · স্তর: ${TIER_LABELS[entry.tier] ?? entry.tier}` : ""}
              {entry.note ? ` · নোট: ${entry.note}` : ""}
              {entry.reason ? ` · কারণ: ${entry.reason}` : ""}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

function RevealBody({
  dialog,
  onReason,
}: {
  dialog: RevealDialog;
  onReason: (reason: string) => void;
}) {
  const { item, full } = dialog;
  if (full) {
    return (
      <div>
        <p className="admin-muted" role="status">
          এই দেখাটি অডিট লগে লেখা হয়েছে। {dialog.secondsLeft} সেকেন্ড পর NID স্বয়ংক্রিয়ভাবে
          লুকিয়ে যাবে। (This view has been written to the audit log. The NID hides automatically
          in {dialog.secondsLeft}s.)
        </p>
        <ul className="admin-plain-list">
          <li>
            সম্পূর্ণ NID:{" "}
            <strong className="payout-long-value">{full.nid_number}</strong>
          </li>
          <li>আবেদনকারী: {full.applicant_name ?? item.applicant_name ?? "—"}</li>
          <li>
            ট্রেড লাইসেন্স: {full.trade_license_number ?? "নেই"}
          </li>
          <li>মাস্কড রূপ: {item.nid_masked}</li>
        </ul>
        {item.document_url ? (
          <>
            {/* eslint-disable-next-line @next/next/no-img-element */}
            <img
              src={item.document_url}
              alt="যাচাইকরণ ডকুমেন্ট — Verification document"
              referrerPolicy="no-referrer"
              style={{ maxWidth: "100%", maxHeight: 320, objectFit: "contain" }}
              onError={(event) => {
                event.currentTarget.style.display = "none";
              }}
            />
            <p>
              <a href={item.document_url} target="_blank" rel="noreferrer">
                ডকুমেন্ট নতুন ট্যাবে খুলুন — Open document
              </a>
            </p>
          </>
        ) : (
          <p className="payout-warning" role="alert">
            এই আবেদনে কোনো ডকুমেন্টের ছবি নেই — নম্বরটি মেলানোর কিছু নেই। (This application has
            no document image to compare against.)
          </p>
        )}
      </div>
    );
  }
  return (
    <div>
      <p>
        সম্পূর্ণ NID দেখতে কারণ লিখুন। কারণটি অডিট লগে স্থায়ীভাবে লেখা থাকবে — কারণে কোনো NID
        বা দীর্ঘ নম্বর লিখবেন না। (State why you need the full NID. The reason is written to the
        audit log permanently — never paste an NID or other long number into it.)
      </p>
      <label>
        কারণ — Reason
        <textarea
          value={dialog.reason}
          onChange={(event) => onReason(event.target.value)}
          rows={3}
          placeholder="যেমন: ডকুমেন্টের ছবির সঙ্গে নম্বর মেলানো"
          aria-describedby="reveal-reason-hint"
        />
      </label>
      <p id="reveal-reason-hint" className="admin-muted">
        কমপক্ষে ৪ অক্ষর। (At least 4 characters.)
      </p>
      {dialog.loadingFull ? <p className="admin-muted">লোড হচ্ছে…</p> : null}
    </div>
  );
}

function ApproveBody({
  dialog,
  onTier,
  onOverride,
}: {
  dialog: ApproveDialog;
  onTier: (tier: "basic" | "verified") => void;
  onOverride: (override: boolean) => void;
}) {
  const { item } = dialog;
  const verifiedDisabled = !item.has_trade_license;
  return (
    <div>
      {item.store_status === "suspended" ? (
        <p className="payout-warning" role="alert">
          স্টোরটি সাসপেন্ডেড — সক্রিয় না করা পর্যন্ত অনুমোদন করা যাবে না। (This store is
          suspended — approval is blocked until it is reactivated.)
        </p>
      ) : null}
      <fieldset>
        <legend>স্তর — Tier</legend>
        <label>
          <input
            type="radio"
            name="verification-tier"
            checked={dialog.tier === "basic"}
            onChange={() => onTier("basic")}
          />{" "}
          basic — যাচাইকৃত বিক্রেতা (NID অনুমোদিত)
        </label>
        <label>
          <input
            type="radio"
            name="verification-tier"
            checked={dialog.tier === "verified"}
            disabled={verifiedDisabled}
            onChange={() => onTier("verified")}
            aria-describedby="verified-tier-hint"
          />{" "}
          verified — যাচাইকৃত ব্যবসা (NID + ট্রেড লাইসেন্স)
        </label>
        <p id="verified-tier-hint" className="admin-muted">
          {verifiedDisabled
            ? "এই আবেদনে ট্রেড লাইসেন্স নেই — verified স্তর দেওয়া যাবে না। (No trade license on this application — the verified tier is unavailable.)"
            : "ট্রেড লাইসেন্স আবেদনে আছে। (A trade license is on the application.)"}
        </p>
      </fieldset>
      {!item.document_url ? (
        <label>
          <input
            type="checkbox"
            checked={dialog.override}
            onChange={(event) => onOverride(event.target.checked)}
          />{" "}
          কোনো ডকুমেন্টের ছবি নেই — তবু অনুমোদন করছি (এই ওভাররাইড অডিট লগে লেখা থাকবে)। (No
          document image — approve anyway; this override is recorded in the audit log.)
        </label>
      ) : null}
      {!item.document_url && !dialog.override ? (
        <p className="admin-muted">
          ডকুমেন্ট ছাড়া অনুমোদন করতে উপরের ওভাররাইডটি চালু করুন, অথবা API 400 ফেরত দেবে। (Without
          the override, the API refuses a document-less approval with a 400.)
        </p>
      ) : null}
    </div>
  );
}

function RejectBody({
  dialog,
  onNote,
}: {
  dialog: RejectDialog;
  onNote: (note: string) => void;
}) {
  const count = Array.from(dialog.note.trim()).length;
  return (
    <div>
      <p>
        কারণটি বিক্রেতা নিজের যাচাইকরণ পেজে দেখবেন এবং নোটিফিকেশনেও পাবেন — কী ঠিক করে আবার আবেদন
        করতে হবে তা স্পষ্ট করে লিখুন। কারণে সম্পূর্ণ NID নম্বর লিখবেন না। (The seller reads this
        note on their verification page and in their notification — write what to fix, and never
        include the full NID number.)
      </p>
      <label>
        প্রত্যাখ্যানের কারণ — Reason (আবশ্যক)
        <textarea
          value={dialog.note}
          onChange={(event) => onNote(event.target.value)}
          rows={4}
          placeholder="যেমন: NID-র ছবি ঝাপসা — পরিষ্কার ছবি দিয়ে আবার আবেদন করুন"
          aria-describedby="reject-note-count"
        />
      </label>
      <p id="reject-note-count" className="admin-muted">
        {count} / 500 অক্ষর
      </p>
    </div>
  );
}
