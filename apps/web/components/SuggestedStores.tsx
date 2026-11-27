"use client";

import { useEffect, useState } from "react";
import Link from "next/link";
import FollowButton from "./FollowButton";
import Stars from "./Stars";
import { getSuggestedStores, type SuggestedStore } from "@/lib/api";
import { useAuth } from "@/hooks/useAuth";

/**
 * Store suggestions (Wave 54 extension): active stores
 * the signed-in buyer does not own or follow yet, ranked
 * by the API on real signals — follower count, then
 * buyer ratings. Rendered on the /following page under
 * the buyer's own shelf: the place a buyer managing
 * their follows is most likely to want one more good
 * shop. No paid placement exists in this list —
 * promotions are a separate, labelled surface.
 */
export default function SuggestedStores() {
  const { token } = useAuth();
  const [stores, setStores] = useState<SuggestedStore[] | null>(null);

  useEffect(() => {
    if (!token) return;
    let cancelled = false;
    getSuggestedStores(token)
      .then((list) => {
        if (!cancelled) setStores(list.items);
      })
      .catch(() => {
        if (!cancelled) setStores([]);
      });
    return () => {
      cancelled = true;
    };
  }, [token]);

  if (!token || stores === null || stores.length === 0) return null;

  return (
    <section className="suggested-stores">
      <h2>আরও ভালো স্টোর — Stores you may like</h2>
      {stores.map((store) => (
        <article key={store.id} className="card suggested-store">
          <h3>
            <Link href={`/${store.slug}`}>{store.name}</Link>
          </h3>
          <p>
            {store.rating_count > 0 ? (
              <>
                <Stars value={store.rating_avg} />{" "}
                {store.rating_avg?.toFixed(1)} ({store.rating_count}টি রিভিউ)
                {" · "}
              </>
            ) : null}
            {store.follower_count.toLocaleString("en-IN")} জন ফলোয়ার ·{" "}
            {store.product_count.toLocaleString("en-IN")}টি পণ্য
          </p>
          <p>
            <FollowButton
              slug={store.slug}
              storeOwnerId={store.owner_user_id}
              initialFollowing={false}
              initialCount={store.follower_count}
            />
          </p>
        </article>
      ))}
    </section>
  );
}
