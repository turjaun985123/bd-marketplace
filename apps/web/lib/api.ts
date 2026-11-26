/**
 * The single typed client for the Rust API — every page and hook calls
 * these functions instead of hand-writing fetch calls. One operation =
 * one function, mirroring the API's one operation = one endpoint rule.
 * Money stays integer poisha, exactly as the API returns it.
 */

export interface User {
  id: string;
  phone: string;
  name: string | null;
  role: string;
  created_at: string;
}

export interface Store {
  id: string;
  owner_user_id: string;
  slug: string;
  name: string;
  description: string | null;
  created_at: string;
}

export interface Product {
  id: string;
  store_id: string;
  store_slug: string;
  title: string;
  title_bn: string | null;
  description: string | null;
  price_poisha: number;
  compare_at_poisha: number | null;
  stock: number;
  status: string;
  created_at: string;
  updated_at: string;
}

export interface ProductList {
  items: Product[];
  total: number;
  page: number;
  limit: number;
}

export interface RequestOtpResult {
  phone: string;
  /** Present only when the API runs in dev mode — for local testing. */
  dev_otp?: string;
}

export interface VerifyOtpResult {
  token: string;
  user: User;
}

export interface CreateProductInput {
  store_id: string;
  title: string;
  title_bn?: string;
  description?: string;
  price_poisha: number;
  compare_at_poisha?: number;
  stock: number;
  status: string;
}

/** One cart line (flattened API shape: row fields + line_total_poisha). */
export interface CartItem {
  product_id: string;
  store_id: string;
  store_slug: string;
  title: string;
  title_bn: string | null;
  unit_price_poisha: number;
  quantity: number;
  stock: number;
  line_total_poisha: number;
}

export interface Cart {
  items: CartItem[];
  goods_total_poisha: number;
}

export interface OrderItem {
  id: string;
  sub_order_id: string;
  product_id: string;
  title_snapshot: string;
  unit_price_poisha: number;
  quantity: number;
  line_total_poisha: number;
}

export interface SubOrder {
  id: string;
  order_id: string;
  store_id: string;
  store_slug: string;
  status: string;
  goods_subtotal_poisha: number;
  delivery_charge_poisha: number;
  commission_poisha: number;
  seller_payout_poisha: number;
  created_at: string;
  items: OrderItem[];
}

export interface Order {
  id: string;
  buyer_user_id: string;
  status: string;
  payment_method: string;
  delivery_name: string;
  delivery_phone: string;
  delivery_address: string;
  delivery_zone: string;
  goods_total_poisha: number;
  delivery_total_poisha: number;
  grand_total_poisha: number;
  created_at: string;
  sub_orders: SubOrder[];
}

export interface OrderList {
  items: Order[];
  total: number;
  page: number;
  limit: number;
}

/** A sub-order as the seller sees it (with the delivery details). */
export interface SellerSubOrder {
  id: string;
  order_id: string;
  store_id: string;
  store_slug: string;
  status: string;
  goods_subtotal_poisha: number;
  delivery_charge_poisha: number;
  commission_poisha: number;
  seller_payout_poisha: number;
  created_at: string;
  delivery_name: string;
  delivery_phone: string;
  delivery_address: string;
  delivery_zone: string;
  items: OrderItem[];
}

export interface SellerSubOrderList {
  items: SellerSubOrder[];
  total: number;
  page: number;
  limit: number;
}

/** Checkout payload. Wave 3: payment_method is "cod" only. */
export interface PlaceOrderInput {
  delivery_name: string;
  delivery_phone: string;
  delivery_address: string;
  /** inside_dhaka | dhaka_suburb | outside_dhaka */
  delivery_zone: string;
  payment_method: string;
}

function apiBase(): string {
  return process.env.NEXT_PUBLIC_API_BASE_URL ?? "http://localhost:8080";
}

async function call<T>(
  path: string,
  options: { method?: string; token?: string | null; body?: unknown } = {}
): Promise<T> {
  const headers: Record<string, string> = {};
  if (options.body !== undefined) headers["Content-Type"] = "application/json";
  if (options.token) headers["Authorization"] = `Bearer ${options.token}`;
  const res = await fetch(`${apiBase()}${path}`, {
    method: options.method ?? "GET",
    headers,
    body: options.body !== undefined ? JSON.stringify(options.body) : undefined,
  });
  const data = (await res.json().catch(() => ({}))) as { error?: string } & T;
  if (!res.ok) {
    throw new Error(data.error ?? `API returned ${res.status}`);
  }
  return data;
}

