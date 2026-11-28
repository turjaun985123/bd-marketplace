"use client";

import { useEffect, useState } from "react";
import Link from "next/link";
import FollowButton from "./FollowButton";
import StoreNav from "./StoreNav";
import StoreStats from "./StoreStats";
import VerificationBadge from "./VerificationBadge";
import { useAuth } from "@/hooks/useAuth";
import { getStore, type PublicStore } from "@/lib/api";

/**
 * The shop page's store header (Wave 53): the store's
 * identity block — banner, logo, name with its
 * verification badge, description — in the classic
 * presentation, above the faceted catalogue. The
 * storefront page keeps its themed StorefrontView; this
 * header is the shop page's own, kept deliberately
 * simple: the shop page's content is the catalogue, the
 * header only says whose it is. A failed load renders
 * just the way back — the catalogue below still works.
 */
export default function StoreShopHeader({
  slug,
  navCurrent = "shop",
}: {
  slug: string;
  /** Which nav entry is current (Wave 59): the shop
   * page's own default; the content pages pass theirs. */
  navCurrent?: string;
}) {
  const { token } = useAuth();
  const [store, setStore] = useState<PublicStore | null>(null);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    let cancelled = false;
    getStore(slug, token)
      .then((data) => {
        if (!cancelled) setStore(data);
      })
      .catch(() => {
        if (!cancelled) setFailed(true);
      });
    return () => {
      cancelled = true;
    };
  }, [slug, token]);

  return (
    <header className="store-shop-header">
      <p>
        <Link href={`/${slug}`}>← স্টোরফ্রন্ট — Storefront</Link>
      </p>
      {failed ? null : store === null ? (
        <p>স্টোর লোড হচ্ছে… (Loading store…)</p>
      ) : (
        <>
          {store.banner_url && (
            // eslint-disable-next-line @next/next/no-img-element
            <img className="storefront-banner" src={store.banner_url} alt="" />
          )}
          <div className="storefront-header">
            {store.logo_url && (
              // eslint-disable-next-line @next/next/no-img-element
              <img className="storefront-logo" src={store.logo_url} alt="" />
            )}
            <h1>{store.name}</h1>
            <VerificationBadge tier={store.verified_tier} />
          </div>
          {store.description && <p>{store.description}</p>}
          {store.announcement && (
            <p className="announcement-bar">{store.announcement}</p>
          )}
          {/* Wave 54: the trust line + follow action, the
              same as the storefront's. */}
          <StoreStats store={store} />
          <p>
            <FollowButton
              slug={store.slug}
              storeOwnerId={store.owner_user_id}
              initialFollowing={store.is_following}
              initialCount={store.follower_count}
            />
          </p>
          {/* Wave 59: the store navigation rides the
              shop header too — the content pages render
              this same header, so the nav is the frame
              every store surface shares. */}
          <StoreNav slug={slug} current={navCurrent} />
        </>
      )}
    </header>
  );
}
