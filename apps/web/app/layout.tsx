import type { Metadata, Viewport } from "next";
import "./globals.css";
import PwaRegister from "@/components/PwaRegister";
import { WishlistProvider } from "@/hooks/useWishlist";

export const metadata: Metadata = {
  title: "বিডি মার্কেটপ্লেস — BD Marketplace",
  description:
    "Bangladesh-first marketplace: every seller gets a storefront, every product is in one central marketplace.",
  // Wave 38 (PWA): the generated icons (scripts/make-icons.mjs)
  // and the iOS home-screen behaviour. The manifest itself
  // lives in app/manifest.ts — Next links it automatically.
  icons: {
    icon: [
      { url: "/icons/icon-192.png", sizes: "192x192", type: "image/png" },
      { url: "/icons/icon-512.png", sizes: "512x512", type: "image/png" },
    ],
    apple: [{ url: "/apple-touch-icon.png", sizes: "180x180", type: "image/png" }],
  },
  appleWebApp: {
    capable: true,
    title: "BD Marketplace",
    statusBarStyle: "default",
  },
};

/** Wave 38: theme colour is the brand green from globals.css. */
export const viewport: Viewport = {
  themeColor: "#1b5e20",
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
        {/* Wave 38: registers /sw.js in production (renders
            nothing — see components/PwaRegister). */}
        <PwaRegister />
      </body>
    </html>
  );
}
