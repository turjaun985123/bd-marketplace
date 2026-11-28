"use client";

import { useEffect, useState } from "react";
import Link from "next/link";
import PriceTag from "@/components/PriceTag";
import VerificationBadge from "@/components/VerificationBadge";
import {
  getBuyerInvoice,
  getSellerInvoice,
  paymentStatusLabel,
  type Invoice,
} from "@/lib/api";
import { useAuth } from "@/hooks/useAuth";

/** Zone values with their Bangla labels (display only). */
const ZONE_LABELS: Record<string, string> = {
  inside_dhaka: "ঢাকার ভিতরে",
  dhaka_suburb: "ঢাকার উপশহর",
  outside_dhaka: "ঢাকার বাইরে",
};

/**
 * Printable invoice for ONE sub-order (Wave 22) — the seller's
 * legal document for its slice of an order, distinct from the
 * buyer's whole-order receipt. The invoice number is sequential
 * per store and is issued by the API on the first read of a
 * delivered (or returned) sub-order; this page only renders it.
 *
 * One page serves both parties: the buyer orders list links
 * here plainly (buyer endpoint), the seller orders list links
 * with ?as=seller (seller endpoint). The Print button saves a
 * PDF through the browser's own print dialog — there is no
 * server-side PDF generator. The document title is set to the
 * invoice filename stem (invoice-<slug>-<seq>) so the browser
 * proposes it as the saved PDF's name.
 */