/** POST /v1/auth/request-otp — step 1 of phone login. */
export function requestOtp(phone: string): Promise<RequestOtpResult> {
  return call("/v1/auth/request-otp", { method: "POST", body: { phone } });
}

/** POST /v1/auth/verify-otp — step 2: returns the JWT + user. */
export function verifyOtp(phone: string, code: string): Promise<VerifyOtpResult> {
  return call("/v1/auth/verify-otp", { method: "POST", body: { phone, code } });
}

/** GET /v1/stores/{slug} — public store profile (also resolves the id). */
export function getStore(slug: string): Promise<Store> {
  return call(`/v1/stores/${encodeURIComponent(slug)}`);
}

/** GET /v1/products — central marketplace listing (active products). */
export function listProducts(params?: {
  q?: string;
  store_slug?: string;
  page?: number;
  limit?: number;
  token?: string | null;
}): Promise<ProductList> {
  const search = new URLSearchParams();
  if (params?.q) search.set("q", params.q);
  if (params?.store_slug) search.set("store_slug", params.store_slug);
  if (params?.page) search.set("page", String(params.page));
  if (params?.limit) search.set("limit", String(params.limit));
  const qs = search.toString();
  return call(`/v1/products${qs ? `?${qs}` : ""}`, { token: params?.token });
}

/** GET /v1/stores/{slug}/products — one store's storefront listing. */
export function getStoreProducts(
  slug: string,
  token?: string | null
): Promise<ProductList> {
  return call(`/v1/stores/${encodeURIComponent(slug)}/products`, { token });
}

/** POST /v1/products — create a product (seller, own store only). */
export function createProduct(
  token: string,
  input: CreateProductInput
): Promise<Product> {
  return call("/v1/products", { method: "POST", token, body: input });
}

/** GET /v1/cart — the signed-in buyer's cart. */
export function getCart(token: string): Promise<Cart> {
  return call("/v1/cart", { token });
}

/** POST /v1/cart/items — add (or accumulate) a product in the cart. */
export function addCartItem(
  token: string,
  productId: string,
  quantity: number
): Promise<Cart> {
  return call("/v1/cart/items", {
    method: "POST",
    token,
    body: { product_id: productId, quantity },
  });
}

/** PATCH /v1/cart/items/{productId} — set a line's quantity (0 removes). */
export function updateCartItem(
  token: string,
  productId: string,
  quantity: number
): Promise<Cart> {
  return call(`/v1/cart/items/${encodeURIComponent(productId)}`, {
    method: "PATCH",
    token,
    body: { quantity },
  });
}

/** DELETE /v1/cart/items/{productId} — remove one line. */
export function removeCartItem(token: string, productId: string): Promise<Cart> {
  return call(`/v1/cart/items/${encodeURIComponent(productId)}`, {
    method: "DELETE",
    token,
  });
}

/** DELETE /v1/cart — empty the cart. */
export function clearCart(token: string): Promise<Cart> {
  return call("/v1/cart", { method: "DELETE", token });
}

/** POST /v1/orders — checkout (COD in Wave 3). Returns the placed order. */
export function placeOrder(token: string, input: PlaceOrderInput): Promise<Order> {
  return call("/v1/orders", { method: "POST", token, body: input });
}

/** GET /v1/orders — the buyer's own orders, newest first. */
export function listOrders(
  token: string,
  params?: { page?: number; limit?: number }
): Promise<OrderList> {
  const search = new URLSearchParams();
  if (params?.page) search.set("page", String(params.page));
  if (params?.limit) search.set("limit", String(params.limit));
  const qs = search.toString();
  return call(`/v1/orders${qs ? `?${qs}` : ""}`, { token });
}

/** GET /v1/orders/{id} — one of the buyer's own orders. */
export function getOrder(token: string, id: string): Promise<Order> {
  return call(`/v1/orders/${encodeURIComponent(id)}`, { token });
}

/** GET /v1/seller/sub-orders — sub-orders for the seller's own stores. */
export function listSellerSubOrders(
  token: string,
  params?: { page?: number; limit?: number }
): Promise<SellerSubOrderList> {
  const search = new URLSearchParams();
  if (params?.page) search.set("page", String(params.page));
  if (params?.limit) search.set("limit", String(params.limit));
  const qs = search.toString();
  return call(`/v1/seller/sub-orders${qs ? `?${qs}` : ""}`, { token });
}

/** PATCH /v1/seller/sub-orders/{id}/status — move a sub-order's status. */
export function updateSubOrderStatus(
  token: string,
  id: string,
  status: string
): Promise<SubOrder> {
  return call(`/v1/seller/sub-orders/${encodeURIComponent(id)}/status`, {
    method: "PATCH",
    token,
    body: { status },
  });
}
