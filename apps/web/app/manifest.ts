import type { MetadataRoute } from "next";

/**
 * The web app manifest (Wave 38) — what makes the
 * marketplace installable. Next 14 serves this from
 * app/manifest.ts as /manifest.webmanifest and links it
 * from every page automatically.
 *
 * theme_color is the brand green from app/globals.css
 * (#1b5e20 — the dark green of the verification-badge
 * palette; globals.css defines no --brand variable, so
 * this hex IS the brand colour today, and the generated
 * icons in public/icons/ use the same one). Icons are
 * drawn by scripts/make-icons.mjs — regenerate, never
 * hand-edit the PNGs.
 */
export default function manifest(): MetadataRoute.Manifest {
  return {
    name: "বাংলা মার্কেটপ্লেস — BD Marketplace",
    short_name: "BD Marketplace",
    description:
      "বাংলাদেশের মার্কেটপ্লেস — প্রতিটা বিক্রেতার নিজের স্টোরফ্রন্ট, সব পণ্য এক জায়গায়, ক্যাশ অন ডেলিভারি।",
    lang: "bn",
    start_url: "/",
    scope: "/",
    display: "standalone",
    background_color: "#ffffff",
    theme_color: "#1b5e20",
    icons: [
      {
        src: "/icons/icon-192.png",
        sizes: "192x192",
        type: "image/png",
        purpose: "any",
      },
      {
        src: "/icons/icon-512.png",
        sizes: "512x512",
        type: "image/png",
        purpose: "any",
      },
      {
        src: "/icons/icon-maskable-512.png",
        sizes: "512x512",
        type: "image/png",
        purpose: "maskable",
      },
    ],
  };
}
