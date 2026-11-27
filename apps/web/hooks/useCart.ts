"use client";

import { useCallback, useEffect, useState } from "react";
import {
  addCartItem,
  clearCart as clearCartApi,
  clearSavedItems,
  getCart,
  moveAllSavedToCart,
  removeCartItem,
  saveCartItem,
  unsaveCartItem,
  updateCartItem,
  type Cart,
  type CartItem,
} from "@/lib/api";
import { useAuth } from "@/hooks/useAuth";

export interface CartState {
  items: CartItem[];
  /** Lines parked on the save-for-later shelf (Wave 55). */
  savedItems: CartItem[];
  goodsTotalPoisha: number;
  /** Total unit count across all lines (for badges/labels). */
  count: number;
  loading: boolean;
  /** Last cart error, in the API's message (Bangla UI shows it as-is). */
  error: string | null;
  refresh: () => Promise<void>;
  /** `variantId` (Wave 33) picks the variant the new line buys. */
  add: (
    productId: string,
    quantity?: number,
    variantId?: string | null
  ) => Promise<void>;
  /** Lines are (product, variant) since Wave 33 — pass the line's variant. */
  setQuantity: (
    productId: string,
    quantity: number,
    variantId?: string | null
  ) => Promise<void>;
  remove: (productId: string, variantId?: string | null) => Promise<void>;
  clear: () => Promise<void>;
  /** Park a line on the save-for-later shelf (Wave 55). */
  moveToSaved: (productId: string, variantId?: string | null) => Promise<void>;
  /** Move a saved line back into the active cart (Wave 55). */
  moveToCart: (productId: string, variantId?: string | null) => Promise<void>;
  /** Bring the whole shelf back into the cart (Wave 55). */
  moveAllSaved: () => Promise<void>;
  /** Empty the shelf; the active cart is untouched (Wave 55). */
  clearSaved: () => Promise<void>;
}

const EMPTY_CART: Cart = { items: [], saved_items: [], goods_total_poisha: 0 };

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
    (productId: string, quantity = 1, variantId?: string | null) =>
      mutate((t) => addCartItem(t, productId, quantity, variantId)),
    [mutate]
  );
  const setQuantity = useCallback(
    (productId: string, quantity: number, variantId?: string | null) =>
      mutate((t) => updateCartItem(t, productId, quantity, variantId)),
    [mutate]
  );
  const remove = useCallback(
    (productId: string, variantId?: string | null) =>
      mutate((t) => removeCartItem(t, productId, variantId)),
    [mutate]
  );
  const clear = useCallback(() => mutate((t) => clearCartApi(t)), [mutate]);
  const moveToSaved = useCallback(
    (productId: string, variantId?: string | null) =>
      mutate((t) => saveCartItem(t, productId, variantId)),
    [mutate]
  );
  const moveToCart = useCallback(
    (productId: string, variantId?: string | null) =>
      mutate((t) => unsaveCartItem(t, productId, variantId)),
    [mutate]
  );
  const moveAllSaved = useCallback(
    () => mutate((t) => moveAllSavedToCart(t)),
    [mutate]
  );
  const clearSaved = useCallback(
    () => mutate((t) => clearSavedItems(t)),
    [mutate]
  );

  return {
    items: cart.items,
    savedItems: cart.saved_items,
    goodsTotalPoisha: cart.goods_total_poisha,
    count: cart.items.reduce((sum, item) => sum + item.quantity, 0),
    loading,
    error,
    refresh,
    add,
    setQuantity,
    remove,
    clear,
    moveToSaved,
    moveToCart,
    moveAllSaved,
    clearSaved,
  };
}
