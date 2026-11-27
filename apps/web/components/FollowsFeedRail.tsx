"use client";

import { useEffect, useState } from "react";
import ProductRail from "./ProductRail";
import { getFollowsFeed, type Product } from "@/lib/api";
import { useAuth } from "@/hooks/useAuth";

/**
 * The home page's personal rail (Wave 54 extension): the
 * newest products from the stores the signed-in visitor
 * follows. Signed-out visitors and buyers who follow
 * nothing see nothing — the rail never pads itself with
 * catalogue filler (the anonymous home feed below is the
 * discovery surface; this one is strictly the follow
 * graph's output).
 */
export default function FollowsFeedRail() {
  const { token } = useAuth();
  const [products, setProducts] = useState<Product[]>([]);

  useEffect(() => {
    if (!token) {
      setProducts([]);
      return;
    }
    let cancelled = false;
    getFollowsFeed(token)
      .then((feed) => {
        if (!cancelled) setProducts(feed.items);
      })
      .catch(() => {
        if (!cancelled) setProducts([]);
      });
    return () => {
      cancelled = true;
    };
  }, [token]);

  if (!token || products.length === 0) return null;

  return (
    <ProductRail
      title="ফলো করা স্টোরে নতুন — New from stores you follow"
      products={products}
      linkHref="/following"
      linkLabel="ফলো করা স্টোর — Following"
    />
  );
}
