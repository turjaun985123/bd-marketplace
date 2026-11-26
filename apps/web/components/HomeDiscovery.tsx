"use client";

import { useEffect, useState } from "react";
import ProductRail from "./ProductRail";
import { getProduct, getRecommendations, type Product } from "@/lib/api";
import { useAuth } from "@/hooks/useAuth";
import { useRecentlyViewed } from "@/hooks/useRecentlyViewed";

/**
 * The home page's discovery strips (Wave 24), in one client
 * component so the home page itself stays a server
 * component:
 *
 * - "আপনার জন্য" — the personalised rail. Rendered only for
 *   a signed-in visitor whose rail is actually personalised
 *   (the API labels the top-rated fallback as such, and a
 *   fallback is not presented as "for you") and non-empty.
 * - "সম্প্রতি দেখেছেন" — recently viewed, resolved from the
 *   device-local id list (see useRecentlyViewed) through the
 *   public product endpoint. Ids whose product no longer
 *   resolves (deleted, hidden) are skipped silently.
 *
 * Either strip renders nothing when it has nothing to show.
 */
export default function HomeDiscovery() {
  const { ready, token } = useAuth();
  const recentIds = useRecentlyViewed();
  const [forYou, setForYou] = useState<Product[]>([]);
  const [recent, setRecent] = useState<Product[]>([]);

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

  useEffect(() => {
    if (recentIds.length === 0) return;
    let cancelled = false;
    Promise.all(
      recentIds.map((id) => getProduct(id).catch(() => null))
    ).then((products) => {
      if (!cancelled) {
        setRecent(
          products.filter((p): p is Product => p !== null)
        );
      }
    });
    return () => {
      cancelled = true;
    };
  }, [recentIds]);

  return (
    <>
      <ProductRail title="আপনার জন্য — For you" products={forYou} />
      <ProductRail
        title="সম্প্রতি দেখেছেন — Recently viewed"
        products={recent}
      />
    </>
  );
}
