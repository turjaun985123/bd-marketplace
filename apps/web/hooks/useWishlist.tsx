"use client";

import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useState,
  type ReactNode,
} from "react";
import {
  addToWishlist,
  getWishlistIds,
  removeFromWishlist,
} from "@/lib/api";
import { useAuth } from "./useAuth";

export interface WishlistState {
  /** True once the saved ids have loaded (or it is clear there is no session). */
  ready: boolean;
  /** The saved product ids — the one set every heart on the page reads. */
  ids: ReadonlySet<string>;
  isWishlisted: (productId: string) => boolean;
  /** Flip one product's saved state (optimistic; reverts when the API refuses). */
  toggle: (productId: string) => Promise<void>;
}

const WishlistContext = createContext<WishlistState>({
  ready: false,
  ids: new Set<string>(),
  isWishlisted: () => false,
  toggle: async () => {},
});

/**
 * The wishlist provider (Wave 24): ONE fetch of the saved ids
 * per session, shared by every heart toggle on the page —
 * cards never fetch their own state. Mounted in the root
 * layout, so marketplace, storefront and search hearts all
 * read the same set. Signed-out visitors get an empty set
 * (the button itself offers the login link in that case).
 */
export function WishlistProvider({ children }: { children: ReactNode }) {
  const { ready: authReady, token } = useAuth();
  const [ready, setReady] = useState(false);
  const [ids, setIds] = useState<ReadonlySet<string>>(new Set());

  useEffect(() => {
    if (!authReady) return;
    if (!token) {
      setIds(new Set());
      setReady(true);
      return;
    }
    let cancelled = false;
    setReady(false);
    getWishlistIds(token)
      .then((result) => {
        if (!cancelled) {
          setIds(new Set(result.product_ids));
          setReady(true);
        }
      })
      .catch(() => {
        // A failed load leaves an empty set; the wishlist page
        // does its own fetch and can surface the error there.
        if (!cancelled) setReady(true);
      });
    return () => {
      cancelled = true;
    };
  }, [authReady, token]);

  const toggle = useCallback(
    async (productId: string) => {
      if (!token) return;
      const wasSaved = ids.has(productId);
      // Optimistic: flip first so the heart answers instantly.
      setIds((prev) => {
        const next = new Set(prev);
        if (wasSaved) {
          next.delete(productId);
        } else {
          next.add(productId);
        }
        return next;
      });
      try {
        const result = wasSaved
          ? await removeFromWishlist(token, productId)
          : await addToWishlist(token, productId);
        // The API's answer is the truth — reconcile with it.
        setIds((prev) => {
          const next = new Set(prev);
          if (result.wishlisted) {
            next.add(productId);
          } else {
            next.delete(productId);
          }
          return next;
        });
      } catch {
        // Refused (e.g. the product stopped being visible):
        // revert to the state before the tap.
        setIds((prev) => {
          const next = new Set(prev);
          if (wasSaved) {
            next.add(productId);
          } else {
            next.delete(productId);
          }
          return next;
        });
      }
    },
    [ids, token]
  );

  const value = useMemo<WishlistState>(
    () => ({
      ready,
      ids,
      isWishlisted: (productId: string) => ids.has(productId),
      toggle,
    }),
    [ready, ids, toggle]
  );

  return (
    <WishlistContext.Provider value={value}>
      {children}
    </WishlistContext.Provider>
  );
}

/** Read the shared wishlist state (Wave 24). */
export function useWishlist(): WishlistState {
  return useContext(WishlistContext);
}
