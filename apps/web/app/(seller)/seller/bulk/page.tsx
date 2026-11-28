"use client";

import { useCallback, useEffect, useMemo, useState } from "react";
import Link from "next/link";
import {
  bulkEditProducts,
  exportCatalogueCsv,
  getBulkImportDetail,
  getBulkImportTemplate,
  getStoreProducts,
  importBulkCsv,
  listBulkImportHistory,
  listSellerStores,
  type BulkEditOutcome,
  type BulkImportReport,
  type BulkImportSummary,
  type Product,
  type SellerStore,
} from "@/lib/api";
import { formatBdt, parseTakaToPoisha } from "@/lib/format";
import { outcomesToCsv, previewBulkSheet } from "@/lib/bulkCsv";
import { useAuth } from "@/hooks/useAuth";
import { Badge, Button, EmptyState, SectionHeader } from "@/components/ui";
import CategoryPicker, {
  EMPTY_CATEGORY_PICK,
  type CategoryPick,
} from "@/components/CategoryPicker";

/**
 * Seller bulk catalogue page (Wave 57): the spreadsheet
 * door for the catalogue. Three tools on one page —
 *
 * 1. CSV import: paste or pick a sheet, dry-run it first
 *    (the report renders as a table: one row per sheet
 *    row with its action, its field errors, its notes),
 *    then run it for real. Re-importing the same sheet
 *    updates by SKU instead of duplicating.
 * 2. Import history: every run (dry runs included) is
 *    recorded by the API; a past report re-opens here.
 * 3. Bulk edit: tick products, apply a price / stock /
 *    status / category action to all of them at once,
 *    with a per-product outcome list.
 *
 * Plus the round-trip export button: the catalogue comes
 * down in the import's own column format.
 */

function downloadText(filename: string, text: string) {
  const blob = new Blob([text], { type: "text/csv; charset=utf-8" });
  const url = URL.createObjectURL(blob);
  const anchor = document.createElement("a");
  anchor.href = url;
  anchor.download = filename;
  anchor.click();
  URL.revokeObjectURL(url);
}

/**
 * The live sheet preview (client-side mirror of the
 * domain's header contract): header problems, the data
 * row count against the 500 cap, ragged rows and a few
 * sample SKUs — shown as the seller types or picks a
 * file, before anything is uploaded. The server report
 * stays the authority; this only saves a round-trip.
 */
function SheetPreview({ text }: { text: string }) {
  const preview = useMemo(() => previewBulkSheet(text), [text]);
  if (!preview.parseable) {
    return (
      <p role="alert">
        CSV পড়া যাচ্ছে না — একটি উদ্ধৃতি চিহ্ন সম্ভবত বন্ধ হয়নি।
      </p>
    );
  }
  return (
    <div>
      {preview.headerProblems.map((problem) => (
        <p key={problem} role="alert">
          {problem}
        </p>
      ))}
      <p>
        ডেটা সারি: {preview.dataRows}
        {preview.overCap ? " — সর্বোচ্চ ৫০০, ফাইল ভাগ করুন!" : ""}
        {preview.raggedRows > 0
          ? ` · ${preview.raggedRows}টি সারিতে কলাম সংখ্যা মিলছে না`
          : ""}
        {preview.sampleSkus.length > 0
          ? ` · SKU: ${preview.sampleSkus.join(", ")}`
          : ""}
      </p>
    </div>
  );
}

function actionTone(action: string): "brand" | "accent" | "danger" | "muted" {
  if (action === "created" || action === "would_create") return "brand";
  if (action === "updated" || action === "would_update") return "accent";
  return "danger";
}

function actionLabel(action: string): string {
  switch (action) {
    case "created":
      return "তৈরি হয়েছে — Created";
    case "updated":
      return "আপডেট হয়েছে — Updated";
    case "would_create":
      return "তৈরি হবে — Would create";
    case "would_update":
      return "আপডেট হবে — Would update";
    default:
      return "ত্রুটি — Error";
  }
}

