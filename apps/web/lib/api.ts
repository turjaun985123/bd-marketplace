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
  /** Wave 10: platform admin flag, on top of the role. */
  is_admin: boolean;
  created_at: string;
}

export interface Store {
  id: string;
  owner_user_id: string;
  slug: string;
  name: string;
  description: string | null;
  /** active | suspended (Wave 10 admin kill switch). */
  status: string;
  /** Linked Facebook Page id (Wave 12); null when no page is linked. */
  facebook_page_id: string | null;
  /** Linked Facebook Page display name (Wave 12). */
  facebook_page_name: string | null;
  /**
   * SIDRA mode (Wave 13): true = AI replies post directly into the
   * conversation; false = they wait as seller-approved suggestions.
   */
  sidra_auto_reply: boolean;
  /**
   * Storefront theme (Wave 18): one of the StoreTheme keys —
   * a layout variant of the shared storefront view, never markup.
   */
  theme_key: StoreTheme;
  /** Brand accent colour, `#RRGGBB` uppercase, or null (Wave 18). */
  brand_color: string | null;
  /** Hotlinked logo image URL, or null (Wave 18). */
  logo_url: string | null;
  /** Hotlinked banner image URL, or null (Wave 18). */
  banner_url: string | null;
  /** One-line announcement shown atop the storefront, or null. */
  announcement: string | null;
  created_at: string;
}

/** The three storefront layout variants (Wave 18). */
export type StoreTheme = "classic" | "minimal" | "bold";

