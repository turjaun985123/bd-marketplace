import type { ReactNode } from "react";
import NotificationBell from "@/components/NotificationBell";

/**
 * Seller frame (Wave 54 extension): sellers are
 * notification recipients too — payout confirmations and
 * order events land in the same inbox — so the bell
 * rides the seller area as well, linking to the one
 * inbox. It renders nothing signed-out.
 */
export default function SellerLayout({
  children,
}: {
  children: ReactNode;
}) {
  return (
    <>
      <NotificationBell />
      {children}
    </>
  );
}
