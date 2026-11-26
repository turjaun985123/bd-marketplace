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
  /** Weight of one unit in grams (Wave 4 — drives delivery charges). */
  weight_grams: number;
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
  delivery_charge_poisha: number;
  commission_poisha: number;
  seller_payout_poisha: number;
  created_at: string;
  items: OrderItem[];
  /** Courier shipment (Wave 6); null until the seller books one. */
  shipment: ShipmentSummary | null;
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
    default:
      return "ডেলিভারিতে পেমেন্ট";
  }
}

export interface Order {
  id: string;
  buyer_user_id: string;
  status: string;
  payment_method: string;
  /** pay_on_delivery | pending | paid | failed | refunded (Wave 5). */
  payment_status: string;
  /** Latest payment attempt; null for COD orders. */
  payment: PaymentSummary | null;
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
  /** Courier shipment (Wave 6); null until the seller books one. */
  shipment: ShipmentSummary | null;
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
  /** pay_on_delivery | pending | paid | failed | refunded (Wave 5). */
  payment_status: string;
  /** Grand total for COD orders; 0 when nothing is due on delivery. */
  cod_amount_due_poisha: number;
  delivery: {
    name: string;
    phone: string;
    address: string;
    zone: string;
  };
  sub_orders: ReceiptSubOrder[];
  goods_total_poisha: number;
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
