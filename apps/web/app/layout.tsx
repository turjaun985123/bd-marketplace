import type { Metadata } from "next";
import "./globals.css";
import { WishlistProvider } from "@/hooks/useWishlist";

export const metadata: Metadata = {
  title: "বিডি মার্কেটপ্লেস — BD Marketplace",
  description:
    "Bangladesh-first marketplace: every seller gets a storefront, every product is in one central marketplace.",
};

export default function RootLayout({
  children,
}: {
  children: React.ReactNode;
}) {
  return (
    <html lang="bn">
      <body>
        {/* Wave 24: one shared wishlist state for every heart
            toggle in the app (see hooks/useWishlist). */}
        <WishlistProvider>{children}</WishlistProvider>
      </body>
    </html>
  );
}
