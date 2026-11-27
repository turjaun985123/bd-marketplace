"use client";

import { useCallback, useEffect, useState } from "react";
import AdminShell from "@/components/AdminShell";
import { Badge, Button, EmptyState } from "@/components/ui";
import { useAuth } from "@/hooks/useAuth";
import {
  createAdminFlashSale,
  deleteAdminFlashSale,
  getAdminFlashSale,
  listAdminFlashSales,
  searchProducts,
  setAdminFlashSaleItems,
  updateAdminFlashSale,
  type FlashSale,
  type FlashSaleItemInput,
  type Product,
} from "@/lib/api";
import {
  formatBdt,
  formatDateTime,
  isoToLocalInput,
  localInputToIso,
  parseTakaToPoisha,
} from "@/lib/format";

/**
 * Where a sale's window stands right now, by the half-open
 * rule the price resolution applies (live from `starts_at`
 * up to but not including `ends_at`). A display aid for the
 * admin list — the resolution remains the authority.
 */
function saleState(sale: FlashSale): "live" | "scheduled" | "ended" {
  const now = Date.now();
  if (new Date(sale.starts_at).getTime() > now) return "scheduled";
  if (new Date(sale.ends_at).getTime() <= now) return "ended";
  return "live";
}

/**
 * Flash sales (Wave 52): time-boxed platform-wide price
 * drops. The list manages the sales; the editor below
 * manages one sale's items — each product gets a sale price
 * (entered in taka, stored in poisha) and an optional stock
 * cap, the maximum units sellable at the sale price.
 *
 * The money rules are the API's, shown here so the admin
 * sees them before saving: a sale price must be a genuine
 * reduction of the product's current base price; at most
 * one ACTIVE sale may cover any instant (overlaps are
 * refused); and units sold at the sale price are counted
 * from order snapshots, so the sold/cap columns below are
 * the same numbers the price resolution itself uses.
 */