/** The import report, rendered as a table (used for the
 * fresh run and for a re-opened history entry alike). */
function ImportReportView({ report }: { report: BulkImportReport }) {
  return (
    <div>
      <p>
        মোট সারি {report.total_rows} — তৈরি {report.created}, আপডেট{" "}
        {report.updated}, ত্রুটি {report.skipped}
        {report.dry_run ? " (ড্রাই রান — কিছুই সেভ হয়নি)" : ""}{" "}
        {report.skipped > 0 ? (
          <Button
            variant="ghost"
            size="sm"
            onClick={() =>
              downloadText("import-report.csv", outcomesToCsv(report.outcomes))
            }
          >
            ত্রুটির তালিকা CSV ডাউনলোড
          </Button>
        ) : null}
      </p>
      <div style={{ overflowX: "auto" }}>
        <table>
          <thead>
            <tr>
              <th>সারি</th>
              <th>SKU</th>
              <th>ফলাফল</th>
              <th>বিস্তারিত</th>
            </tr>
          </thead>
          <tbody>
            {report.outcomes.map((outcome) => (
              <tr key={outcome.row_number}>
                <td>{outcome.row_number}</td>
                <td>{outcome.sku ?? "—"}</td>
                <td>
                  <Badge tone={actionTone(outcome.action)}>
                    {actionLabel(outcome.action)}
                  </Badge>
                </td>
                <td>
                  {outcome.errors.map((error, index) => (
                    <div key={index}>
                      <strong>{error.field}:</strong> {error.message}
                    </div>
                  ))}
                  {outcome.notes.map((note, index) => (
                    <div key={`n-${index}`}>{note}</div>
                  ))}
                  {outcome.image_error ? (
                    <div>ছবি যোগ হয়নি: {outcome.image_error}</div>
                  ) : null}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </div>
  );
}

export default function SellerBulkPage() {
  const { token, ready } = useAuth();
  const [stores, setStores] = useState<SellerStore[] | null>(null);
  const [storeId, setStoreId] = useState<string>("");
  const [error, setError] = useState<string | null>(null);

  // Import state.
  const [csvText, setCsvText] = useState("");
  const [dryRun, setDryRun] = useState(true);
  const [running, setRunning] = useState(false);
  const [report, setReport] = useState<BulkImportReport | null>(null);

  // History state.
  const [history, setHistory] = useState<BulkImportSummary[] | null>(null);
  const [historyReport, setHistoryReport] = useState<BulkImportReport | null>(
    null
  );

  // Bulk-edit state.
  const [products, setProducts] = useState<Product[] | null>(null);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [priceMode, setPriceMode] = useState<"none" | "set" | "percent" | "adjust">(
    "none"
  );
  const [priceTaka, setPriceTaka] = useState("");
  const [percent, setPercent] = useState("");
  const [stockMode, setStockMode] = useState<"none" | "set" | "adjust">("none");
  const [stockValue, setStockValue] = useState("");
  const [statusValue, setStatusValue] = useState<string>("");
  const [categoryPick, setCategoryPick] = useState<CategoryPick>(EMPTY_CATEGORY_PICK);
  const [editOutcomes, setEditOutcomes] = useState<BulkEditOutcome[] | null>(null);
  const [applying, setApplying] = useState(false);

  const store = useMemo(
    () => stores?.find((entry) => entry.id === storeId) ?? null,
    [stores, storeId]
  );

  useEffect(() => {
    if (!token) return;
    listSellerStores(token)
      .then((list) => {
        setStores(list);
        if (list.length > 0) setStoreId(list[0].id);
      })
      .catch((err: Error) => setError(err.message));
  }, [token]);

  const refreshSideData = useCallback(() => {
    if (!token || !store) return;
    listBulkImportHistory(token, store.id)
      .then(setHistory)
      .catch(() => setHistory([]));
    getStoreProducts(store.slug, token)
      .then((list) => setProducts(list.items))
      .catch(() => setProducts([]));
  }, [token, store]);

  useEffect(() => {
    setReport(null);
    setHistoryReport(null);
    setEditOutcomes(null);
    setSelected(new Set());
    refreshSideData();
  }, [refreshSideData]);

  if (!ready) return <p>লোড হচ্ছে…</p>;
  if (!token) {
    return (
      <EmptyState title="আগে লগইন করুন">
        <p>
          বাল্ক টুল ব্যবহার করতে <Link href="/login">লগইন</Link> করুন।
        </p>
      </EmptyState>
    );
  }

  const runImport = async () => {
    if (!store) return;
    setRunning(true);
    setError(null);
    try {
      const result = await importBulkCsv(token, store.id, csvText, dryRun);
      setReport(result);
      refreshSideData();
    } catch (err) {
      setError(err instanceof Error ? err.message : "ইমপোর্ট ব্যর্থ হয়েছে");
    } finally {
      setRunning(false);
    }
  };

  const onFilePicked = (file: File | null) => {
    if (!file) return;
    const reader = new FileReader();
    reader.onload = () => setCsvText(String(reader.result ?? ""));
    reader.readAsText(file);
  };

  const toggleSelected = (id: string) => {
    setSelected((current) => {
      const next = new Set(current);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  };

  const applyBulkEdit = async () => {
    if (!store || selected.size === 0) return;
    setApplying(true);
    setError(null);
    try {
      const body: Parameters<typeof bulkEditProducts>[1] = {
        product_ids: Array.from(selected),
      };
      if (priceMode === "set") {
        const poisha = parseTakaToPoisha(priceTaka);
        if (poisha === null) throw new Error("মূল্য সঠিক নয় — টাকায় লিখুন");
        body.price = { mode: "set", price_poisha: poisha };
      } else if (priceMode === "percent") {
        const value = Number.parseInt(percent, 10);
        if (!Number.isFinite(value)) throw new Error("শতাংশ সঠিক নয়");
        body.price = { mode: "percent", percent: value };
      } else if (priceMode === "adjust") {
        const poisha = parseTakaToPoisha(priceTaka);
        if (poisha === null) throw new Error("পরিমাণ সঠিক নয় — টাকায় লিখুন");
        body.price = {
          mode: "adjust",
          delta_poisha: priceTaka.trim().startsWith("-") ? -poisha : poisha,
        };
      }
      if (stockMode !== "none") {
        const value = Number.parseInt(stockValue, 10);
        if (!Number.isFinite(value)) throw new Error("স্টকের সংখ্যা সঠিক নয়");
        body.stock =
          stockMode === "set"
            ? { mode: "set", stock: value }
            : { mode: "adjust", delta: value };
      }
      if (statusValue) body.status = statusValue;
      if (categoryPick.category_id) {
        body.category = { category_id: categoryPick.category_id };
      } else if (categoryPick.category.trim()) {
        body.category = { category_label: categoryPick.category.trim() };
      }
      const result = await bulkEditProducts(token, body);
      setEditOutcomes(result.outcomes);
      refreshSideData();
    } catch (err) {
      setError(err instanceof Error ? err.message : "বাল্ক এডিট ব্যর্থ হয়েছে");
    } finally {
      setApplying(false);
    }
  };

  return (
    <main>
      <SectionHeader title="বাল্ক ক্যাটালগ — Bulk catalogue" />
      <p>
        <Link href="/seller">← ড্যাশবোর্ড</Link>
      </p>
      {error ? <p role="alert">{error}</p> : null}

      <label>
        স্টোর — Store{" "}
        <select
          value={storeId}
          onChange={(event) => setStoreId(event.target.value)}
        >
          {(stores ?? []).map((entry) => (
            <option key={entry.id} value={entry.id}>
              {entry.name}
            </option>
          ))}
        </select>
      </label>

      <section>
        <SectionHeader title="CSV ইমপোর্ট — Import" />
        <p>
          কলাম: sku, title, title_bn, description, price_poisha,
          compare_at_poisha, stock, weight_grams, status, category_slug,
          category_label, image_url। দাম পয়সায় (৳১ = ১০০)। একই SKU আবার
          ইমপোর্ট করলে পণ্য আপডেট হয়, ডুপ্লিকেট হয় না। সর্বোচ্চ ৫০০ সারি।
        </p>
        <p>
          <Button
            onClick={() => {
              getBulkImportTemplate(token)
                .then((text) => downloadText("bulk-import-template.csv", text))
                .catch((err: Error) => setError(err.message));
            }}
          >
            নমুনা টেমপ্লেট ডাউনলোড
          </Button>{" "}
          <Button
            variant="ghost"
            onClick={() => {
              if (!store) return;
              exportCatalogueCsv(token, store.id)
                .then((text) => downloadText("catalogue-export.csv", text))
                .catch((err: Error) => setError(err.message));
            }}
          >
            ক্যাটালগ এক্সপোর্ট (CSV)
          </Button>
        </p>
        <p>
          <input
            type="file"
            accept=".csv,text/csv"
            onChange={(event) => onFilePicked(event.target.files?.[0] ?? null)}
          />
        </p>
        <textarea
          value={csvText}
          onChange={(event) => setCsvText(event.target.value)}
          rows={10}
          style={{ width: "100%" }}
          placeholder="sku,title,price_poisha,..."
        />
        {csvText.trim() ? <SheetPreview text={csvText} /> : null}
        <p>
          <label>
            <input
              type="checkbox"
              checked={dryRun}
              onChange={(event) => setDryRun(event.target.checked)}
            />{" "}
            ড্রাই রান — Dry run (শুধু যাচাই, কিছুই সেভ হবে না)
          </label>
        </p>
        <Button loading={running} disabled={!store || !csvText.trim()} onClick={runImport}>
          {dryRun ? "যাচাই করুন — Preview" : "ইমপোর্ট করুন — Import"}
        </Button>
        {report ? <ImportReportView report={report} /> : null}
      </section>

      <section>
        <SectionHeader title="ইমপোর্ট ইতিহাস — History" />
        {history === null ? (
          <p>লোড হচ্ছে…</p>
        ) : history.length === 0 ? (
          <p>এখনো কোনো ইমপোর্ট হয়নি।</p>
        ) : (
          <ul>
            {history.map((entry) => (
              <li key={entry.id}>
                {new Date(entry.created_at).toLocaleString()} — মোট{" "}
                {entry.total_rows}, তৈরি {entry.created_count}, আপডেট{" "}
                {entry.updated_count}, ত্রুটি {entry.error_count}{" "}
                {entry.dry_run ? <Badge tone="muted">ড্রাই রান</Badge> : null}{" "}
                <Button
                  variant="ghost"
                  size="sm"
                  onClick={() => {
                    getBulkImportDetail(token, entry.id)
                      .then(setHistoryReport)
                      .catch((err: Error) => setError(err.message));
                  }}
                >
                  রিপোর্ট দেখুন
                </Button>
              </li>
            ))}
          </ul>
        )}
        {historyReport ? <ImportReportView report={historyReport} /> : null}
      </section>

      <section>
        <SectionHeader title="বাল্ক এডিট — Bulk edit" />
        <p>পণ্য বেছে নিন, তারপর একসাথে মূল্য / স্টক / অবস্থা / ক্যাটাগরি বদলান।</p>
        {products === null ? (
          <p>লোড হচ্ছে…</p>
        ) : products.length === 0 ? (
          <p>এই স্টোরে এখনো পণ্য নেই।</p>
        ) : (
          <div style={{ overflowX: "auto" }}>
            <table>
              <thead>
                <tr>
                  <th>
                    <input
                      type="checkbox"
                      checked={selected.size === products.length}
                      onChange={(event) =>
                        setSelected(
                          event.target.checked
                            ? new Set(products.map((product) => product.id))
                            : new Set()
                        )
                      }
                    />
                  </th>
                  <th>পণ্য</th>
                  <th>মূল্য</th>
                  <th>স্টক</th>
                  <th>অবস্থা</th>
                </tr>
              </thead>
              <tbody>
                {products.map((product) => (
                  <tr key={product.id}>
                    <td>
                      <input
                        type="checkbox"
                        checked={selected.has(product.id)}
                        onChange={() => toggleSelected(product.id)}
                      />
                    </td>
                    <td>{product.title}</td>
                    <td>{formatBdt(product.price_poisha)}</td>
                    <td>{product.stock}</td>
                    <td>{product.status}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}

        <div>
          <label>
            মূল্য — Price{" "}
            <select
              value={priceMode}
              onChange={(event) =>
                setPriceMode(event.target.value as typeof priceMode)
              }
            >
              <option value="none">পরিবর্তন নেই</option>
              <option value="set">নতুন মূল্য (টাকা)</option>
              <option value="percent">শতাংশ ±</option>
              <option value="adjust">টাকা ± যোগ/বিয়োগ</option>
            </select>
          </label>{" "}
          {priceMode === "set" || priceMode === "adjust" ? (
            <input
              value={priceTaka}
              onChange={(event) => setPriceTaka(event.target.value)}
              placeholder={priceMode === "set" ? "যেমন 1500" : "যেমন -50 বা 100"}
              inputMode="decimal"
            />
          ) : null}
          {priceMode === "percent" ? (
            <input
              value={percent}
              onChange={(event) => setPercent(event.target.value)}
              placeholder="যেমন -10 বা 15"
              inputMode="numeric"
            />
          ) : null}
        </div>
        <div>
          <label>
            স্টক — Stock{" "}
            <select
              value={stockMode}
              onChange={(event) =>
                setStockMode(event.target.value as typeof stockMode)
              }
            >
              <option value="none">পরিবর্তন নেই</option>
              <option value="set">ঠিক এই পরিমাণ</option>
              <option value="adjust">± যোগ/বিয়োগ</option>
            </select>
          </label>{" "}
          {stockMode !== "none" ? (
            <input
              value={stockValue}
              onChange={(event) => setStockValue(event.target.value)}
              placeholder="যেমন 40 বা -5"
              inputMode="numeric"
            />
          ) : null}
        </div>
        <div>
          <label>
            অবস্থা — Status{" "}
            <select
              value={statusValue}
              onChange={(event) => setStatusValue(event.target.value)}
            >
              <option value="">পরিবর্তন নেই</option>
              <option value="draft">draft (খসড়া)</option>
              <option value="active">active (সক্রিয়)</option>
              <option value="archived">archived (আর্কাইভ)</option>
            </select>
          </label>
        </div>
        <div>
          <span>ক্যাটাগরি — Category</span>
          <CategoryPicker value={categoryPick} onChange={setCategoryPick} />
        </div>
        <p>
          <Button
            loading={applying}
            disabled={selected.size === 0}
            onClick={applyBulkEdit}
          >
            {selected.size}টি পণ্যে প্রয়োগ করুন — Apply
          </Button>
        </p>
        {editOutcomes ? (
          <ul>
            {editOutcomes.map((outcome) => {
              const product = products?.find(
                (entry) => entry.id === outcome.product_id
              );
              return (
                <li key={outcome.product_id}>
                  {product?.title ?? outcome.product_id}:{" "}
                  {outcome.ok ? (
                    <Badge tone="brand">
                      হয়েছে — {outcome.changed.join(", ") || "কোনো বদল লাগেনি"}
                    </Badge>
                  ) : (
                    <Badge tone="danger">{outcome.error}</Badge>
                  )}
                </li>
              );
            })}
          </ul>
        ) : null}
      </section>
    </main>
  );
}
