"use client";

import { useCallback, useEffect, useState } from "react";
import {
  addCartItem,
  clearCart as clearCartApi,
  getCart,
  removeCartItem,
  updateCartItem,
  type Cart,
  type CartItem,
} from "@/lib/api";
import { useAuth } from "@/hooks/useAuth";

export interface CartState {
  items: CartItem[];
  goodsTotalPoisha: number;
  /** Total unit count across all lines (for badges/labels). */
  count: number;
  loading: boolean;
  /** Last cart error, in the API's message (Bangla UI shows it as-is). */
  error: string | null;
  refresh: () => Promise<void>;
  add: (productId: string, quantity?: number) => Promise<void>;
  setQuantity: (productId: string, quantity: number) => Promise<void>;
  remove: (productId: string) => Promise<void>;
  clear: () => Promise<void>;
}

const EMPTY_CART: Cart = { items: [], goods_total_poisha: 0 };

/**
 * Shared cart hook — the ONLY place the web app calls the cart
 * endpoints (via the typed client in lib/api.ts). The marketplace grid,
 * the /cart page and checkout all use this hook; each page gets its own
 * instance, refreshed from the API (the server is the source of truth).
 */
export function useCart(): CartState {
  const { token } = useAuth();
  const [cart, setCart] = useState<Cart>(EMPTY_CART);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const apply = useCallback((next: Cart) => {
    setCart(next);
    setError(null);
  }, []);

  const refresh = useCallback(async () => {
    if (!token) {
      setCart(EMPTY_CART);
      return;
    }
    setLoading(true);
    try {
      apply(await getCart(token));
    } catch (err) {
      setError(err instanceof Error ? err.message : "কার্ট লোড করা যায়নি।");
    } finally {
      setLoading(false);
    }
  }, [token, apply]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const mutate = useCallback(
    async (fn: (token: string) => Promise<Cart>) => {
      if (!token) {
        setError("কার্ট ব্যবহার করতে আগে লগইন করুন।");
        return;
      }
      setLoading(true);
      try {
        apply(await fn(token));
      } catch (err) {
        setError(err instanceof Error ? err.message : "কার্ট আপডেট করা যায়নি।");
      } finally {
        setLoading(false);
      }
    },
    [token, apply]
  );

  const add = useCallback(
    (productId: string, quantity = 1) =>
      mutate((t) => addCartItem(t, productId, quantity)),
    [mutate]
  );
  const setQuantity = useCallback(
    (productId: string, quantity: number) =>
      mutate((t) => updateCartItem(t, productId, quantity)),
    [mutate]
  );
  const remove = useCallback(
    (productId: string) => mutate((t) => removeCartItem(t, productId)),
    [mutate]
  );
  const clear = useCallback(() => mutate((t) => clearCartApi(t)), [mutate]);

  return {
    items: cart.items,
    goodsTotalPoisha: cart.goods_total_poisha,
    count: cart.items.reduce((sum, item) => sum + item.quantity, 0),
    loading,
    error,
    refresh,
    add,
    setQuantity,
    remove,
    clear,
  };
}
