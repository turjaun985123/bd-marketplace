"use client";

import { useEffect } from "react";

/**
 * Registers the Wave 38 service worker (public/sw.js).
 * Production only: in `next dev` a worker would cache
 * stale dev bundles and confuse every edit, so dev keeps
 * the network-only behaviour developers expect. Renders
 * nothing; mounted once by the root layout.
 */
export default function PwaRegister() {
  useEffect(() => {
    if (process.env.NODE_ENV !== "production") return;
    if (!("serviceWorker" in navigator)) return;
    const register = () => {
      navigator.serviceWorker.register("/sw.js").catch(() => {
        /* a failed registration must never break the page —
           the site simply stays a plain website */
      });
    };
    if (document.readyState === "complete") {
      register();
    } else {
      window.addEventListener("load", register, { once: true });
      return () => window.removeEventListener("load", register);
    }
  }, []);
  return null;
}
