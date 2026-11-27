import FlashStrip from "@/components/FlashStrip";

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
      {children}
    </>
  );
}
