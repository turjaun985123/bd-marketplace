"use client";

import { useEffect, useState } from "react";
import ProductRail from "./ProductRail";
import RecentlyViewedRail from "./RecentlyViewedRail";
import { getRecommendations, type Product } from "@/lib/api";
import { useAuth } from "@/hooks/useAuth";

/**
 * The home page's discovery strips (Wave 24), in one client
 * component so the home page itself stays a server
 * component:
 *
 * - "আপনার জন্য" — the personalised rail. Rendered only for
 *   a signed-in visitor whose rail is actually personalised
 *   (the API labels the top-rated fallback as such, and a
 *   fallback is not presented as "for you") and non-empty.
 * - "সম্প্রতি দেখেছেন" — recently viewed, the shared
 *   RecentlyViewedRail (Wave 53): the device-local history
 *   rendered from its card snapshots, with its clear
 *   button. The history is never sent to the server.
 *
 * Either strip renders nothing when it has nothing to show.
 */
export default function HomeDiscovery() {
  const { ready, token } = useAuth();
  const [forYou, setForYou] = useState<Product[]>([]);

  useEffect(() => {
    if (!ready || !token) return;
    let cancelled = false;
    getRecommendations(token)
      .then((result) => {
        if (!cancelled && result.personalized) {
          setForYou(result.items);
        }
      })
      .catch(() => {
        // Discovery is an enhancement; the catalogue below
        // is the page. A failed rail stays hidden.
      });
    return () => {
      cancelled = true;
    };
  }, [ready, token]);

  return (
    <>
      <ProductRail title="আপনার জন্য — For you" products={forYou} />
      <RecentlyViewedRail title="সম্প্রতি দেখেছেন — Recently viewed" />
    </>
  );
}