export default function AdminFlashSalesPage() {
  const { token } = useAuth();
  const [sales, setSales] = useState<FlashSale[]>([]);
  const [page, setPage] = useState(1);
  const [total, setTotal] = useState(0);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const PAGE_SIZE = 20;

  const [editingId, setEditingId] = useState<string | null>(null);
  const [title, setTitle] = useState("");
  const [startsAt, setStartsAt] = useState("");
  const [endsAt, setEndsAt] = useState("");
  const [isActive, setIsActive] = useState(true);

  // The item editor: one sale, its rows (product + entered
  // terms), and the picker state.
  const [itemTarget, setItemTarget] = useState<FlashSale | null>(null);
  const [rows, setRows] = useState<EditorRow[]>([]);
  const [query, setQuery] = useState("");
  const [results, setResults] = useState<Product[]>([]);
  const [savingItems, setSavingItems] = useState(false);

  const load = useCallback(async () => {
    if (!token) return;
    try {
      const data = await listAdminFlashSales(token, { page, limit: PAGE_SIZE });
      setSales(data.items);
      setTotal(data.total);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  }, [token, page]);

  useEffect(() => {
    void load();
  }, [load]);

  function resetForm() {
    setEditingId(null);
    setTitle("");
    setStartsAt("");
    setEndsAt("");
    setIsActive(true);
  }

  function startEdit(sale: FlashSale) {
    setEditingId(sale.id);
    setTitle(sale.title);
    setStartsAt(isoToLocalInput(sale.starts_at));
    setEndsAt(isoToLocalInput(sale.ends_at));
    setIsActive(sale.is_active);
    setError(null);
  }

  async function onSubmit(event: React.FormEvent) {
    event.preventDefault();
    if (!token) return;
    const startsIso = localInputToIso(startsAt);
    const endsIso = localInputToIso(endsAt);
    if (!startsIso || !endsIso) {
      setError("শুরু ও শেষের সময় দুটোই লাগবে — Both the start and end times are required.");
      return;
    }
    setBusy(true);
    setError(null);
    setNotice(null);
    try {
      const input = {
        title: title.trim(),
        starts_at: startsIso,
        ends_at: endsIso,
        is_active: isActive,
      };
      if (editingId) {
        await updateAdminFlashSale(token, editingId, input);
        setNotice("ফ্ল্যাশ সেল আপডেট হয়েছে — Flash sale updated.");
      } else {
        await createAdminFlashSale(token, input);
        setNotice("ফ্ল্যাশ সেল তৈরি হয়েছে — Flash sale created.");
      }
      resetForm();
      await load();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  }

  async function onDelete(sale: FlashSale) {
    if (!token) return;
    if (!window.confirm(`"${sale.title}" ফ্ল্যাশ সেলটি মুছবেন? (Delete this flash sale?)`)) {
      return;
    }
    setError(null);
    try {
      await deleteAdminFlashSale(token, sale.id);
      if (editingId === sale.id) resetForm();
      if (itemTarget?.id === sale.id) setItemTarget(null);
      await load();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  }

  async function openItems(sale: FlashSale) {
    if (!token) return;
    setError(null);
    try {
      const detail = await getAdminFlashSale(token, sale.id);
      setItemTarget(sale);
      setRows(
        detail.items.map((item) => ({
          productId: item.product_id,
          productTitle: item.product_title,
          basePricePoisha: item.base_price_poisha,
          salePriceTaka: String(item.sale_price_poisha / 100),
          stockCap: item.stock_cap === null ? "" : String(item.stock_cap),
          soldAtSale: item.sold_at_sale,
          capRemaining: item.cap_remaining,
        }))
      );
      setResults([]);
      setQuery("");
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  }

  async function onSearch(event: React.FormEvent) {
    event.preventDefault();
    setError(null);
    try {
      const page = await searchProducts({ q: query.trim(), per_page: 20 });
      setResults(page.items);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  }

  function updateRow(productId: string, patch: Partial<EditorRow>) {
    setRows((current) =>
      current.map((row) =>
        row.productId === productId ? { ...row, ...patch } : row
      )
    );
  }

  async function onSaveItems() {
    if (!token || !itemTarget) return;
    const items: FlashSaleItemInput[] = [];
    for (const row of rows) {
      const salePrice = parseTakaToPoisha(row.salePriceTaka);
      if (salePrice === null || salePrice <= 0) {
        setError(
          `"${row.productTitle}"-এর সেল মূল্য ঠিকমতো লিখুন (টাকায়)। (Enter a valid sale price in taka for "${row.productTitle}".)`
        );
        return;
      }
      let cap: number | null = null;
      if (row.stockCap.trim() !== "") {
        cap = Number.parseInt(row.stockCap, 10);
        if (!Number.isFinite(cap) || cap <= 0) {
          setError(
            `"${row.productTitle}"-এর স্টক ক্যাপ ধনাত্মক সংখ্যা হতে হবে, অথবা খালি রাখুন। (The stock cap must be a positive number, or empty for uncapped.)`
          );
          return;
        }
      }
      items.push({
        product_id: row.productId,
        sale_price_poisha: salePrice,
        stock_cap: cap,
      });
    }
    setSavingItems(true);
    setError(null);
    setNotice(null);
    try {
      const detail = await setAdminFlashSaleItems(token, itemTarget.id, items);
      setRows(
        detail.items.map((item) => ({
          productId: item.product_id,
          productTitle: item.product_title,
          basePricePoisha: item.base_price_poisha,
          salePriceTaka: String(item.sale_price_poisha / 100),
          stockCap: item.stock_cap === null ? "" : String(item.stock_cap),
          soldAtSale: item.sold_at_sale,
          capRemaining: item.cap_remaining,
        }))
      );
      setNotice("আইটেম সংরক্ষিত — Items saved.");
      await load();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSavingItems(false);
    }
  }

  const rowIds = new Set(rows.map((r) => r.productId));

  return (
    <AdminShell>
      <h1>ফ্ল্যাশ সেল — Flash sales</h1>
      <p>
        নির্দিষ্ট সময়ের দাম-ছাড়: সেল চলাকালীন কার্ট ও চেকআউটে সেল মূল্যই
        চার্জ হয়, আর বিক্রেতার পে-আউটও সেই মূল্য থেকেই হিসাব হয়। একসাথে
        একটির বেশি সক্রিয় সেল চলতে পারে না। (Time-boxed price drops: while
        a sale is live the cart and checkout charge the sale price and the
        seller payout derives from it. Only one active sale may cover any
        instant.)
      </p>
      {error && (
        <p role="alert" className="merch-error">
          {error}
        </p>
      )}
      {notice && <p>{notice}</p>}

      <form onSubmit={onSubmit} className="merch-form">
        <h2>
          {editingId
            ? "ফ্ল্যাশ সেল সম্পাদনা — Edit flash sale"
            : "নতুন ফ্ল্যাশ সেল — New flash sale"}
        </h2>
        <label>
          শিরোনাম — Title (required)
          <input
            value={title}
            onChange={(e) => setTitle(e.target.value)}
            required
            maxLength={120}
          />
        </label>
        <div className="merch-form-row">
          <label>
            শুরু — Starts at (required)
            <input
              type="datetime-local"
              value={startsAt}
              onChange={(e) => setStartsAt(e.target.value)}
              required
            />
          </label>
          <label>
            শেষ — Ends at (required)
            <input
              type="datetime-local"
              value={endsAt}
              onChange={(e) => setEndsAt(e.target.value)}
              required
            />
          </label>
          <label>
            <input
              type="checkbox"
              checked={isActive}
              onChange={(e) => setIsActive(e.target.checked)}
            />{" "}
            সক্রিয় — Active
          </label>
        </div>
        <p>
          <Button type="submit" variant="primary" loading={busy}>
            {editingId ? "আপডেট করুন — Update" : "তৈরি করুন — Create"}
          </Button>{" "}
          {editingId && (
            <Button variant="ghost" onClick={resetForm}>
              বাতিল — Cancel edit
            </Button>
          )}
        </p>
      </form>

      {sales.length === 0 ? (
        <EmptyState title="কোনো ফ্ল্যাশ সেল নেই — No flash sales yet" />
      ) : (
        <table className="merch-table">
          <thead>
            <tr>
              <th>সেল — Sale</th>
              <th>উইন্ডো — Window</th>
              <th>অবস্থা — State</th>
              <th>পদক্ষেপ — Actions</th>
            </tr>
          </thead>
          <tbody>
            {sales.map((sale) => (
              <tr key={sale.id}>
                <td>
                  <strong>{sale.title}</strong>
                  <div>{sale.item_count}টি আইটেম — items</div>
                </td>
                <td>
                  {formatDateTime(sale.starts_at)} → {formatDateTime(sale.ends_at)}
                </td>
                <td>
                  {sale.is_active ? (
                    <Badge tone="brand">সক্রিয় — Active</Badge>
                  ) : (
                    <Badge tone="muted">বন্ধ — Off</Badge>
                  )}{" "}
                  {sale.is_active && (
                    <Badge tone={saleState(sale) === "live" ? "flash" : "muted"}>
                      {saleState(sale) === "live"
                        ? "এখন লাইভ — Live now"
                        : saleState(sale) === "scheduled"
                          ? "সময়সূচিতে — Scheduled"
                          : "শেষ — Ended"}
                    </Badge>
                  )}
                </td>
                <td>
                  <div className="merch-actions">
                    <Button
                      size="sm"
                      variant="ghost"
                      onClick={() => void openItems(sale)}
                    >
                      আইটেম সাজান — Edit items
                    </Button>
                    <Button size="sm" variant="ghost" onClick={() => startEdit(sale)}>
                      সম্পাদনা — Edit
                    </Button>
                    <Button
                      size="sm"
                      variant="danger"
                      onClick={() => void onDelete(sale)}
                    >
                      মুছুন — Delete
                    </Button>
                  </div>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
      {total > PAGE_SIZE && (
        <p>
          মোট {total}টি — Total {total} · পৃষ্ঠা {page} /{" "}
          {Math.ceil(total / PAGE_SIZE)}{" "}
          <Button
            size="sm"
            variant="ghost"
            disabled={page <= 1}
            onClick={() => setPage((p) => Math.max(1, p - 1))}
          >
            ← আগের — Previous
          </Button>{" "}
          <Button
            size="sm"
            variant="ghost"
            disabled={page >= Math.ceil(total / PAGE_SIZE)}
            onClick={() => setPage((p) => p + 1)}
          >
            পরের — Next →
          </Button>
        </p>
      )}

      {itemTarget && (
        <section className="merch-editor">
          <h2>আইটেম — Items of “{itemTarget.title}”</h2>
          <p>
            সেল মূল্য অবশ্যই বর্তমান মূল্যের চেয়ে কম হতে হবে। স্টক ক্যাপ
            খালি রাখলে সীমাহীন। (A sale price must be below the current base
            price. An empty stock cap means uncapped.)
          </p>
          {rows.length === 0 ? (
            <EmptyState title="এখনো কোনো আইটেম নেই — No items yet" />
          ) : (
            <table className="merch-table">
              <thead>
                <tr>
                  <th>পণ্য — Product</th>
                  <th>বর্তমান মূল্য — Base</th>
                  <th>সেল মূল্য (৳) — Sale price</th>
                  <th>স্টক ক্যাপ — Cap</th>
                  <th>বিক্রি — Sold</th>
                  <th />
                </tr>
              </thead>
              <tbody>
                {rows.map((row) => (
                  <tr key={row.productId}>
                    <td>{row.productTitle}</td>
                    <td>{formatBdt(row.basePricePoisha)}</td>
                    <td>
                      <input
                        inputMode="decimal"
                        value={row.salePriceTaka}
                        onChange={(e) =>
                          updateRow(row.productId, { salePriceTaka: e.target.value })
                        }
                        aria-label={`সেল মূল্য — Sale price for ${row.productTitle}`}
                      />
                    </td>
                    <td>
                      <input
                        inputMode="numeric"
                        value={row.stockCap}
                        onChange={(e) =>
                          updateRow(row.productId, { stockCap: e.target.value })
                        }
                        placeholder="∞"
                        aria-label={`স্টক ক্যাপ — Stock cap for ${row.productTitle}`}
                      />
                    </td>
                    <td>
                      {row.soldAtSale}
                      {row.capRemaining !== null
                        ? ` (বাকি ${row.capRemaining} — left)`
                        : ""}
                    </td>
                    <td>
                      <Button
                        size="sm"
                        variant="danger"
                        onClick={() =>
                          setRows((current) =>
                            current.filter((r) => r.productId !== row.productId)
                          )
                        }
                      >
                        সরান — Remove
                      </Button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
          <p>
            <Button variant="primary" loading={savingItems} onClick={() => void onSaveItems()}>
              আইটেম সংরক্ষণ — Save items
            </Button>
          </p>

          <form onSubmit={onSearch} className="merch-form">
            <label>
              পণ্য খুঁজুন — Search products to add
              <input
                value={query}
                onChange={(e) => setQuery(e.target.value)}
                placeholder="শাড়ি, phone, …"
              />
            </label>
            <Button type="submit" variant="ghost">
              খুঁজুন — Search
            </Button>
          </form>
          {results.length > 0 && (
            <ul className="merch-member-list">
              {results.map((product) => (
                <li key={product.id} className="merch-member">
                  <span>
                    {product.title} — {formatBdt(product.price_poisha)} (
                    {product.store_slug})
                  </span>
                  <Button
                    size="sm"
                    disabled={rowIds.has(product.id)}
                    onClick={() =>
                      setRows((current) => [
                        ...current,
                        {
                          productId: product.id,
                          productTitle: product.title,
                          basePricePoisha: product.price_poisha,
                          salePriceTaka: String(product.price_poisha / 100),
                          stockCap: "",
                          soldAtSale: 0,
                          capRemaining: null,
                        },
                      ])
                    }
                  >
                    {rowIds.has(product.id)
                      ? "যোগ হয়েছে — Added"
                      : "যোগ করুন — Add"}
                  </Button>
                </li>
              ))}
            </ul>
          )}
        </section>
      )}
    </AdminShell>
  );
}

/** One editable item row of the flash sale editor. */
interface EditorRow {
  productId: string;
  productTitle: string;
  basePricePoisha: number;
  /** The sale price as typed, in taka (converted on save). */
  salePriceTaka: string;
  /** The cap as typed; empty = uncapped. */
  stockCap: string;
  /** Units already sold at the sale price (API accounting). */
  soldAtSale: number;
  capRemaining: number | null;
}
