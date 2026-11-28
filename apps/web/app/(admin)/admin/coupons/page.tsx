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
  couponStatusLabel,
  createAdminCoupon,
  getAdminCoupon,
  listAdminCoupons,
  orderStatusLabel,
  setAdminCouponActive,
  updateAdminCouponTerms,
  type AdminCoupon,
  type AdminCouponDetail,
  type AdminCouponList,
  type CreateCouponInput,
  type UpdateCouponTermsInput,
} from "@/lib/api";
import { formatCount, formatDhakaDateTime } from "@/lib/format";
import { useAuth } from "@/hooks/useAuth";

const PAGE_LIMIT = 25;
const MAX_REASON_CHARS = 500;

const STATUS_FILTERS: { value: string; label: string }[] = [
  { value: "", label: "সব — All" },
  { value: "live", label: "চলমান — Live" },
  { value: "scheduled", label: "নির্ধারিত — Scheduled" },
  { value: "expired", label: "মেয়াদ শেষ — Expired" },
  { value: "exhausted", label: "সীমা শেষ — Exhausted" },
  { value: "inactive", label: "বন্ধ — Inactive" },
];

const SCOPE_FILTERS: { value: string; label: string }[] = [
  { value: "", label: "সব ধরন — All scopes" },
  { value: "platform", label: "প্ল্যাটফর্ম — Platform" },
  { value: "store", label: "স্টোর — Store" },
];

const TYPE_FILTERS: { value: string; label: string }[] = [
  { value: "", label: "সব ছাড় — All types" },
  { value: "percent", label: "শতাংশ — Percent" },
  { value: "fixed", label: "নির্দিষ্ট টাকা — Fixed" },
];

const SORT_OPTIONS: { value: string; label: string }[] = [
  { value: "newest", label: "সবচেয়ে নতুন আগে — Newest first" },
  { value: "oldest", label: "সবচেয়ে পুরনো আগে — Oldest first" },
  { value: "code_asc", label: "কোড অনুযায়ী — Code A–Z" },
  { value: "most_used", label: "বেশি ব্যবহৃত আগে — Most used" },
  { value: "value_desc", label: "বড় ছাড় আগে — Value: high to low" },
];

const VALID_STATUSES = ["live", "scheduled", "expired", "exhausted", "inactive"];

function statusTone(status: string): "brand" | "accent" | "muted" | "danger" {
  switch (status) {
    case "live":
      return "brand";
    case "scheduled":
      return "accent";
    case "inactive":
      return "danger";
    default:
      return "muted";
  }
}

