import type { ReactNode } from "react";
import NotificationBell from "@/components/NotificationBell";

/**
 * Storefront frame (Wave 54 extension): the storefront
 * group previously had no layout of its own — this one
 * exists to carry the signed-in visitor's notification
 * bell onto store pages too, since following a store is
 * done here and the notices it produces should be one
 * tap away here as well. The bell renders nothing for
 * signed-out visitors.
 */
export default function StorefrontLayout({
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
