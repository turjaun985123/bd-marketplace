"use client";

import { useEffect, useState } from "react";
import Link from "next/link";
import VerificationBadge from "./VerificationBadge";
import { getStore, type Store } from "@/lib/api";

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
export default function StoreShopHeader({ slug }: { slug: string }) {
  const [store, setStore] = useState<Store | null>(null);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    let cancelled = false;
    getStore(slug)
      .then((data) => {
        if (!cancelled) setStore(data);
      })
      .catch(() => {
        if (!cancelled) setFailed(true);
      });
    return () => {
      cancelled = true;
    };
  }, [slug]);

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
        </>
      )}
    </header>
  );
}
