import CategoryNav from "@/components/CategoryNav";
import FlashStrip from "@/components/FlashStrip";
import NotificationBell from "@/components/NotificationBell";

/**
 * The marketplace route group's frame (Wave 52): the
 * site-wide flash sale strip rides above every marketplace
 * page while a sale is live (and renders nothing when none
 * is). Seller, admin and storefront groups keep their own
 * frames — a platform campaign bar belongs to the shopping
 * surface, not to the back offices.
 */
export default function MarketplaceLayout({
  children,
}: {
  children: React.ReactNode;
}) {
  return (
    <>
      <FlashStrip />
      {/* Wave 56: the taxonomy navigation bar (quick roots
          + the mega panel) rides under the strip on every
          marketplace page. It renders nothing until the
          public tree loads, and nothing if it fails. */}
      <CategoryNav />
      {/* Wave 54 extension: the signed-in visitor's inbox
          bell (unread badge) rides the marketplace frame. */}
      <NotificationBell />
      {children}
    </>
  );
}