export default function InvoicePage({
  params,
}: {
  params: { subOrderId: string };
}) {
  const { ready, token } = useAuth();
  const [asSeller, setAsSeller] = useState(false);
  const [invoice, setInvoice] = useState<Invoice | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!token) return;
    const seller =
      new URLSearchParams(window.location.search).get("as") === "seller";
    setAsSeller(seller);
    let cancelled = false;
    const load = seller ? getSellerInvoice : getBuyerInvoice;
    load(token, params.subOrderId)
      .then((data) => {
        if (cancelled) return;
        setInvoice(data);
        document.title = `invoice-${data.store.slug}-${String(
          data.seq
        ).padStart(6, "0")}`;
      })
      .catch((err) => {
        if (!cancelled) {
          setError(
            err instanceof Error ? err.message : "ইনভয়েস লোড করা যায়নি।"
          );
        }
      });
    return () => {
      cancelled = true;
    };
  }, [token, params.subOrderId]);

  if (!ready) {
    return (
      <main>
        <p>লোড হচ্ছে…</p>
      </main>
    );
  }

  if (!token) {
    return (
      <main>
        <h1>ইনভয়েস — Invoice</h1>
        <p>
          ইনভয়েস দেখতে আগে লগইন করতে হবে।{" "}
          <Link href="/login">লগইন পেজে যান — go to login</Link>
        </p>
      </main>
    );
  }

  if (error) {
    return (
      <main>
        <h1>ইনভয়েস — Invoice</h1>
        <p role="alert">{error}</p>
        <p>
          <Link href={asSeller ? "/seller/orders" : "/orders"}>
            অর্ডারে ফিরে যান
          </Link>
        </p>
      </main>
    );
  }

  if (invoice === null) {
    return (
      <main>
        <p>ইনভয়েস লোড হচ্ছে…</p>
      </main>
    );
  }

  const accent = invoice.store.brand_color ?? undefined;

  return (
    <main>
      <p className="no-print">
        <Link href={asSeller ? "/seller/orders" : "/orders"}>
          ← অর্ডারে ফিরে যান
        </Link>
      </p>
      <article
        className="invoice"
        style={accent ? { borderTop: `6px solid ${accent}` } : undefined}
      >
        <header>
          <h1 style={accent ? { color: accent } : undefined}>
            {invoice.store.name}{" "}
            <VerificationBadge tier={invoice.store.verified_tier} />
          </h1>
          <p>
            স্টোর: {invoice.store.slug} · ইনভয়েস — Invoice{" "}
            <strong>{invoice.invoice_number}</strong>
          </p>
          <p>
            ইস্যুর তারিখ:{" "}
            {new Date(invoice.issued_at).toLocaleString("bn-BD")} ·
            সাব-অর্ডারের অবস্থা: {invoice.sub_order_status} · পেমেন্ট:{" "}
            {invoice.payment_method === "cod"
              ? "ক্যাশ অন ডেলিভারি (COD)"
              : invoice.payment_method}{" "}
            — {paymentStatusLabel(invoice.payment_status)}
          </p>
        </header>

        <h2>ক্রেতার তথ্য</h2>
        <p>
          {invoice.buyer.name} · {invoice.buyer.phone}
          <br />
          {invoice.buyer.address} ·{" "}
          {ZONE_LABELS[invoice.buyer.zone] ?? invoice.buyer.zone}
          {invoice.buyer.district && <> · জেলা: {invoice.buyer.district}</>}
        </p>
        {invoice.order_note && (
          <p>ক্রেতার নোট — Buyer note: {invoice.order_note}</p>
        )}

        <h2>পণ্যের বিবরণ</h2>
        <table>
          <thead>
            <tr>
              <th>পণ্য</th>
              <th className="num">পরিমাণ</th>
              <th className="num">একক মূল্য</th>
              <th className="num">মোট</th>
            </tr>
          </thead>
          <tbody>
            {invoice.items.map((item) => (
              <tr key={item.id}>
                <td>
                  {item.image_url ? (
                    <>
                      {/* eslint-disable-next-line @next/next/no-img-element */}
                      <img
                        src={item.image_url}
                        alt=""
                        style={{
                          height: 36,
                          width: 36,
                          objectFit: "cover",
                          borderRadius: 4,
                          verticalAlign: "middle",
                          marginRight: 8,
                        }}
                      />
                    </>
                  ) : null}
                  {item.title_snapshot}
                </td>
                <td className="num">{item.quantity}</td>
                <td className="num">
                  <PriceTag amountPoisha={item.unit_price_poisha} />
                </td>
                <td className="num">
                  <PriceTag amountPoisha={item.line_total_poisha} />
                </td>
              </tr>
            ))}
          </tbody>
        </table>

        <h2>হিসাব</h2>
        <p>
          পণ্যের উপমোট:{" "}
          <PriceTag amountPoisha={invoice.money.goods_subtotal_poisha} />
          {invoice.money.discount_poisha > 0 && (
            <>
              <br />
              কুপন ছাড়: −
              <PriceTag amountPoisha={invoice.money.discount_poisha} />
            </>
          )}
          {invoice.money.gift_card_discount_poisha > 0 && (
            <>
              <br />
              গিফট কার্ড: −
              <PriceTag amountPoisha={invoice.money.gift_card_discount_poisha} />
            </>
          )}
          <br />
          ডেলিভারি চার্জ:{" "}
          <PriceTag amountPoisha={invoice.money.delivery_charge_poisha} />
          <br />
          সর্বমোট:{" "}
          <strong>
            <PriceTag amountPoisha={invoice.money.grand_total_poisha} />
          </strong>
          {invoice.money.advance_paid_poisha > 0 && (
            <>
              <br />
              অগ্রিম পরিশোধিত:{" "}
              <PriceTag amountPoisha={invoice.money.advance_paid_poisha} />
            </>
          )}
        </p>
        {invoice.money.cod_amount_due_poisha > 0 && (
          <p className="cod-due">
            <strong>
              ডেলিভারির সময় পরিশোধ করতে হবে:{" "}
              <PriceTag amountPoisha={invoice.money.cod_amount_due_poisha} />
            </strong>
          </p>
        )}
        <p className="invoice-note">
          এই ইনভয়েসটি কম্পিউটারে তৈরি — কোনো স্বাক্ষরের প্রয়োজন নেই।
          ইনভয়েস নম্বরটি এই স্টোরের নিজস্ব ধারাবাহিক নম্বর; অর্ডার
          নম্বর: <code>{invoice.order_id}</code>
        </p>
      </article>
      <p className="no-print">
        <button type="button" onClick={() => window.print()}>
          প্রিন্ট / PDF — Print / Save as PDF
        </button>
      </p>
    </main>
  );
}