export interface Product {
  id: string;
  store_id: string;
  store_slug: string;
  title: string;
  title_bn: string | null;
  description: string | null;
  /** Free-text category label (Wave 15); null when uncategorised. */
  category: string | null;
  price_poisha: number;
  compare_at_poisha: number | null;
  stock: number;
  status: string;
  /** Weight of one unit in grams (Wave 4 — drives delivery charges). */
  weight_grams: number;
  /** manual | facebook — where the product came from (Wave 12). */
  source: string;
  /** The origin's id for imports (the Facebook post id); null otherwise. */
  source_ref: string | null;
  /** Mean review rating, one decimal (Wave 8); null when unreviewed. */
  rating_avg: number | null;
  /** Number of reviews (Wave 8). */
  rating_count: number;
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
  /** Optional category label (Wave 15). */
  category?: string;
  price_poisha: number;
  compare_at_poisha?: number;
  stock: number;
  status: string;
  /** Grams per unit; the API defaults to 1000 when omitted. */
  weight_grams?: number;
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
  /** Weight of one unit in grams (Wave 4). */
  weight_grams: number;
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

/** The courier shipment on a sub-order (Wave 6). */
export interface ShipmentSummary {
  id: string;
  courier: string;
  consignment_id: string;
  /** Unified shipment status (created, picked_up, in_transit, …). */
  status: string;
}

/** One courier booking (booking / refresh responses, Wave 6). */
export interface Shipment {
  id: string;
  sub_order_id: string;
  courier: string;
  consignment_id: string;
  tracking_code: string | null;
  status: string;
  /** Cash the courier collects on delivery, in poisha. */
  cod_amount_poisha: number;
  note: string | null;
  created_at: string;
  updated_at: string;
}

/** Availability of one courier (GET /v1/couriers, Wave 6). */
export interface CourierInfo {
  courier: string;
  available: boolean;
  /** "live" | "dev" | null — "dev" means the DEV simulator. */
  mode: string | null;
  /** Machine-readable reason when unavailable, e.g. "not_configured". */
  reason: string | null;
  is_default: boolean;
}

/**
 * One courier's delivery-charge estimate (GET /v1/couriers/estimate,
 * Wave 7). The price is the platform's local tariff estimate, never
 * a live courier quote, and it is null for an unavailable courier.
 */
export interface CourierEstimate {
  courier: string;
  available: boolean;
  /** "live" | "dev" | null — "dev" means the DEV simulator. */
  mode: string | null;
  /** Machine-readable reason when unavailable, e.g. "not_configured". */
  reason: string | null;
  price_poisha: number | null;
}

/**
 * Estimates for one zone + weight, plus the smart-routing
 * recommendation (cheapest available courier; platform priority
 * breaks ties; null when no courier is available).
 */
export interface CourierEstimates {
  zone: string;
  weight_grams: number;
  estimates: CourierEstimate[];
  recommended: string | null;
}

/** Bangla label for a unified shipment status (display only). */
export function shipmentStatusLabel(status: string): string {
  switch (status) {
    case "created":
      return "তৈরি হয়েছে";
    case "picked_up":
      return "পিক হয়েছে";
    case "in_transit":
      return "পথে আছে";
    case "out_for_delivery":
      return "ডেলিভারির জন্য বেরিয়েছে";
    case "delivered":
      return "ডেলিভারি হয়েছে";
    case "partial_delivered":
      return "আংশিক ডেলিভারি হয়েছে";
    case "return_in_transit":
      return "ফেরত আসছে";
    case "returned":
      return "ফেরত হয়েছে";
    case "cancelled":
      return "বাতিল";
    default:
      return "অজানা";
  }
}

export interface SubOrder {
  id: string;
  order_id: string;
  store_id: string;
  store_slug: string;
  status: string;
  goods_subtotal_poisha: number;
  /** Coupon discount allocated to this sub-order (Wave 14). */
  discount_poisha: number;
  delivery_charge_poisha: number;
  commission_poisha: number;
  seller_payout_poisha: number;
  created_at: string;
  items: OrderItem[];
  /** Courier shipment (Wave 6); null until the seller books one. */
  shipment: ShipmentSummary | null;
  /**
   * The buyer's return request status for this sub-order
   * (Wave 16: requested | approved | rejected | received);
   * null when no return was requested.
   */
  return_status: string | null;
}

/** The latest payment attempt on an order (Wave 5). */
export interface PaymentSummary {
  id: string;
  method: string;
  status: string;
  gateway_ref: string | null;
  /** Local DEV simulator path; present only for dev payments. */
  dev_redirect_url: string | null;
}

/** One payment attempt (webhook / retry / dev-simulate responses). */
export interface Payment {
  id: string;
  order_id: string;
  method: string;
  amount_poisha: number;
  status: string;
  gateway_ref: string | null;
  created_at: string;
  updated_at: string;
  paid_at: string | null;
}

/** Availability of one payment method (GET /v1/payments/methods). */
export interface PaymentMethodInfo {
  method: string;
  available: boolean;
  /** "cod" | "live" | "dev" | null — "dev" means the DEV simulator. */
  mode: string | null;
  /** Machine-readable reason when unavailable, e.g. "not_configured". */
  reason: string | null;
}

/** Bangla label for an order-level payment status (display only). */
export function paymentStatusLabel(paymentStatus: string): string {
  switch (paymentStatus) {
    case "paid":
      return "পরিশোধিত";
    case "pending":
      return "পেমেন্ট বাকি";
    case "failed":
      return "পেমেন্ট ব্যর্থ হয়েছে";
    case "refunded":
      return "ফেরত দেওয়া হয়েছে";
    case "advance_pending":
      return "অগ্রিম বাকি আছে";
    case "advance_paid":
      return "অগ্রিম পরিশোধিত (বাকি ডেলিভারিতে)";
    default:
      return "ডেলিভারিতে পেমেন্ট";
  }
}

export interface Order {
  id: string;
  buyer_user_id: string;
  status: string;
  payment_method: string;
  /**
   * pay_on_delivery | pending | paid | failed | refunded (Wave 5),
   * plus advance_pending | advance_paid for COD orders with an
   * advance (Wave 17).
   */
  payment_status: string;
  /**
   * Advance this COD order must collect before shipping (Wave 17);
   * 0 when none. Positive while payment_status is pay_on_delivery
   * means no gateway could collect it and the order proceeded
   * flagged — no money was taken.
   */
  advance_required_poisha: number;
  /** Advance actually collected online (Wave 17). */
  advance_paid_poisha: number;
  /** Latest payment attempt; null for COD orders. */
  payment: PaymentSummary | null;
  delivery_name: string;
  delivery_phone: string;
  delivery_address: string;
  delivery_zone: string;
  goods_total_poisha: number;
  /** Coupon code applied at checkout (Wave 14); null when none. */
  coupon_code: string | null;
  /** Total coupon discount across the sub-orders (Wave 14). */
  discount_total_poisha: number;
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
  /** Coupon discount allocated to this sub-order (Wave 14). */
  discount_poisha: number;
  delivery_charge_poisha: number;
  commission_poisha: number;
  seller_payout_poisha: number;
  created_at: string;
  delivery_name: string;
  delivery_phone: string;
  delivery_address: string;
  delivery_zone: string;
  items: OrderItem[];
  /** Courier shipment (Wave 6); null until the seller books one. */
  shipment: ShipmentSummary | null;
  /**
   * The parent order's payment status (Wave 17) — advance_pending
   * means the parcel must not ship yet (booking enforces it).
   */
  order_payment_status: string;
  /** Advance the parent COD order must collect (Wave 17). */
  advance_required_poisha: number;
  /** Advance the parent order has collected (Wave 17). */
  advance_paid_poisha: number;
  /** The buyer's fraud-shield level: low | medium | high (Wave 17). */
  buyer_risk_level: string;
}

export interface SellerSubOrderList {
  items: SellerSubOrder[];
  total: number;
  page: number;
  limit: number;
}

/** Checkout payload. Wave 5: cod | bkash | nagad | sslcommerz | shurjopay. */
export interface PlaceOrderInput {
  delivery_name: string;
  delivery_phone: string;
  delivery_address: string;
  /** inside_dhaka | dhaka_suburb | outside_dhaka */
  delivery_zone: string;
  payment_method: string;
  /** Optional coupon code (Wave 14); checkout re-validates it. */
  coupon_code?: string;
  /**
   * The online method for a COD advance (Wave 17). Needed only
   * when checkout decides this order requires an advance; when
   * it does and no method was sent, checkout answers 400 and the
   * checkout page asks for one and retries.
   */
  advance_method?: string;
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

/**
 * GET /v1/auth/me — the signed-in user's row, re-read from the API
 * (Wave 10). Login stores a snapshot; this refreshes it — above all
 * `is_admin`, which may have changed since the token was issued.
 */
export function getMe(token: string): Promise<User> {
  return call("/v1/auth/me", { token });
}

/** GET /v1/stores/{slug} — public store profile (also resolves the id). */
export function getStore(slug: string): Promise<Store> {
  return call(`/v1/stores/${encodeURIComponent(slug)}`);
}

/**
 * Branding write body (Wave 18). Every field is optional and the
 * API applies the codebase's PATCH convention: a field left out
 * (or null) keeps its stored value; a field sent as a string is
 * validated and stored; an empty string CLEARS a nullable field
 * (brand_color, logo_url, banner_url, announcement, description)
 * back to null. theme_key can be changed but never cleared.
 */
export interface UpdateBrandingInput {
  theme_key?: StoreTheme;
  brand_color?: string;
  logo_url?: string;
  banner_url?: string;
  announcement?: string;
  description?: string;
}

/** PUT /v1/seller/store/branding — update the caller's own store branding. */
export function updateStoreBranding(
  token: string,
  input: UpdateBrandingInput
): Promise<Store> {
  return call("/v1/seller/store/branding", {
    method: "PUT",
    token,
    body: input,
  });
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

/** Sort orders the search endpoint understands (Wave 15). */
export type SearchSort =
  | "relevance"
  | "price_asc"
  | "price_desc"
  | "newest"
  | "rating";

/** One page of search results (Wave 15): the shared Product shape
 * plus the pagination envelope (`per_page`, not `limit`). */
export interface SearchResult {
  items: Product[];
  total: number;
  page: number;
  per_page: number;
}

/**
 * GET /v1/search — Postgres-native product search (Wave 15):
 * text over title / Bangla title / description, composed with the
 * category, price-range and store filters. Public — no token.
 */
export function searchProducts(params?: {
  q?: string;
  store_slug?: string;
  category?: string;
  min_price_poisha?: number;
  max_price_poisha?: number;
  sort?: SearchSort;
  page?: number;
  per_page?: number;
}): Promise<SearchResult> {
  const search = new URLSearchParams();
  if (params?.q) search.set("q", params.q);
  if (params?.store_slug) search.set("store_slug", params.store_slug);
  if (params?.category) search.set("category", params.category);
  if (params?.min_price_poisha !== undefined)
    search.set("min_price_poisha", String(params.min_price_poisha));
  if (params?.max_price_poisha !== undefined)
    search.set("max_price_poisha", String(params.max_price_poisha));
  if (params?.sort) search.set("sort", params.sort);
  if (params?.page) search.set("page", String(params.page));
  if (params?.per_page) search.set("per_page", String(params.per_page));
  const qs = search.toString();
  return call(`/v1/search${qs ? `?${qs}` : ""}`);
}

/** Typeahead suggestions (Wave 15): product titles + stores. */
export interface SearchSuggestions {
  products: { id: string; title: string }[];
  stores: { slug: string; name: string }[];
}

/**
 * GET /v1/search/suggest — suggestions for a text fragment. The
 * API answers with two empty lists below 2 characters.
 */
export function searchSuggest(q: string): Promise<SearchSuggestions> {
  const search = new URLSearchParams({ q });
  return call(`/v1/search/suggest?${search.toString()}`);
}

/** POST /v1/products — create a product (seller, own store only). */
export function createProduct(
  token: string,
  input: CreateProductInput
): Promise<Product> {
  return call("/v1/products", { method: "POST", token, body: input });
}

/** GET /v1/products/{id} — one product, with its rating summary. */
export function getProduct(id: string, token?: string | null): Promise<Product> {
  return call(`/v1/products/${encodeURIComponent(id)}`, { token });
}

/** A seller's reply to a review (Wave 8). */
export interface ReviewReply {
  id: string;
  review_id: string;
  store_id: string;
  body: string;
  created_at: string;
  updated_at: string;
}

/** One product review (Wave 8). The buyer is shown by name when they
 * have one, otherwise by masked phone — never the full number. */
export interface Review {
  id: string;
  product_id: string;
  buyer_id: string;
  buyer_display: string;
  order_id: string;
  rating: number;
  body: string | null;
  created_at: string;
  updated_at: string;
  reply: ReviewReply | null;
}

export interface ReviewList {
  items: Review[];
  total: number;
  page: number;
  limit: number;
}

/** GET /v1/products/{id}/reviews — public review list, newest first. */
export function getProductReviews(
  productId: string,
  params?: { page?: number; limit?: number }
): Promise<ReviewList> {
  const search = new URLSearchParams();
  if (params?.page) search.set("page", String(params.page));
  if (params?.limit) search.set("limit", String(params.limit));
  const qs = search.toString();
  return call(`/v1/products/${encodeURIComponent(productId)}/reviews${qs ? `?${qs}` : ""}`);
}

/**
 * POST /v1/products/{id}/reviews — review a product. The API allows
 * this only after the buyer received the product in a delivered
 * order; otherwise it rejects with 403 and the eligibility message.
 */
export function createReview(
  token: string,
  productId: string,
  rating: number,
  body?: string
): Promise<Review> {
  return call(`/v1/products/${encodeURIComponent(productId)}/reviews`, {
    method: "POST",
    token,
    body: { rating, body },
  });
}

/** PATCH /v1/reviews/{id} — edit your own review (others get 404). */
export function updateReview(
  token: string,
  reviewId: string,
  patch: { rating?: number; body?: string }
): Promise<Review> {
  return call(`/v1/reviews/${encodeURIComponent(reviewId)}`, {
    method: "PATCH",
    token,
    body: patch,
  });
}

/** DELETE /v1/reviews/{id} — delete your own review (others get 404). */
export function deleteReview(token: string, reviewId: string): Promise<void> {
  return call(`/v1/reviews/${encodeURIComponent(reviewId)}`, {
    method: "DELETE",
    token,
  });
}

/**
 * POST /v1/seller/reviews/{id}/reply — the store owner replies to a
 * review of their product. One reply per review (a second is a 409);
 * sellers of other stores get a 404.
 */
export function replyToReview(
  token: string,
  reviewId: string,
  body: string
): Promise<ReviewReply> {
  return call(`/v1/seller/reviews/${encodeURIComponent(reviewId)}/reply`, {
    method: "POST",
    token,
    body: { body },
  });
}

/** PATCH /v1/seller/reviews/{id}/reply — edit your store's reply. */
export function updateReply(
  token: string,
  reviewId: string,
  body: string
): Promise<ReviewReply> {
  return call(`/v1/seller/reviews/${encodeURIComponent(reviewId)}/reply`, {
    method: "PATCH",
    token,
    body: { body },
  });
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

/** POST /v1/orders — checkout. Returns the placed order. */
export function placeOrder(token: string, input: PlaceOrderInput): Promise<Order> {
  return call("/v1/orders", { method: "POST", token, body: input });
}

/** GET /v1/payments/methods — which payment methods are usable now. */
export function getPaymentMethods(): Promise<PaymentMethodInfo[]> {
  return call("/v1/payments/methods");
}

/** POST /v1/payments/{id}/retry — a failed payment goes back to pending. */
export function retryPayment(token: string, paymentId: string): Promise<Payment> {
  return call(`/v1/payments/${encodeURIComponent(paymentId)}/retry`, {
    method: "POST",
    token,
  });
}

/**
 * POST /v1/payments/{id}/dev-simulate-success — DEV ONLY helper: the
 * API signs and applies a success callback for a dev payment through
 * the same path as a real webhook. Exists only when the API runs
 * with PAYMENTS_DEV_MODE on; otherwise the API returns 404.
 */
export function devSimulateSuccess(
  token: string,
  paymentId: string
): Promise<Payment> {
  return call(
    `/v1/payments/${encodeURIComponent(paymentId)}/dev-simulate-success`,
    { method: "POST", token }
  );
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

/** One seller's slice of a receipt — no commission/payout (buyer view). */
export interface ReceiptSubOrder {
  store_name: string;
  store_slug: string;
  status: string;
  goods_subtotal_poisha: number;
  delivery_charge_poisha: number;
  items: OrderItem[];
}

/** GET /v1/orders/{id}/receipt — the buyer receipt view-model. */
export interface Receipt {
  platform_name: string;
  order_id: string;
  created_at: string;
  status: string;
  payment_method: string;
  /**
   * pay_on_delivery | pending | paid | failed | refunded (Wave 5),
   * plus advance_pending | advance_paid (Wave 17).
   */
  payment_status: string;
  /**
   * Cash due on delivery: grand total minus any advance already
   * collected, for COD orders; 0 when nothing is due on delivery.
   */
  cod_amount_due_poisha: number;
  /** Advance this COD order required at checkout (Wave 17). */
  advance_required_poisha: number;
  /** Advance collected online (Wave 17). */
  advance_paid_poisha: number;
  delivery: {
    name: string;
    phone: string;
    address: string;
    zone: string;
  };
  sub_orders: ReceiptSubOrder[];
  goods_total_poisha: number;
  /** Coupon applied at checkout (Wave 14); null when none. */
  coupon_code: string | null;
  /** Total coupon discount (Wave 14); shown as its own line. */
  discount_total_poisha: number;
  delivery_total_poisha: number;
  grand_total_poisha: number;
}

/**
 * POST /v1/orders/{id}/cancel — buyer cancels while every sub-order is
 * still placed; the API restores stock. Rejects with 409 afterwards.
 */
export function cancelOrder(token: string, id: string): Promise<Order> {
  return call(`/v1/orders/${encodeURIComponent(id)}/cancel`, {
    method: "POST",
    token,
  });
}

/** GET /v1/orders/{id}/receipt — receipt view-model for one own order. */
export function getOrderReceipt(token: string, id: string): Promise<Receipt> {
  return call(`/v1/orders/${encodeURIComponent(id)}/receipt`, { token });
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

/** GET /v1/couriers — which couriers can be booked right now. */
export function getCouriers(): Promise<CourierInfo[]> {
  return call("/v1/couriers");
}

/**
 * GET /v1/couriers/estimate — per-courier delivery-charge estimates
 * for a zone and weight, plus the smart-routing recommendation
 * (Wave 7). Prices are local tariff estimates, not live quotes.
 */
export function getCourierEstimates(
  zone: string,
  weightGrams = 1000
): Promise<CourierEstimates> {
  return call(
    `/v1/couriers/estimate?zone=${encodeURIComponent(zone)}&weight_grams=${weightGrams}`
  );
}

/**
 * POST /v1/seller/sub-orders/{id}/shipment — book a courier shipment
 * for a confirmed sub-order (Wave 6). Omitting `courier` books the
 * platform's default courier; `courier: "auto"` (Wave 7) lets smart
 * routing pick the cheapest available courier, and the shipment
 * stores the chosen courier's name. `areaId` is the recipient's
 * numeric area id for couriers that require one (RedX); the others
 * ignore it. One shipment per sub-order: a second booking is a 409,
 * and with no courier configured it is a 503.
 */
export function createShipment(
  token: string,
  subOrderId: string,
  courier?: string,
  areaId?: number
): Promise<Shipment> {
  const body: { courier?: string; area_id?: number } = {};
  if (courier) body.courier = courier;
  if (areaId !== undefined) body.area_id = areaId;
  return call(`/v1/seller/sub-orders/${encodeURIComponent(subOrderId)}/shipment`, {
    method: "POST",
    token,
    body,
  });
}

/**
 * POST /v1/shipments/{id}/refresh — poll the courier for the latest
 * tracking status (Wave 6). The shipment's seller and the order's
 * buyer may call this; anyone else gets a 404.
 */
export function refreshShipment(token: string, shipmentId: string): Promise<Shipment> {
  return call(`/v1/shipments/${encodeURIComponent(shipmentId)}/refresh`, {
    method: "POST",
    token,
  });
}

/** A store's money summary (Wave 9): earnings from DELIVERED
 * sub-orders only, minus what is paid out or waiting in requests. */
export interface Earnings {
  earned_poisha: number;
  paid_out_poisha: number;
  pending_poisha: number;
  available_poisha: number;
}

/** Sub-order counts per fulfilment status (Wave 9 dashboard). */
export interface SubOrderStatusCounts {
  placed: number;
  confirmed: number;
  shipped: number;
  delivered: number;
  cancelled: number;
  returned: number;
}

/** Everything the seller home page renders (Wave 9). */
export interface SellerDashboard {
  store: Store;
  counts: {
    products: number;
    sub_orders_by_status: SubOrderStatusCounts;
  };
  earnings: Earnings;
  recent_sub_orders: SellerSubOrder[];
}

/** Where a store's payout money goes (Wave 9). */
export interface PayoutAccount {
  id: string;
  store_id: string;
  /** bkash | nagad | bank */
  method: string;
  account_name: string;
  account_number: string;
  created_at: string;
  updated_at: string;
}

/** One payout request with its account snapshot (Wave 9). */
export interface Payout {
  id: string;
  store_id: string;
  amount_poisha: number;
  method: string;
  account_name: string;
  account_number: string;
  /** requested | approved | paid | rejected */
  status: string;
  note: string | null;
  created_at: string;
  processed_at: string | null;
}

export interface PayoutList {
  items: Payout[];
  total: number;
  page: number;
  limit: number;
}

/** Bangla label for a payout status (Wave 9). */
export function payoutStatusLabel(status: string): string {
  switch (status) {
    case "requested":
      return "অনুরোধ করা হয়েছে";
    case "approved":
      return "অনুমোদিত";
    case "paid":
      return "পরিশোধিত";
    case "rejected":
      return "প্রত্যাখ্যাত";
    default:
      return status;
  }
}

/** GET /v1/seller/dashboard — store, counts, earnings, recent orders. */
export function getSellerDashboard(token: string): Promise<SellerDashboard> {
  return call("/v1/seller/dashboard", { token });
}

/** GET /v1/seller/balance — the caller's store earnings summary. */
export function getSellerBalance(token: string): Promise<Earnings> {
  return call("/v1/seller/balance", { token });
}

/** GET /v1/seller/payout-account — the saved account, or null. */
export function getPayoutAccount(token: string): Promise<PayoutAccount | null> {
  return call("/v1/seller/payout-account", { token });
}

/** PUT /v1/seller/payout-account — create or replace the account. */
export function savePayoutAccount(
  token: string,
  input: { method: string; account_name: string; account_number: string }
): Promise<PayoutAccount> {
  return call("/v1/seller/payout-account", { method: "PUT", token, body: input });
}

/**
 * POST /v1/seller/payouts — request a payout of `amountPoisha`
 * against the available balance. The API enforces the 500-taka
 * minimum and the balance fit; the account is snapshotted.
 */
export function requestPayout(token: string, amountPoisha: number): Promise<Payout> {
  return call("/v1/seller/payouts", {
    method: "POST",
    token,
    body: { amount_poisha: amountPoisha },
  });
}

/** GET /v1/seller/payouts — the store's payout requests, newest first. */
export function listPayouts(
  token: string,
  params?: { page?: number; limit?: number }
): Promise<PayoutList> {
  const search = new URLSearchParams();
  if (params?.page) search.set("page", String(params.page));
  if (params?.limit) search.set("limit", String(params.limit));
  const qs = search.toString();
  return call(`/v1/seller/payouts${qs ? `?${qs}` : ""}`, { token });
}

// ---------------------------------------------------------------------------
// Wave 10 — platform admin. Every call needs an admin token; the API
// answers 403 for everyone else (the pages also gate on /v1/auth/me).
// ---------------------------------------------------------------------------

/** Platform metrics (GET /v1/admin/overview). GMV and commission
 * exclude cancelled sub-orders — they never moved money. */
export interface AdminOverview {
  users: number;
  stores: number;
  products: number;
  orders: number;
  gmv_goods_poisha: number;
  commission_earned_poisha: number;
  payouts_pending: { count: number; amount_poisha: number };
  payments_pending_count: number;
}

/** A store in the admin list: owner phone + product count added. */
export interface AdminStore {
  id: string;
  owner_user_id: string;
  owner_phone: string;
  slug: string;
  name: string;
  description: string | null;
  /** active | suspended */
  status: string;
  product_count: number;
  created_at: string;
}

export interface AdminStoreList {
  items: AdminStore[];
  total: number;
  page: number;
  limit: number;
}

/** A payout in the admin queue: store name + owner phone added. */
export interface AdminPayout {
  id: string;
  store_id: string;
  store_name: string;
  owner_phone: string;
  amount_poisha: number;
  method: string;
  account_name: string;
  account_number: string;
  /** requested | approved | paid | rejected */
  status: string;
  note: string | null;
  created_at: string;
  processed_at: string | null;
}

export interface AdminPayoutList {
  items: AdminPayout[];
  total: number;
  page: number;
  limit: number;
}

/** A user in the admin list: store count added. */
export interface AdminUser {
  id: string;
  phone: string;
  name: string | null;
  role: string;
  is_admin: boolean;
  store_count: number;
  created_at: string;
}

export interface AdminUserList {
  items: AdminUser[];
  total: number;
  page: number;
  limit: number;
}

/** Effective platform settings (GET /v1/admin/settings): the values
 * checkout and payout creation actually use, after overrides. */
export interface AdminSettings {
  commission_bps: number;
  min_payout_poisha: number;
  /** COD advance rate in basis points (Wave 17). */
  advance_percent_bps: number;
  /** COD advance threshold in poisha; 0 disables it (Wave 17). */
  advance_threshold_poisha: number;
  platform_name: string;
}

/** GET /v1/admin/overview — platform metrics for the stat cards. */
export function getAdminOverview(token: string): Promise<AdminOverview> {
  return call("/v1/admin/overview", { token });
}

/** GET /v1/admin/stores — every store, optional name/slug search. */
export function listAdminStores(
  token: string,
  params?: { q?: string; page?: number; limit?: number }
): Promise<AdminStoreList> {
  const search = new URLSearchParams();
  if (params?.q) search.set("q", params.q);
  if (params?.page) search.set("page", String(params.page));
  if (params?.limit) search.set("limit", String(params.limit));
  const qs = search.toString();
  return call(`/v1/admin/stores${qs ? `?${qs}` : ""}`, { token });
}

/**
 * PATCH /v1/admin/stores/{id}/status — suspend or reactivate a
 * store. Suspension hides its products publicly and blocks its
 * checkouts (409); it deletes nothing.
 */
export function setAdminStoreStatus(
  token: string,
  storeId: string,
  status: "active" | "suspended"
): Promise<Store> {
  return call(`/v1/admin/stores/${encodeURIComponent(storeId)}/status`, {
    method: "PATCH",
    token,
    body: { status },
  });
}

/** GET /v1/admin/orders — every order, optional status filter. */
export function listAdminOrders(
  token: string,
  params?: { status?: string; page?: number; limit?: number }
): Promise<OrderList> {
  const search = new URLSearchParams();
  if (params?.status) search.set("status", params.status);
  if (params?.page) search.set("page", String(params.page));
  if (params?.limit) search.set("limit", String(params.limit));
  const qs = search.toString();
  return call(`/v1/admin/orders${qs ? `?${qs}` : ""}`, { token });
}

/** GET /v1/admin/payouts — the processing queue, optional status filter. */
export function listAdminPayouts(
  token: string,
  params?: { status?: string; page?: number; limit?: number }
): Promise<AdminPayoutList> {
  const search = new URLSearchParams();
  if (params?.status) search.set("status", params.status);
  if (params?.page) search.set("page", String(params.page));
  if (params?.limit) search.set("limit", String(params.limit));
  const qs = search.toString();
  return call(`/v1/admin/payouts${qs ? `?${qs}` : ""}`, { token });
}

/** POST /v1/admin/payouts/{id}/approve — requested -> approved. */
export function approvePayout(token: string, payoutId: string): Promise<Payout> {
  return call(`/v1/admin/payouts/${encodeURIComponent(payoutId)}/approve`, {
    method: "POST",
    token,
  });
}

/** POST /v1/admin/payouts/{id}/reject — requested -> rejected, with a reason. */
export function rejectPayout(
  token: string,
  payoutId: string,
  note?: string
): Promise<Payout> {
  return call(`/v1/admin/payouts/${encodeURIComponent(payoutId)}/reject`, {
    method: "POST",
    token,
    body: { note },
  });
}

/** POST /v1/admin/payouts/{id}/mark-paid — approved -> paid. */
export function markPayoutPaid(token: string, payoutId: string): Promise<Payout> {
  return call(`/v1/admin/payouts/${encodeURIComponent(payoutId)}/mark-paid`, {
    method: "POST",
    token,
  });
}

/** GET /v1/admin/users — every user, optional phone substring search. */
export function listAdminUsers(
  token: string,
  params?: { q?: string; page?: number; limit?: number }
): Promise<AdminUserList> {
  const search = new URLSearchParams();
  if (params?.q) search.set("q", params.q);
  if (params?.page) search.set("page", String(params.page));
  if (params?.limit) search.set("limit", String(params.limit));
  const qs = search.toString();
  return call(`/v1/admin/users${qs ? `?${qs}` : ""}`, { token });
}

/**
 * PATCH /v1/admin/users/{id} — set or clear ANOTHER user's admin
 * flag. The API refuses self-changes with a 400.
 */
export function setUserAdmin(
  token: string,
  userId: string,
  isAdmin: boolean
): Promise<User> {
  return call(`/v1/admin/users/${encodeURIComponent(userId)}`, {
    method: "PATCH",
    token,
    body: { is_admin: isAdmin },
  });
}

/** GET /v1/admin/settings — the effective platform settings. */
export function getAdminSettings(token: string): Promise<AdminSettings> {
  return call("/v1/admin/settings", { token });
}

// ---------------------------------------------------------------------------
// Wave 12 — Facebook Page link + post import
// ---------------------------------------------------------------------------

/**
 * The seller's Facebook status: whether a Page is linked (and
 * which), plus whether the platform's Meta app credentials exist.
 * The two are independent — a page can be linked long before the
 * Graph API can fetch from it.
 */
export interface FacebookStatus {
  linked: boolean;
  page_id: string | null;
  page_name: string | null;
  /** "configured" | "not_configured" */
  graph_api: string;
}

/** One pasted Facebook post, keyed by the id the seller gives it. */
export interface FbPostInput {
  source_ref: string;
  text: string;
}

/** One editable product draft parsed from a pasted post. */
export interface ImportDraft {
  source_ref: string;
  title: string;
  title_bn: string | null;
  description: string | null;
  price_poisha: number | null;
  weight_grams: number;
  stock: number;
  valid: boolean;
  errors: string[];
}

/** One draft as sent to the import endpoint (price always set). */
export interface ImportDraftInput {
  source_ref: string;
  title: string;
  title_bn?: string;
  description?: string;
  price_poisha: number;
  weight_grams?: number;
  stock: number;
}

/** One draft the import skipped, with the reason. */
export interface SkippedImport {
  source_ref: string;
  reason: string;
}

export interface ImportResult {
  created: Product[];
  skipped: SkippedImport[];
}

/** GET /v1/seller/facebook/status — link state + Graph API state. */
export function getFacebookStatus(token: string): Promise<FacebookStatus> {
  return call("/v1/seller/facebook/status", { token });
}

/** PUT /v1/seller/store/facebook — link (or re-link) the seller's Page. */
export function linkFacebookPage(
  token: string,
  pageId: string,
  pageName: string
): Promise<Store> {
  return call("/v1/seller/store/facebook", {
    method: "PUT",
    token,
    body: { page_id: pageId, page_name: pageName },
  });
}

/** DELETE /v1/seller/store/facebook — remove the Page link. */
export function unlinkFacebookPage(token: string): Promise<Store> {
  return call("/v1/seller/store/facebook", { method: "DELETE", token });
}

/**
 * POST /v1/seller/products/import-preview — parse pasted posts
 * into editable drafts. Writes nothing on the server.
 */
export function previewImport(
  token: string,
  posts: FbPostInput[]
): Promise<{ drafts: ImportDraft[] }> {
  return call("/v1/seller/products/import-preview", {
    method: "POST",
    token,
    body: { posts },
  });
}

/**
 * POST /v1/seller/products/import — create products from the
 * reviewed drafts. Invalid or already-imported drafts come back
 * in `skipped` with reasons; the rest are created.
 */
export function runImport(
  token: string,
  drafts: ImportDraftInput[]
): Promise<ImportResult> {
  return call("/v1/seller/products/import", {
    method: "POST",
    token,
    body: { drafts },
  });
}

/**
 * PUT /v1/admin/settings — store overrides. A stored commission
 * rate beats COMMISSION_RATE_BPS at checkout; a stored minimum
 * payout beats the built-in 500-taka floor for new requests.
 */
export function updateAdminSettings(
  token: string,
  patch: {
    commission_bps?: number;
    min_payout_poisha?: number;
    advance_percent_bps?: number;
    advance_threshold_poisha?: number;
  }
): Promise<AdminSettings> {
  return call("/v1/admin/settings", { method: "PUT", token, body: patch });
}

// ---------------------------------------------------------------------------
// Wave 13 — messaging + SIDRA bridge
// ---------------------------------------------------------------------------

/** One message in a conversation thread. */
export interface Message {
  id: string;
  conversation_id: string;
  /** buyer | seller | ai */
  sender_type: string;
  /** The sending user; null for AI messages. */
  sender_id: string | null;
  body: string;
  created_at: string;
}

/**
 * One conversation as the caller sees it: `role` is the caller's
 * side (buyer | seller) and `unread_count` counts the OTHER side's
 * messages the caller has not read yet.
 */
export interface Conversation {
  id: string;
  store: { id: string; name: string; slug: string };
  buyer_id: string;
  /** Buyer name, or the masked phone when no name is set. */
  buyer_display: string;
  product_id: string | null;
  product_title: string | null;
  /** buyer | seller — the caller's role in this conversation. */
  role: string;
  /** open | closed */
  status: string;
  last_message_at: string;
  /** First 80 chars of the newest message; null while empty. */
  last_message_preview: string | null;
  unread_count: number;
  created_at: string;
}

/** A thread read: the conversation view plus its messages. */
export interface ConversationThread {
  conversation: Conversation;
  messages: Message[];
}

/** One SIDRA reply suggestion in the seller's queue. */
export interface SidraSuggestion {
  id: string;
  conversation_id: string;
  in_reply_to_message_id: string;
  body: string;
  /** pending | sent | dismissed */
  status: string;
  created_at: string;
  decided_at: string | null;
  /** Who SIDRA was answering (name or masked phone). */
  buyer_display: string;
}

/**
 * The seller's SIDRA status: whether the platform bridge is
 * configured at all, and this store's auto-reply mode. The two
 * are independent — the toggle exists even while unconfigured.
 */
export interface SidraStatus {
  configured: boolean;
  auto_reply: boolean;
}

/** Bangla label for a message sender (display only). */
export function senderTypeLabel(senderType: string): string {
  switch (senderType) {
    case "buyer":
      return "ক্রেতা";
    case "seller":
      return "বিক্রেতা";
    case "ai":
      return "SIDRA (AI)";
    default:
      return senderType;
  }
}

/**
 * POST /v1/conversations — find the caller's open conversation
 * with a store (optionally about one product), or create it.
 */
export function createConversation(
  token: string,
  storeId: string,
  productId?: string
): Promise<Conversation> {
  return call("/v1/conversations", {
    method: "POST",
    token,
    body: { store_id: storeId, product_id: productId ?? null },
  });
}

/** GET /v1/conversations — every conversation the caller is in. */
export function listConversations(token: string): Promise<Conversation[]> {
  return call("/v1/conversations", { token });
}

/**
 * GET /v1/conversations/{id} — read a thread. Reading stamps the
 * caller's read marker, so the returned unread count is 0.
 */
export function getConversation(
  token: string,
  conversationId: string
): Promise<ConversationThread> {
  return call(`/v1/conversations/${encodeURIComponent(conversationId)}`, {
    token,
  });
}

/** POST /v1/conversations/{id}/messages — post as your own side. */
export function sendMessage(
  token: string,
  conversationId: string,
  body: string
): Promise<Message> {
  return call(
    `/v1/conversations/${encodeURIComponent(conversationId)}/messages`,
    { method: "POST", token, body: { body } }
  );
}

/** POST /v1/conversations/{id}/close — close (history stays readable). */
export function closeConversation(
  token: string,
  conversationId: string
): Promise<Conversation> {
  return call(
    `/v1/conversations/${encodeURIComponent(conversationId)}/close`,
    { method: "POST", token }
  );
}

/** GET /v1/seller/sidra/suggestions — the pending AI reply queue. */
export function listSidraSuggestions(
  token: string
): Promise<SidraSuggestion[]> {
  return call("/v1/seller/sidra/suggestions", { token });
}

/**
 * POST /v1/seller/sidra/suggestions/{id}/send — approve a
 * suggestion: its words enter the thread as an AI message.
 */
export function sendSidraSuggestion(
  token: string,
  suggestionId: string
): Promise<SidraSuggestion> {
  return call(
    `/v1/seller/sidra/suggestions/${encodeURIComponent(suggestionId)}/send`,
    { method: "POST", token }
  );
}

/** POST /v1/seller/sidra/suggestions/{id}/dismiss — reject a suggestion. */
export function dismissSidraSuggestion(
  token: string,
  suggestionId: string
): Promise<SidraSuggestion> {
  return call(
    `/v1/seller/sidra/suggestions/${encodeURIComponent(
      suggestionId
    )}/dismiss`,
    { method: "POST", token }
  );
}

/** GET /v1/seller/sidra/status — bridge configuration + store mode. */
export function getSidraStatus(token: string): Promise<SidraStatus> {
  return call("/v1/seller/sidra/status", { token });
}

/**
 * PUT /v1/seller/store/sidra — set the store's auto-reply mode.
 * Returns the full store view (which carries the flag).
 */
export function setSidraAutoReply(
  token: string,
  autoReply: boolean
): Promise<Store> {
  return call("/v1/seller/store/sidra", {
    method: "PUT",
    token,
    body: { auto_reply: autoReply },
  });
}

// ---------------------------------------------------------------------------
// Coupons (Wave 14)
// ---------------------------------------------------------------------------

/**
 * A coupon. `store_id` null = platform coupon (discounts the whole
 * order's goods; the platform bears it). A store coupon discounts
 * only its store's goods and the seller bears it. Codes are
 * case-insensitive (stored upper-case).
 */
export interface Coupon {
  id: string;
  code: string;
  store_id: string | null;
  /** percent | fixed */
  discount_type: string;
  /** Percent (1–90) for percent coupons; poisha for fixed ones. */
  discount_value: number;
  min_order_poisha: number;
  /** Percent coupons only: the discount ceiling in poisha. */
  max_discount_poisha: number | null;
  starts_at: string | null;
  ends_at: string | null;
  /** Global redemption cap; null = unlimited. */
  usage_limit: number | null;
  used_count: number;
  active: boolean;
  created_at: string;
}

export interface CouponList {
  items: Coupon[];
  total: number;
  page: number;
  limit: number;
}

/** Create payload for seller and admin coupons (scope comes from the endpoint). */
export interface CreateCouponInput {
  code: string;
  discount_type: string;
  discount_value: number;
  min_order_poisha?: number;
  max_discount_poisha?: number | null;
  starts_at?: string | null;
  ends_at?: string | null;
  usage_limit?: number | null;
}

/**
 * The coupon preview answer. `scope`/`store_id` say WHICH goods the
 * coupon applies to: for a store coupon the discount depends on that
 * store's goods in the cart, so the client re-previews with
 * `store_goods_poisha` once it knows the scope.
 */
export interface CouponPreview {
  valid: boolean;
  code: string;
  /** "platform" | "store" | null (coupon not found). */
  scope: string | null;
  store_id: string | null;
  discount_poisha: number;
  reason: string | null;
}

/** Preview input: the cart's whole goods total plus, for store coupons, one store's share. */
export interface CouponPreviewInput {
  code: string;
  goods_total_poisha: number;
  store_id?: string | null;
  store_goods_poisha?: number | null;
}

/**
 * POST /v1/coupons/preview — stateless coupon check for the cart.
 * Checkout re-validates under a row lock and stays the authority.
 */
export function previewCoupon(
  token: string,
  input: CouponPreviewInput
): Promise<CouponPreview> {
  return call("/v1/coupons/preview", { method: "POST", token, body: input });
}

/** GET /v1/seller/coupons — the store's own coupons, newest first. */
export function listSellerCoupons(
  token: string,
  params?: { page?: number; limit?: number }
): Promise<CouponList> {
  const search = new URLSearchParams();
  if (params?.page) search.set("page", String(params.page));
  if (params?.limit) search.set("limit", String(params.limit));
  const qs = search.toString();
  return call(`/v1/seller/coupons${qs ? `?${qs}` : ""}`, { token });
}

/** POST /v1/seller/coupons — create a coupon for the caller's store. */
export function createSellerCoupon(
  token: string,
  input: CreateCouponInput
): Promise<Coupon> {
  return call("/v1/seller/coupons", { method: "POST", token, body: input });
}

/** PATCH /v1/seller/coupons/{id} — activate or deactivate a store coupon. */
export function setSellerCouponActive(
  token: string,
  couponId: string,
  active: boolean
): Promise<Coupon> {
  return call(`/v1/seller/coupons/${encodeURIComponent(couponId)}`, {
    method: "PATCH",
    token,
    body: { active },
  });
}

/** DELETE /v1/seller/coupons/{id} — delete an unused store coupon (409 once redeemed). */
export function deleteSellerCoupon(token: string, couponId: string): Promise<void> {
  return call(`/v1/seller/coupons/${encodeURIComponent(couponId)}`, {
    method: "DELETE",
    token,
  });
}

/** GET /v1/admin/coupons — every coupon on the platform, newest first. */
export function listAdminCoupons(
  token: string,
  params?: { page?: number; limit?: number }
): Promise<CouponList> {
  const search = new URLSearchParams();
  if (params?.page) search.set("page", String(params.page));
  if (params?.limit) search.set("limit", String(params.limit));
  const qs = search.toString();
  return call(`/v1/admin/coupons${qs ? `?${qs}` : ""}`, { token });
}

/** POST /v1/admin/coupons — create a platform coupon (store_id null). */
export function createAdminCoupon(
  token: string,
  input: CreateCouponInput
): Promise<Coupon> {
  return call("/v1/admin/coupons", { method: "POST", token, body: input });
}

/** PATCH /v1/admin/coupons/{id} — activate or deactivate any coupon. */
export function setAdminCouponActive(
  token: string,
  couponId: string,
  active: boolean
): Promise<Coupon> {
  return call(`/v1/admin/coupons/${encodeURIComponent(couponId)}`, {
    method: "PATCH",
    token,
    body: { active },
  });
}

// ---------------------------------------------------------------------------
// Wave 16 — public tracking + returns
// ---------------------------------------------------------------------------

/** One step in a tracked shipment's history, oldest first. */
export interface TrackedShipmentEvent {
  /** Unified shipment status. */
  status: string;
  /** The courier's raw status string, when it provided one. */
  note: string | null;
  at: string;
}

/** The courier shipment on a tracked sub-order, with its events. */
export interface TrackedShipment {
  courier: string;
  consignment_id: string;
  status: string;
  events: TrackedShipmentEvent[];
}

/** One seller's slice of a tracked order. */
export interface TrackedSubOrder {
  store_slug: string;
  status: string;
  items: { title_snapshot: string; quantity: number }[];
  shipment: TrackedShipment | null;
}

/**
 * The public tracking view (GET /v1/track). No login: the order
 * id plus the delivery phone given at checkout are the
 * credential, and every failure (unknown id, wrong phone) is the
 * same 404 — the page shows one "not found" message for all.
 */
export interface TrackedOrder {
  order_id: string;
  placed_at: string;
  /** pay_on_delivery | pending | paid | failed | refunded */
  payment_status: string;
  payment_method: string;
  grand_total_poisha: number;
  sub_orders: TrackedSubOrder[];
}

/** GET /v1/track — public tracking by order id + delivery phone. */
export function trackOrder(
  orderId: string,
  phone: string
): Promise<TrackedOrder> {
  const search = new URLSearchParams({ order_id: orderId, phone });
  return call(`/v1/track?${search.toString()}`);
}

/** One returned line, summarised on the return views. */
export interface ReturnItemSummary {
  title_snapshot: string;
  quantity: number;
  line_total_poisha: number;
}

/**
 * A return request. The buyer's view and the seller/admin view
 * share every field except the masked buyer phone, which only
 * the seller side carries.
 */
export interface ReturnRequest {
  id: string;
  sub_order_id: string;
  order_id: string;
  buyer_id: string;
  store_id: string;
  store_slug: string;
  reason: string;
  /** requested | approved | rejected | received */
  status: string;
  /** Goods minus coupon discount; null until the goods are received. */
  refund_poisha: number | null;
  /** none | pending | done — the refund's own lifecycle. */
  refund_status: string;
  seller_note: string | null;
  created_at: string;
  decided_at: string | null;
  items: ReturnItemSummary[];
}

/** A return request as the seller / admin sees it. */
export interface SellerReturnRequest extends ReturnRequest {
  /** The buyer's phone, masked (e.g. 018****111). */
  buyer_phone_masked: string;
}

export interface ReturnRequestList {
  items: ReturnRequest[];
  total: number;
  page: number;
  limit: number;
}

export interface SellerReturnRequestList {
  items: SellerReturnRequest[];
  total: number;
  page: number;
  limit: number;
}

/** Bangla label for a return request status (display only). */
export function returnStatusLabel(status: string): string {
  switch (status) {
    case "requested":
      return "আবেদন করা হয়েছে";
    case "approved":
      return "অনুমোদিত — ফেরত পাঠান";
    case "rejected":
      return "প্রত্যাখ্যাত";
    case "received":
      return "ফেরত পৌঁছেছে";
    default:
      return status;
  }
}

/** Bangla label for a refund status (display only). */
export function refundStatusLabel(status: string): string {
  switch (status) {
    case "pending":
      return "রিফান্ড বাকি আছে";
    case "done":
      return "রিফান্ড দেওয়া হয়েছে";
    default:
      return "রিফান্ড প্রযোজ্য নয়";
  }
}

/**
 * POST /v1/buyer/sub-orders/{id}/return-request — ask to return
 * a DELIVERED sub-order. A not-delivered sub-order is a 409, a
 * second request is a 409, someone else's sub-order is a 404.
 */
export function createReturnRequest(
  token: string,
  subOrderId: string,
  reason: string
): Promise<ReturnRequest> {
  return call(
    `/v1/buyer/sub-orders/${encodeURIComponent(subOrderId)}/return-request`,
    { method: "POST", token, body: { reason } }
  );
}

/** GET /v1/buyer/return-requests — the buyer's own requests. */
export function listBuyerReturnRequests(
  token: string,
  params?: { page?: number; limit?: number }
): Promise<ReturnRequestList> {
  const search = new URLSearchParams();
  if (params?.page) search.set("page", String(params.page));
  if (params?.limit) search.set("limit", String(params.limit));
  const qs = search.toString();
  return call(`/v1/buyer/return-requests${qs ? `?${qs}` : ""}`, { token });
}

/** GET /v1/seller/return-requests — requests for the seller's stores. */
export function listSellerReturnRequests(
  token: string,
  params?: { page?: number; limit?: number }
): Promise<SellerReturnRequestList> {
  const search = new URLSearchParams();
  if (params?.page) search.set("page", String(params.page));
  if (params?.limit) search.set("limit", String(params.limit));
  const qs = search.toString();
  return call(`/v1/seller/return-requests${qs ? `?${qs}` : ""}`, { token });
}

/** POST /v1/seller/return-requests/{id}/approve — requested -> approved. */
export function approveReturnRequest(
  token: string,
  returnId: string,
  note?: string
): Promise<SellerReturnRequest> {
  return call(
    `/v1/seller/return-requests/${encodeURIComponent(returnId)}/approve`,
    { method: "POST", token, body: { note } }
  );
}

/** POST /v1/seller/return-requests/{id}/reject — requested -> rejected. */
export function rejectReturnRequest(
  token: string,
  returnId: string,
  note?: string
): Promise<SellerReturnRequest> {
  return call(
    `/v1/seller/return-requests/${encodeURIComponent(returnId)}/reject`,
    { method: "POST", token, body: { note } }
  );
}

/**
 * POST /v1/seller/return-requests/{id}/received — the goods came
 * back: the API snapshots the refund, opens it as pending, marks
 * the sub-order returned and restocks the items, atomically.
 */
export function markReturnReceived(
  token: string,
  returnId: string
): Promise<SellerReturnRequest> {
  return call(
    `/v1/seller/return-requests/${encodeURIComponent(returnId)}/received`,
    { method: "POST", token }
  );
}

/** GET /v1/admin/return-requests — every request; the refund queue filters refund_status=pending. */
export function listAdminReturnRequests(
  token: string,
  params?: { refund_status?: string; page?: number; limit?: number }
): Promise<SellerReturnRequestList> {
  const search = new URLSearchParams();
  if (params?.refund_status) search.set("refund_status", params.refund_status);
  if (params?.page) search.set("page", String(params.page));
  if (params?.limit) search.set("limit", String(params.limit));
  const qs = search.toString();
  return call(`/v1/admin/return-requests${qs ? `?${qs}` : ""}`, { token });
}

/**
 * POST /v1/admin/return-requests/{id}/refund-done — record that
 * the platform refunded the buyer off-platform (bKash / cash).
 * Only a pending refund can be marked done; a second call is 409.
 */
export function markRefundDone(
  token: string,
  returnId: string
): Promise<SellerReturnRequest> {
  return call(
    `/v1/admin/return-requests/${encodeURIComponent(returnId)}/refund-done`,
    { method: "POST", token }
  );
}

// ---------------------------------------------------------------------------
// Wave 17 — COD advance policy + buyer risk (fraud shield)
// ---------------------------------------------------------------------------

/**
 * The public COD advance policy (GET /v1/payments/advance-policy):
 * the effective rate and threshold checkout applies. A threshold
 * of 0 disables threshold-based advances. A buyer's personal
 * risk-forced advance is never visible here — only the server
 * knows a buyer's history.
 */
export interface AdvancePolicy {
  percent_bps: number;
  threshold_poisha: number;
}

/** GET /v1/payments/advance-policy — public, no token. */
export function getAdvancePolicy(): Promise<AdvancePolicy> {
  return call("/v1/payments/advance-policy");
}

/**
 * One phone number's fraud-shield report (GET /v1/seller/buyer-risk).
 * `found` false means the phone has no account; the counts are
 * then all zero and the level low.
 */
export interface BuyerRisk {
  found: boolean;
  total_orders: number;
  delivered: number;
  returned_rto: number;
  buyer_cancelled: number;
  /** low | medium | high */
  risk_level: string;
  requires_advance: boolean;
}

/** GET /v1/seller/buyer-risk?phone= — sellers (store owners) only. */
export function getBuyerRisk(token: string, phone: string): Promise<BuyerRisk> {
  const search = new URLSearchParams({ phone });
  return call(`/v1/seller/buyer-risk?${search.toString()}`, { token });
}

/** Bangla label for a buyer risk level (display only). */
export function riskLevelLabel(level: string): string {
  switch (level) {
    case "high":
      return "উচ্চ ঝুঁকি";
    case "medium":
      return "মাঝারি ঝুঁকি";
    default:
      return "নিম্ন ঝুঁকি";
  }
}