/** ISO instant → `datetime-local` input value (browser-local wall time). */
function toLocalInput(iso: string | null): string {
  if (!iso) return "";
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return "";
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(
    date.getDate()
  )}T${pad(date.getHours())}:${pad(date.getMinutes())}`;
}

/** `datetime-local` value → ISO instant, or null when empty/invalid. */
function fromLocalInput(value: string): string | null {
  if (!value) return null;
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? null : date.toISOString();
}

function takaToPoisha(value: string): number | null {
  const parsed = Number.parseFloat(value);
  if (!Number.isFinite(parsed)) return null;
  return Math.round(parsed * 100);
}

/**
 * Admin coupons — the coupon moderation queue, remade after
 * the surface analysis of the admin remake program (eighth
 * surface). What an admin can do here, and what the API
 * guarantees behind it:
 *
 * - See EVERY coupon — platform and store coupons alike —
 *   with its derived lifecycle status (চলমান / নির্ধারিত /
 *   মেয়াদ শেষ / সীমা শেষ / বন্ধ), its terms, and its usage.
 *   The pre-remake page showed a bare newest-50 list with no
 *   status, no store name and no usage at all.
 * - Create platform coupons (audited). A platform coupon
 *   discounts the whole order's goods and the PLATFORM
 *   bears the cost — commission is still charged on the
 *   full goods. A store coupon's discount comes out of
 *   that store's own goods and its commission base.
 * - Deactivate ANY coupon, with a REQUIRED reason: the
 *   reason is stored on the coupon, written to the audit
 *   log, and — for a store coupon — sent to the store
 *   owner. Reactivation clears the stamp, and the audit
 *   row keeps the outgoing reason. Setting the state a
 *   coupon is already in is a 409 — this page freezes the
 *   row and reloads.
 * - Edit a platform coupon's terms ONLY while it has
 *   never been redeemed. Orders snapshot the discount
 *   amount, never the terms, so after a first redemption
 *   the terms are locked: deactivate and create a new
 *   code. A store coupon's terms belong to its seller —
 *   the admin lever there is deactivation, not rewriting.
 *
 * Honest semantics the copy keeps visible: a cart does NOT
 * snapshot a coupon. Checkout re-checks the code against
 * the live coupon row, so deactivating (or editing an
 * unredeemed coupon's terms) changes what a buyer holding
 * the code will get — a deactivated code FAILS that
 * checkout until the buyer removes it. Coupons are never
 * deleted from this surface: deactivation is the
 * reversible equivalent, and a redeemed coupon's record
 * is the only proof of what it granted.
 */
export default function AdminCouponsPage() {
  return (
    <AdminShell>
      <Suspense fallback={<p>লোড হচ্ছে…</p>}>
        <Coupons />
      </Suspense>
    </AdminShell>
  );
}

type DialogState =
  | { kind: "create" }
  | { kind: "deactivate"; coupon: AdminCoupon }
  | { kind: "reactivate"; coupon: AdminCoupon }
  | { kind: "terms"; coupon: AdminCoupon }
  | null;

interface TermsFormState {
  code: string;
  discountType: string;
  discountValue: string;
  minOrderTaka: string;
  capTaka: string;
  usageLimit: string;
  startsAt: string;
  endsAt: string;
}

function emptyTermsForm(): TermsFormState {
  return {
    code: "",
    discountType: "percent",
    discountValue: "",
    minOrderTaka: "",
    capTaka: "",
    usageLimit: "",
    startsAt: "",
    endsAt: "",
  };
}

function termsFormFromCoupon(coupon: AdminCoupon): TermsFormState {
  return {
    code: coupon.code,
    discountType: coupon.discount_type,
    discountValue:
      coupon.discount_type === "percent"
        ? String(coupon.discount_value)
        : String(coupon.discount_value / 100),
    minOrderTaka:
      coupon.min_order_poisha > 0 ? String(coupon.min_order_poisha / 100) : "",
    capTaka:
      coupon.max_discount_poisha !== null
        ? String(coupon.max_discount_poisha / 100)
        : "",
    usageLimit: coupon.usage_limit !== null ? String(coupon.usage_limit) : "",
    startsAt: toLocalInput(coupon.starts_at),
    endsAt: toLocalInput(coupon.ends_at),
  };
}

/** Build the shared terms payload from the form, or an error message. */
function buildTerms(
  form: TermsFormState
): { ok: true; terms: UpdateCouponTermsInput } | { ok: false; error: string } {
  const isPercent = form.discountType === "percent";
  let discountValue: number | null;
  if (isPercent) {
    discountValue = Number.parseInt(form.discountValue, 10);
    if (!Number.isFinite(discountValue)) discountValue = null;
  } else {
    discountValue = takaToPoisha(form.discountValue);
  }
  if (discountValue === null) {
    return {
      ok: false,
      error: "ছাড়ের পরিমাণ ঠিকমতো লিখুন — enter a valid discount amount.",
    };
  }
  const minOrder =
    form.minOrderTaka.trim() === "" ? 0 : takaToPoisha(form.minOrderTaka);
  if (minOrder === null || minOrder < 0) {
    return {
      ok: false,
      error: "সর্বনিম্ন অর্ডারের পরিমাণ ঠিকমতো লিখুন — enter a valid minimum order.",
    };
  }
  let cap: number | null = null;
  if (isPercent && form.capTaka.trim() !== "") {
    cap = takaToPoisha(form.capTaka);
    if (cap === null || cap <= 0) {
      return {
        ok: false,
        error: "সর্বোচ্চ ছাড়ের সীমা ঠিকমতো লিখুন — enter a valid discount cap.",
      };
    }
  }
  let usageLimit: number | null = null;
  if (form.usageLimit.trim() !== "") {
    usageLimit = Number.parseInt(form.usageLimit, 10);
    if (!Number.isFinite(usageLimit) || usageLimit <= 0) {
      return {
        ok: false,
        error: "ব্যবহারের সীমা ঠিকমতো লিখুন — enter a valid usage limit.",
      };
    }
  }
  return {
    ok: true,
    terms: {
      discount_type: form.discountType,
      discount_value: discountValue,
      min_order_poisha: minOrder,
      max_discount_poisha: cap,
      starts_at: fromLocalInput(form.startsAt),
      ends_at: fromLocalInput(form.endsAt),
      usage_limit: usageLimit,
    },
  };
}

function Coupons() {
  const { token } = useAuth();
  const searchParams = useSearchParams();
  const initialStatus = (() => {
    const param = searchParams.get("status") ?? "";
    return VALID_STATUSES.includes(param) ? param : "";
  })();

  const [status, setStatus] = useState(initialStatus);
  const [scope, setScope] = useState("");
  const [discountType, setDiscountType] = useState("");
  const [sort, setSort] = useState("newest");
  const [qInput, setQInput] = useState("");
  const [q, setQ] = useState("");
  const [page, setPage] = useState(1);

  const [list, setList] = useState<AdminCouponList | null>(null);
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
  const [detail, setDetail] = useState<AdminCouponDetail | null>(null);
  const [detailError, setDetailError] = useState(false);
  const [loadingDetailId, setLoadingDetailId] = useState<string | null>(null);

  const [dialog, setDialog] = useState<DialogState>(null);
  const [reason, setReason] = useState("");
  const [form, setForm] = useState<TermsFormState>(emptyTermsForm());
  const [dialogError, setDialogError] = useState<string | null>(null);
  const [formBusy, setFormBusy] = useState(false);

  useEffect(() => {
    const timer = setTimeout(() => {
      setQ(qInput.trim());
      setPage(1);
    }, 300);
    return () => clearTimeout(timer);
  }, [qInput]);

  const hasDataRef = useRef(false);

  const load = useCallback(async () => {
    if (!token) return;
    try {
      const next = await listAdminCoupons(token, {
        q: q || undefined,
        scope: scope || undefined,
        status: status || undefined,
        discount_type: discountType || undefined,
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
  }, [token, status, scope, discountType, q, sort, page, tick]);

  useEffect(() => {
    void load();
  }, [load]);

  const reload = useCallback(() => setTick((value) => value + 1), []);

  function changeStatus(value: string) {
    setStatus(value);
    setPage(1);
  }

  function changeScope(value: string) {
    setScope(value);
    setPage(1);
  }

  function changeType(value: string) {
    setDiscountType(value);
    setPage(1);
  }

  function changeSort(value: string) {
    setSort(value);
    setPage(1);
  }

  async function loadDetail(id: string) {
    if (!token) return;
    setLoadingDetailId(id);
    setDetailError(false);
    try {
      setDetail(await getAdminCoupon(token, id));
    } catch {
      setDetailError(true);
    } finally {
      setLoadingDetailId(null);
    }
  }

  async function toggleDetail(coupon: AdminCoupon) {
    if (expandedId === coupon.id) {
      setExpandedId(null);
      setDetail(null);
      return;
    }
    setExpandedId(coupon.id);
    setDetail(null);
    await loadDetail(coupon.id);
  }

  async function refetchDetail(id: string) {
    if (!token || expandedId !== id) return;
    try {
      setDetail(await getAdminCoupon(token, id));
    } catch {
      setDetailError(true);
    }
  }

  function openDialog(next: DialogState) {
    setDialog(next);
    setReason("");
    setDialogError(null);
    if (next?.kind === "create") setForm(emptyTermsForm());
    if (next?.kind === "terms") setForm(termsFormFromCoupon(next.coupon));
  }

  function closeDialog() {
    if (formBusy) return;
    setDialog(null);
    setDialogError(null);
  }

  /** Deactivate / reactivate confirm (the Dialog footer path). */
  async function confirmToggle() {
    if (
      !token ||
      !dialog ||
      (dialog.kind !== "deactivate" && dialog.kind !== "reactivate")
    ) {
      return;
    }
    const { kind, coupon } = dialog;
    if (kind === "deactivate" && reason.trim() === "") {
      setDialogError("বন্ধ করার কারণ লিখুন — a deactivation reason is required.");
      return;
    }
    setFormBusy(true);
    setDialogError(null);
    try {
      await setAdminCouponActive(
        token,
        coupon.id,
        kind === "reactivate",
        kind === "deactivate" ? reason.trim() : undefined
      );
      setDialog(null);
      setNotice(
        kind === "deactivate"
          ? coupon.store_id
            ? "কুপন বন্ধ করা হয়েছে — কারণসহ স্টোর মালিককে জানানো হয়েছে। (Coupon deactivated — the store owner has been notified with the reason.)"
            : "প্ল্যাটফর্ম কুপন বন্ধ করা হয়েছে। (Platform coupon deactivated.)"
          : "কুপন আবার চালু করা হয়েছে। (Coupon reactivated.)"
      );
      setPageError(null);
      await load();
      await refetchDetail(coupon.id);
    } catch (err) {
      if (err instanceof ApiCallError && err.status === 409) {
        setDialog(null);
        setConflictedIds((prev) => new Set(prev).add(coupon.id));
        setPageError(err.message);
        await load();
      } else {
        setDialogError(
          err instanceof Error ? err.message : "অবস্থা বদলানো যায়নি।"
        );
      }
    } finally {
      setFormBusy(false);
    }
  }

  /** Create / terms-edit submit (the in-body form button path). */
  async function submitForm() {
    if (
      !token ||
      !dialog ||
      (dialog.kind !== "create" && dialog.kind !== "terms")
    ) {
      return;
    }
    const built = buildTerms(form);
    if (!built.ok) {
      setDialogError(built.error);
      return;
    }
    if (dialog.kind === "create" && form.code.trim() === "") {
      setDialogError("কুপন কোড লিখুন — enter a coupon code.");
      return;
    }
    const termsCoupon = dialog.kind === "terms" ? dialog.coupon : null;
    setFormBusy(true);
    setDialogError(null);
    try {
      if (dialog.kind === "create") {
        const input: CreateCouponInput = {
          code: form.code.trim(),
          ...built.terms,
        };
        await createAdminCoupon(token, input);
        setNotice(
          `কুপন ${input.code.toUpperCase()} তৈরি হয়েছে — খরচ প্ল্যাটফর্ম বহন করবে। (Coupon created — the platform bears its discount.)`
        );
      } else {
        await updateAdminCouponTerms(token, dialog.coupon.id, built.terms);
        setNotice("কুপনের শর্ত হালনাগাদ হয়েছে। (Coupon terms updated.)");
        await refetchDetail(dialog.coupon.id);
      }
      setDialog(null);
      setPageError(null);
      await load();
    } catch (err) {
      if (
        termsCoupon &&
        err instanceof ApiCallError &&
        err.status === 409
      ) {
        setDialog(null);
        setConflictedIds((prev) => new Set(prev).add(termsCoupon.id));
        setPageError(err.message);
        await load();
      } else {
        setDialogError(
          err instanceof Error ? err.message : "সংরক্ষণ করা যায়নি।"
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
          <h2>কুপন — Coupons</h2>
          <p className="admin-muted">
            সর্বশেষ হালনাগাদ: {asOf ? formatDhakaDateTime(asOf) : "—"} (ঢাকা সময়)
          </p>
        </div>
        <div>
          <Button onClick={() => openDialog({ kind: "create" })}>
            + নতুন প্ল্যাটফর্ম কুপন — New platform coupon
          </Button>{" "}
          <Button onClick={reload}>↻ হালনাগাদ করুন — Refresh</Button>
        </div>
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
          ধরন — Scope
          <select value={scope} onChange={(e) => changeScope(e.target.value)}>
            {SCOPE_FILTERS.map((f) => (
              <option key={f.value} value={f.value}>
                {f.label}
              </option>
            ))}
          </select>
        </label>
        <label>
          ছাড় — Discount
          <select
            value={discountType}
            onChange={(e) => changeType(e.target.value)}
          >
            {TYPE_FILTERS.map((f) => (
              <option key={f.value} value={f.value}>
                {f.label}
              </option>
            ))}
          </select>
        </label>
        <label>
          সাজানো — Sort
          <select value={sort} onChange={(e) => changeSort(e.target.value)}>
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
            placeholder="কুপন কোড"
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
          <p>কুপন লোড করা যায়নি — Couldn&apos;t load coupons.</p>
          <Button size="sm" onClick={reload}>
            আবার চেষ্টা করুন — Retry
          </Button>
        </div>
      ) : null}

      {list && list.items.length === 0 ? (
        <EmptyState title={emptyTitle(status, scope, q)}>
          <p>{emptyBody(status, scope, q)}</p>
        </EmptyState>
      ) : null}

      {list ? (
        <div className="queue-list">
          {list.items.map((coupon) => (
            <CouponCard
              key={coupon.id}
              coupon={coupon}
              conflicted={conflictedIds.has(coupon.id)}
              expanded={expandedId === coupon.id}
              detail={expandedId === coupon.id ? detail : null}
              detailError={expandedId === coupon.id && detailError}
              detailLoading={loadingDetailId === coupon.id}
              onToggleDetail={() => void toggleDetail(coupon)}
              onRetryDetail={() => void loadDetail(coupon.id)}
              onAction={(next) => openDialog(next)}
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
        title={dialogTitle(dialog)}
        variant={dialog?.kind === "deactivate" ? "danger" : "default"}
        onConfirm={
          dialog?.kind === "deactivate" || dialog?.kind === "reactivate"
            ? () => void confirmToggle()
            : undefined
        }
        confirmLabel={
          dialog?.kind === "deactivate"
            ? "বন্ধ করুন — Deactivate"
            : "চালু করুন — Reactivate"
        }
        confirmDisabled={
          dialog?.kind === "deactivate" ? reason.trim() === "" : false
        }
        busy={formBusy}
        error={dialogError}
      >
        {dialog?.kind === "deactivate" || dialog?.kind === "reactivate" ? (
          <ToggleDialogBody
            kind={dialog.kind}
            coupon={dialog.coupon}
            reason={reason}
            onReasonChange={setReason}
          />
        ) : null}
        {dialog?.kind === "create" || dialog?.kind === "terms" ? (
          <TermsDialogBody
            kind={dialog.kind}
            coupon={dialog.kind === "terms" ? dialog.coupon : null}
            form={form}
            onFormChange={setForm}
            busy={formBusy}
            onSubmit={() => void submitForm()}
          />
        ) : null}
      </Dialog>
    </>
  );
}

function dialogTitle(dialog: DialogState): string {
  switch (dialog?.kind) {
    case "create":
      return "নতুন প্ল্যাটফর্ম কুপন — New platform coupon";
    case "deactivate":
      return "কুপন বন্ধ করুন — Deactivate coupon";
    case "reactivate":
      return "কুপন আবার চালু করুন — Reactivate coupon";
    case "terms":
      return "কুপনের শর্ত বদলান — Edit coupon terms";
    default:
      return "";
  }
}

function emptyTitle(status: string, scope: string, q: string): string {
  if (q) return "খোঁজে কোনো কুপন মেলেনি — No coupons match the search";
  if (status) return "এই অবস্থায় কোনো কুপন নেই — No coupons in this state";
  if (scope) return "এই ধরনের কোনো কুপন নেই — No coupons of this scope";
  return "কোনো কুপন নেই — No coupons yet";
}

function emptyBody(status: string, scope: string, q: string): string {
  if (q)
    return "কোডের অন্য অংশ দিয়ে খুঁজে দেখুন। (Try another part of the code.)";
  if (status || scope)
    return "ফিল্টার বদলে আবার দেখুন। (Change the filters to see more.)";
  return "প্রথম কুপন তৈরি হলেই এখানে দেখা যাবে। (Coupons appear here once created.)";
}

// ---------------------------------------------------------------------------
// Summary strip: the whole platform by lifecycle + scope
// ---------------------------------------------------------------------------

function SummaryStrip({
  summary,
  activeStatus,
  onPick,
}: {
  summary: AdminCouponList["summary"];
  activeStatus: string;
  onPick: (status: string) => void;
}) {
  const chips: { status: string; label: string; count: number }[] = [
    { status: "", label: "সব কুপন", count: summary.total },
    { status: "live", label: couponStatusLabel("live"), count: summary.live_count },
    {
      status: "scheduled",
      label: couponStatusLabel("scheduled"),
      count: summary.scheduled_count,
    },
    {
      status: "expired",
      label: couponStatusLabel("expired"),
      count: summary.expired_count,
    },
    {
      status: "exhausted",
      label: couponStatusLabel("exhausted"),
      count: summary.exhausted_count,
    },
    {
      status: "inactive",
      label: couponStatusLabel("inactive"),
      count: summary.inactive_count,
    },
  ];
  return (
    <div className="pipeline-row" aria-label="কুপন সারাংশ — Coupon summary">
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
          প্ল্যাটফর্ম: {formatCount(summary.platform_count)}টি · স্টোর:{" "}
          {formatCount(summary.store_count)}টি
        </Badge>
      </span>
      <span className="pipeline-chip">
        <Badge tone="muted">
          মোট ব্যবহার: {formatCount(summary.redemptions_all_time)}বার · মোট ছাড়
          (সর্বমোট): <PriceTag amountPoisha={summary.gross_discount_poisha_all_time} />
        </Badge>
      </span>
    </div>
  );
}

// ---------------------------------------------------------------------------
// One queue card
// ---------------------------------------------------------------------------

function CouponCard({
  coupon,
  conflicted,
  expanded,
  detail,
  detailError,
  detailLoading,
  onToggleDetail,
  onRetryDetail,
  onAction,
}: {
  coupon: AdminCoupon;
  conflicted: boolean;
  expanded: boolean;
  detail: AdminCouponDetail | null;
  detailError: boolean;
  detailLoading: boolean;
  onToggleDetail: () => void;
  onRetryDetail: () => void;
  onAction: (next: DialogState) => void;
}) {
  return (
    <article className="card">
      <div className="admin-overview-head">
        <h3>
          <code>{coupon.code}</code>{" "}
          <Badge tone={statusTone(coupon.status)}>
            {couponStatusLabel(coupon.status)}
          </Badge>{" "}
          <Badge tone={coupon.store_id ? "accent" : "brand"}>
            {coupon.store_id ? "স্টোর কুপন" : "প্ল্যাটফর্ম কুপন"}
          </Badge>{" "}
          <Badge tone="muted">
            {coupon.discount_type === "percent" ? "শতাংশ ছাড়" : "নির্দিষ্ট ছাড়"}
          </Badge>
        </h3>
        <div>
          <Button size="sm" onClick={onToggleDetail}>
            {expanded ? "বিস্তারিত লুকান — Hide" : "বিস্তারিত — Details"}
          </Button>{" "}
          {coupon.store_id === null ? (
            <>
              <Button
                size="sm"
                onClick={() => onAction({ kind: "terms", coupon })}
              >
                শর্ত বদলান — Edit terms
              </Button>{" "}
            </>
          ) : null}
          {coupon.active ? (
            <Button
              size="sm"
              variant="danger"
              onClick={() => onAction({ kind: "deactivate", coupon })}
            >
              বন্ধ করুন — Deactivate
            </Button>
          ) : (
            <Button
              size="sm"
              onClick={() => onAction({ kind: "reactivate", coupon })}
            >
              চালু করুন — Reactivate
            </Button>
          )}
        </div>
      </div>

      <p>
        ছাড়:{" "}
        {coupon.discount_type === "percent" ? (
          <>{formatCount(coupon.discount_value)}%</>
        ) : (
          <PriceTag amountPoisha={coupon.discount_value} />
        )}
        {coupon.max_discount_poisha !== null ? (
          <>
            {" "}
            · সর্বোচ্চ <PriceTag amountPoisha={coupon.max_discount_poisha} />
          </>
        ) : null}
        {coupon.min_order_poisha > 0 ? (
          <>
            {" "}
            · সর্বনিম্ন অর্ডার <PriceTag amountPoisha={coupon.min_order_poisha} />
          </>
        ) : null}{" "}
        · ব্যবহৃত: {formatCount(coupon.used_count)}
        {coupon.usage_limit !== null
          ? ` / ${formatCount(coupon.usage_limit)}`
          : " (সীমাহীন)"}
      </p>
      <p className="admin-muted">
        {coupon.store_id ? (
          <>
            স্টোর:{" "}
            <Link href={`/store/${coupon.store_slug ?? ""}`}>
              {coupon.store_name ?? coupon.store_slug ?? "—"}
            </Link>{" "}
            ·{" "}
          </>
        ) : (
          <>খরচ বহন করে: প্ল্যাটফর্ম · </>
        )}
        সময়সীমা:{" "}
        {coupon.starts_at ? formatDhakaDateTime(coupon.starts_at) : "শুরু থেকে"}{" "}
        → {coupon.ends_at ? formatDhakaDateTime(coupon.ends_at) : "অনির্দিষ্ট"} ·
        তৈরি: {formatDhakaDateTime(coupon.created_at)}
      </p>
      {coupon.admin_deactivated_reason ? (
        <p className="payout-warning">
          প্ল্যাটফর্ম বন্ধ করেছে — কারণ: {coupon.admin_deactivated_reason}
          {coupon.admin_deactivated_at
            ? ` (${formatDhakaDateTime(coupon.admin_deactivated_at)})`
            : ""}
        </p>
      ) : null}
      {conflicted ? (
        <p className="payout-warning">
          এই কুপনের অবস্থা এরই মধ্যে বদলে গেছে — তালিকা হালনাগাদ করা হয়েছে।
          (This coupon changed state in the meantime — the list has been
          refreshed.)
        </p>
      ) : null}

      {expanded ? (
        <CouponDetailView
          detail={detail}
          error={detailError}
          loading={detailLoading}
          onRetry={onRetryDetail}
        />
      ) : null}
    </article>
  );
}

// ---------------------------------------------------------------------------
// The expanded detail: usage + recent redemptions + audit history
// ---------------------------------------------------------------------------

function CouponDetailView({
  detail,
  error,
  loading,
  onRetry,
}: {
  detail: AdminCouponDetail | null;
  error: boolean;
  loading: boolean;
  onRetry: () => void;
}) {
  if (loading) return <Skeleton width="100%" height={120} />;
  if (error) {
    return (
      <div className="admin-error">
        <p>বিস্তারিত লোড করা যায়নি — Couldn&apos;t load the detail.</p>
        <Button size="sm" onClick={onRetry}>
          আবার চেষ্টা করুন — Retry
        </Button>
      </div>
    );
  }
  if (!detail) return null;
  const usage = detail.usage;
  return (
    <div>
      <h4>ব্যবহার — Usage (অর্ডারের স্ন্যাপশট থেকে)</h4>
      <p>
        মোট ব্যবহার: {formatCount(usage.gross_redemptions)}বার · মোট ছাড়:{" "}
        <PriceTag amountPoisha={usage.gross_discount_poisha} /> · আলাদা ক্রেতা:{" "}
        {formatCount(usage.distinct_buyers)}জন
      </p>
      <p>
        বাতিল অর্ডারে: {formatCount(usage.cancelled_redemptions)}বার ·{" "}
        <PriceTag amountPoisha={usage.cancelled_discount_poisha} /> · রিটার্নে:{" "}
        {formatCount(usage.returned_sub_orders)}টি সাব-অর্ডার ·{" "}
        <PriceTag amountPoisha={usage.returned_discount_poisha} />
      </p>
      <p>
        কার্যকর ছাড় (বাতিল ও রিটার্ন বাদে):{" "}
        <PriceTag amountPoisha={usage.net_effective_discount_poisha} />
      </p>
      <p className="admin-muted">
        কুপনের ব্যবহার কখনো মুছে যায় না — অর্ডার বাতিল বা রিটার্ন হলেও
        ব্যবহারের সংখ্যা কমে না, তাই উপরের &quot;মোট&quot; সংখ্যাই
        ব্যবহারের সীমার হিসাব। (A redemption is never removed: cancelling
        or returning an order does not free the usage slot, so the gross
        figures are what the usage limit counts.)
        {usage.first_redeemed_at
          ? ` প্রথম ব্যবহার: ${formatDhakaDateTime(usage.first_redeemed_at)} · সর্বশেষ: ${formatDhakaDateTime(usage.last_redeemed_at ?? "")}`
          : " এখনো একবারও ব্যবহৃত হয়নি।"}
      </p>

      <h4>সাম্প্রতিক ব্যবহার — Recent redemptions</h4>
      {detail.recent_redemptions.length === 0 ? (
        <p className="admin-muted">কোনো ব্যবহার নেই। (No redemptions yet.)</p>
      ) : (
        <ul>
          {detail.recent_redemptions.map((row) => (
            <li key={row.id}>
              <PriceTag amountPoisha={row.discount_poisha} /> · ক্রেতা{" "}
              {row.buyer_phone_masked} · অর্ডার {orderStatusLabel(row.order_status)}{" "}
              · {formatDhakaDateTime(row.created_at)}
            </li>
          ))}
        </ul>
      )}

      <h4>অডিট ইতিহাস — Audit history</h4>
      {detail.audit_history.length === 0 ? (
        <p className="admin-muted">
          কোনো অ্যাডমিন পদক্ষেপের রেকর্ড নেই। (No admin actions recorded.)
        </p>
      ) : (
        <ul>
          {detail.audit_history.map((entry) => (
            <li key={entry.id}>
              {entry.action} · {entry.admin_phone_masked} ·{" "}
              {formatDhakaDateTime(entry.created_at)}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Dialog bodies
// ---------------------------------------------------------------------------

function ToggleDialogBody({
  kind,
  coupon,
  reason,
  onReasonChange,
}: {
  kind: "deactivate" | "reactivate";
  coupon: AdminCoupon;
  reason: string;
  onReasonChange: (value: string) => void;
}) {
  if (kind === "reactivate") {
    return (
      <div>
        <p>
          <code>{coupon.code}</code> আবার চালু হবে — কোডটি চেকআউটে আবার কাজ
          করবে। (The code will work at checkout again.)
        </p>
        {coupon.admin_deactivated_reason ? (
          <p className="admin-muted">
            আগের বন্ধের কারণ (&quot;{coupon.admin_deactivated_reason}&quot;)
            কুপন থেকে মুছে যাবে, তবে অডিট লগে থেকে যাবে। (The previous
            deactivation reason is cleared from the coupon; it stays in the
            audit log.)
          </p>
        ) : null}
      </div>
    );
  }
  return (
    <div>
      <p>
        <code>{coupon.code}</code> বন্ধ হয়ে যাবে। এরপর:
      </p>
      <ul>
        <li>
          নতুন চেকআউটে কোডটি আর কাজ করবে না। (The code stops working at
          checkout.)
        </li>
        <li>
          কার্টে কোড ধরে রাখা ক্রেতার চেকআউট ব্যর্থ হবে — কোড সরিয়ে আবার
          চেষ্টা করতে হবে। কার্ট স্ন্যাপশট নয়, চেকআউটের সময় লাইভ কুপনই
          যাচাই হয়। (A buyer mid-checkout with this code will fail and must
          remove it — carts do not snapshot coupons.)
        </li>
        <li>
          ইতিমধ্যে সম্পন্ন অর্ডার ও তাদের ছাড় অপরিবর্তিত থাকবে। (Completed
          orders and their discounts are untouched.)
        </li>
        {coupon.store_id ? (
          <li>
            এই কুপনের ছাড় স্টোর বহন করে; মালিককে কারণসহ জানানো হবে। (The
            seller bears this coupon&apos;s discount; the owner will be
            notified with your reason.)
          </li>
        ) : (
          <li>
            এই কুপনের খরচ প্ল্যাটফর্ম বহন করে। (The platform bears this
            coupon&apos;s cost.)
          </li>
        )}
        <li>
          কুপন মুছে যাবে না — যেকোনো সময় আবার চালু করা যাবে। (Nothing is
          deleted — the coupon can be reactivated at any time.)
        </li>
      </ul>
      <label>
        বন্ধ করার কারণ (আবশ্যক) — Reason (required)
        <textarea
          value={reason}
          onChange={(e) => onReasonChange(e.target.value)}
          maxLength={MAX_REASON_CHARS}
          rows={3}
        />
      </label>
      <p className="admin-muted">
        {formatCount(reason.length)} / {formatCount(MAX_REASON_CHARS)} অক্ষর —
        কারণটি কুপনে ও অডিট লগে থাকবে
        {coupon.store_id ? " এবং স্টোর মালিক দেখতে পাবেন" : ""}। (Stored on
        the coupon and in the audit log
        {coupon.store_id ? "; the store owner will see it" : ""}.)
      </p>
    </div>
  );
}

function TermsDialogBody({
  kind,
  coupon,
  form,
  onFormChange,
  busy,
  onSubmit,
}: {
  kind: "create" | "terms";
  coupon: AdminCoupon | null;
  form: TermsFormState;
  onFormChange: (form: TermsFormState) => void;
  busy: boolean;
  onSubmit: () => void;
}) {
  const locked = kind === "terms" && coupon !== null && coupon.used_count > 0;
  const set = (patch: Partial<TermsFormState>) =>
    onFormChange({ ...form, ...patch });
  if (locked && coupon) {
    return (
      <div>
        <p>
          <code>{coupon.code}</code> ইতিমধ্যে {formatCount(coupon.used_count)}
          বার ব্যবহৃত হয়েছে — এর শর্ত আর বদলানো যাবে না। অর্ডার শুধু ছাড়ের
          পরিমাণ স্ন্যাপশট রাখে, শর্ত রাখে না; শর্ত বদলালে পুরনো ছাড় কেন
          দেওয়া হয়েছিল তা আর জানা যাবে না। (This coupon has redemptions, so
          its terms are locked — orders snapshot the discount amount, never
          the terms.)
        </p>
        <p>
          বদলাতে চাইলে কুপনটি বন্ধ করে নতুন কোড তৈরি করুন। (Deactivate it and
          create a new code instead.)
        </p>
      </div>
    );
  }
  return (
    <div>
      {kind === "create" ? (
        <p className="admin-muted">
          প্ল্যাটফর্ম কুপন পুরো অর্ডারের পণ্যে ছাড় দেয়, খরচ প্ল্যাটফর্ম
          বহন করে — বিক্রেতার কমিশন পুরো দামের উপরই থাকে। কোড একবার তৈরি
          হলে বদলানো যায় না। (A platform coupon discounts the whole
          order&apos;s goods; the platform bears it and commissions stay on
          the full goods. The code itself never changes.)
        </p>
      ) : (
        <p className="admin-muted">
          এই কুপন এখনো একবারও ব্যবহৃত হয়নি, তাই শর্ত বদলানো যাচ্ছে।
          বদলানোর পর কার্টে কোড ধরে রাখা ক্রেতা চেকআউটে নতুন শর্তই পাবেন —
          কার্ট কোড স্ন্যাপশট করে না। (Never redeemed, so the terms may
          change. Buyers holding the code get the NEW terms at checkout —
          carts do not snapshot the code&apos;s terms.)
        </p>
      )}
      {kind === "create" ? (
        <label>
          কোড — Code (3–32 অক্ষর: A–Z, 0–9, -, _)
          <input
            type="text"
            value={form.code}
            onChange={(e) => set({ code: e.target.value })}
            placeholder="EID2026"
          />
        </label>
      ) : null}
      <label>
        ছাড়ের ধরন — Discount type
        <select
          value={form.discountType}
          onChange={(e) => set({ discountType: e.target.value })}
        >
          <option value="percent">শতাংশ — Percent (%)</option>
          <option value="fixed">নির্দিষ্ট টাকা — Fixed amount (৳)</option>
        </select>
      </label>
      <label>
        {form.discountType === "percent"
          ? "শতাংশ (1–90) — Percent"
          : "পরিমাণ (টাকায়, কমপক্ষে ৳1) — Amount in taka"}
        <input
          type="number"
          value={form.discountValue}
          onChange={(e) => set({ discountValue: e.target.value })}
        />
      </label>
      {form.discountType === "percent" ? (
        <label>
          সর্বোচ্চ ছাড় (টাকায়, ঐচ্ছিক) — Max discount cap in taka (optional)
          <input
            type="number"
            value={form.capTaka}
            onChange={(e) => set({ capTaka: e.target.value })}
          />
        </label>
      ) : null}
      <label>
        সর্বনিম্ন অর্ডার (টাকায়, ঐচ্ছিক) — Minimum order in taka (optional)
        <input
          type="number"
          value={form.minOrderTaka}
          onChange={(e) => set({ minOrderTaka: e.target.value })}
        />
      </label>
      <label>
        ব্যবহারের সীমা (ঐচ্ছিক) — Usage limit (optional)
        <input
          type="number"
          value={form.usageLimit}
          onChange={(e) => set({ usageLimit: e.target.value })}
        />
      </label>
      <label>
        শুরু — Starts at (optional)
        <input
          type="datetime-local"
          value={form.startsAt}
          onChange={(e) => set({ startsAt: e.target.value })}
        />
      </label>
      <label>
        শেষ — Ends at (optional)
        <input
          type="datetime-local"
          value={form.endsAt}
          onChange={(e) => set({ endsAt: e.target.value })}
        />
      </label>
      <p>
        <Button onClick={onSubmit} loading={busy}>
          {kind === "create"
            ? "কুপন তৈরি করুন — Create coupon"
            : "শর্ত সংরক্ষণ করুন — Save terms"}
        </Button>
      </p>
    </div>
  );
}
