import type { Metadata } from "next";
import "./globals.css";

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
      <body>{children}</body>
    </html>
  );
}
