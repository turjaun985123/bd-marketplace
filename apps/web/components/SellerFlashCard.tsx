"use client";

import Link from "next/link";
import { useEffect, useState } from "react";
import { getSellerFlashSale, type SellerFlashSale } from "@/lib/api";
import { formatBdt } from "@/lib/format";

/**
 * The seller dashboard's flash sale card (Wave 52): when a
 * platform flash sale is live AND carries this seller's
 * products, the dashboard surfaces it — the sale, how many
 * of the seller's products are in it, and the units already
 * sold at sale prices — with the way to the full seller
 * flash view. In every other state (no sale, or a sale with
 * none of their products) the card renders nothing: a
 * dashboard card that only ever says "nothing happening"
 * is noise, and the dedicated page already covers the
 * honest empty states.
 */
export default function SellerFlashCard({ token }: { token: string }) {
  const [sale, setSale] = useState<SellerFlashSale | null>(null);

  useEffect(() => {
    let cancelled = false;
    getSellerFlashSale(token)
      .then((data) => {
        if (!cancelled) setSale(data);
      })
      .catch(() => {
        if (!cancelled) setSale(null);
      });
    return () => {
      cancelled = true;
    };
  }, [token]);

  if (sale === null || sale.items.length === 0) return null;
  const soldTotal = sale.items.reduce((sum, item) => sum + item.sold_at_sale, 0);
  const deepest = sale.items.reduce((best, item) => {
    const off =
      item.base_price_poisha > 0
        ? Math.floor(
            ((item.base_price_poisha - item.sale_price_poisha) * 100) /
              item.base_price_poisha
          )
        : 0;
    return Math.max(best, off);
  }, 0);

  return (
    <section>
      <div className="card">
        <h3>⚡ ফ্ল্যাশ সেল চলছে — Flash sale live: {sale.title}</h3>
        <p>
          আপনার {sale.items.length}টি পণ্য এই সেলে আছে
          {deepest > 0 ? `, সর্বোচ্চ -${deepest}% ছাড়ে` : ""}। এখন পর্যন্ত
          সেল মূল্যে বিক্রি হয়েছে {soldTotal}টি। (Your {sale.items.length}{" "}
          products are in this sale
          {deepest > 0 ? `, up to ${deepest}% off` : ""}. {soldTotal} units
          sold at sale prices so far.)
        </p>
        <ul>
          {sale.items.slice(0, 3).map((item) => (
            <li key={item.product_id}>
              {item.product_title}: {formatBdt(item.sale_price_poisha)}{" "}
              <s>{formatBdt(item.base_price_poisha)}</s> · বিক্রি{" "}
              {item.sold_at_sale}
              {item.stock_cap !== null ? ` / ক্যাপ ${item.stock_cap}` : ""}
            </li>
          ))}
        </ul>
        <p>
          <Link href="/seller/flash-sale">
            বিস্তারিত দেখুন — View the flash sale
          </Link>
        </p>
      </div>
    </section>
  );
}
