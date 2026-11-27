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
  /**
   * KYC verification tier (Wave 21): unverified | basic |
   * verified — granted by platform document review, never
   * seller-chosen. Drives the storefront trust badge.
   */
  verified_tier: VerificationTier;
  created_at: string;
}

/** The three storefront layout variants (Wave 18). */
export type StoreTheme = "classic" | "minimal" | "bold";

/** Seller KYC tiers (Wave 21), lowest trust first. */
export type VerificationTier = "unverified" | "basic" | "verified";

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
  /**
   * Effective stock (Wave 33): the sum of the active
   * variants' stocks while the product has active variants,
   * otherwise the product's own stock.
   */
  stock: number;
  /** Whether the product has at least one active variant (Wave 33). */
  has_variants: boolean;
  /** Per-product low-stock warning level (Wave 23); null = platform default. */
  low_stock_threshold: number | null;
  /** in_stock | low | out_of_stock — computed by the API (Wave 23). */
  stock_status: string;
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
  /**
   * Inside a live sponsored-promotion window right now
   * (Wave 26). Sponsored products sort first in the default
   * listings and always render the স্পন্সরড badge — placement
   * is paid and labelled, never disguised.
   */
  is_promoted: boolean;
  /**
   * Primary image's gallery-size URL (Wave 51): the stored
   * derived WebP for uploads, the hotlink itself for external
   * images, null when the product has no image at all.
   */
  image_url: string | null;
  /** Second image's gallery URL (Wave 51 ext): cards swap to it on hover. */
  hover_image_url: string | null;
  /**
   * Units sold (Wave 52): order-item quantities over
   * DELIVERED sub-orders only — the API's one definition of
   * a completed sale. 0 for a product nobody has received.
   */
  sold_count: number;
  /**
   * The live flash sale price (Wave 52) when one resolves
   * for this product right now (sale live, product in it,
   * cap not exhausted) — the price the cart and checkout
   * will charge. Null when the product sells at base price.
   */
  flash_price_poisha: number | null;
  /** When the live flash sale ends (Wave 52); set with the price. */
  flash_ends_at: string | null;
  created_at: string;
  updated_at: string;
}

export interface ProductList {
  items: Product[];
  total: number;
  page: number;
  limit: number;
}

/**
 * One purchasable variant of a product (Wave 33), as the
 * product detail shows it: active variants only, priced at
 * the FINAL price (base + delta), with its own stock.
 */
export interface ProductVariant {
  id: string;
  name: string;
  sku: string | null;
  /** Difference against the base price, in poisha (may be negative). */
  price_delta_poisha: number;
  /** Base price + delta — what one unit of this variant costs. */
  final_price_poisha: number;
  stock: number;
  /** Gallery URL of the image assigned to this variant (Wave 51). */
  image_url: string | null;
}

/**
 * One image in a product's public gallery (Wave 51). Stored
 * uploads carry the derived WebP sizes; external (hotlinked)
 * images repeat the external URL at every size.
 */
export interface GalleryImageView {
  id: string;
  url_card: string;
  url_gallery: string;
  url_zoom: string;
  alt_text: string | null;
  variant_id: string | null;
  is_primary: boolean;
}

/** The product detail shape (Wave 33): the product plus its variants. */
export interface ProductDetail extends Product {
  variants: ProductVariant[];
  /** The full ordered image gallery (Wave 51); empty when imageless. */
  images: GalleryImageView[];
}

/**
 * One product image in the SELLER's management view (Wave 51):
 * the gallery fields plus the source facts the uploader needs
 * (upload vs external, dimensions, stored bytes).
 */
export interface ProductImageView {
  id: string;
  product_id: string;
  variant_id: string | null;
  source: "upload" | "external";
  url_card: string;
  url_gallery: string;
  url_zoom: string;
  url_original: string;
  width: number | null;
  height: number | null;
  size_bytes: number | null;
  position: number;
  is_primary: boolean;
  alt_text: string | null;
  /** The uploaded file's original name, when known (Wave 51 ext). */
  original_filename: string | null;
  /** Quality warning codes (Wave 51 ext): low_resolution | extreme_aspect | very_large. */
  warnings: string[];
  created_at: string;
}

/** One row of the seller media library (Wave 51). */
export interface SellerMediaItem extends ProductImageView {
  product_title: string;
  store_id: string;
}

/** GET /v1/seller/media — the media library page (Wave 51). */
export interface SellerMediaPage {
  items: SellerMediaItem[];
  total: number;
  /** Uploaded bytes across the seller's stores (Wave 51 ext). */
  storage_bytes: number;
  /** The per-store media quota in bytes (Wave 51 ext). */
  storage_quota_bytes: number;
}

/** One image in the ADMIN media list (Wave 51 extension). */
export interface AdminMediaItem extends ProductImageView {
  product_title: string;
  store_id: string;
  store_slug: string;
}

/** One failed file of a bulk upload (Wave 51 extension). */
export interface BulkUploadFailure {
  filename: string;
  error: string;
}

/** The bulk upload's answer (Wave 51 extension). */
export interface BulkUploadResult {
  created: ProductImageView[];
  failed: BulkUploadFailure[];
}

/** One item of the batch image update (Wave 51). */
export interface ProductImageUpdate {
  id: string;
  position?: number;
  is_primary?: boolean;
  alt_text?: string | null;
  variant_id?: string | null;
}

/**
 * One variant in the seller's management view (Wave 33):
 * inactive variants included — managing them is the point.
 */
export interface SellerVariant {
  id: string;
  product_id: string;
  name: string;
  sku: string | null;
  price_delta_poisha: number;
  final_price_poisha: number;
  stock: number;
  /** Gallery URL of the image assigned to this variant (Wave 51). */
  image_url: string | null;
  is_active: boolean;
  sort: number;
  created_at: string;
  updated_at: string;
}

/** The variant adjust-stock answer: the variant + its audit row. */
export interface VariantAdjustStockResult {
  variant: SellerVariant;
  adjustment: StockAdjustment;
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
  /** The variant this line buys (Wave 33); null for variant-less products. */
  variant_id: string | null;
  /** The variant's name (Wave 33); null for variant-less lines. */
  variant_name: string | null;
  store_id: string;
  store_slug: string;
  title: string;
  title_bn: string | null;
  /** The product's primary image URL (Wave 51); null when imageless. */
  image_url: string | null;
  /** The variant's FINAL price for variant lines (Wave 33). */
  unit_price_poisha: number;
  /**
   * The unit price WITHOUT a live flash sale (Wave 52):
   * base + variant delta. Equals `unit_price_poisha` when
   * no sale governs the line; the difference is the saving.
   */
  list_unit_price_poisha: number;
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
  /** The variant bought (Wave 33); null for variant-less lines. */
  variant_id: string | null;
  /** The variant's name at checkout (Wave 33 snapshot). */
  variant_name: string | null;
  title_snapshot: string;
  unit_price_poisha: number;
  quantity: number;
  line_total_poisha: number;
  /** The product's current primary image URL (Wave 51 ext). */
  image_url: string | null;
}

/** The courier shipment on a sub-order (Wave 6). */
export interface ShipmentSummary {
  id: string;
  courier: string;
  consignment_id: string;
  /** Unified shipment status (created, picked_up, in_transit, …). */
  status: string;
  /**
   * Wave 19: for a manual shipment (`courier === "manual"`), the
   * courier's display name and the counter receipt number the
   * seller recorded (null until they do). Both null for API
   * couriers.
   */
  manual_courier_name: string | null;
  manual_tracking_id: string | null;
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
  /** Wave 19: manual courier display name / receipt number. */
  manual_courier_name: string | null;
  manual_tracking_id: string | null;
  created_at: string;
  updated_at: string;
}

/** Availability of one courier (GET /v1/couriers, Wave 6). */
export interface CourierInfo {
  courier: string;
  available: boolean;
  /**
   * "live" | "dev" | "manual" | null — "dev" means the DEV
   * simulator; "manual" (Wave 19) marks a manual courier, whose
   * `courier` is the manual KEY (booking posts courier "manual"
   * plus this key as `manual_courier`).
   */
  mode: string | null;
  /** Machine-readable reason when unavailable, e.g. "not_configured". */
  reason: string | null;
  is_default: boolean;
  /** Human-facing name for manual couriers; null for API couriers. */
  display_name: string | null;
}

/**
 * One courier's delivery-charge estimate (GET /v1/couriers/estimate,
 * Wave 7). The price is the platform's local tariff estimate, never
 * a live courier quote, and it is null for an unavailable courier.
 */
export interface CourierEstimate {
  courier: string;
  available: boolean;
  /** "live" | "dev" | "manual" | null — "dev" means the DEV simulator. */
  mode: string | null;
  /** Machine-readable reason when unavailable, e.g. "not_configured". */
  reason: string | null;
  price_poisha: number | null;
  /**
   * Wave 19: explanation when there is no price to show — the
   * manual couriers carry one (booked at the counter, priced
   * there). Null for API couriers.
   */
  note: string | null;
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
  /** Loyalty discount allocated to this sub-order (Wave 35). */
  loyalty_discount_poisha: number;
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
  /** Loyalty points redeemed on this order (Wave 35); 0 when none. */
  loyalty_points_used: number;
  /**
   * The discount those points bought (Wave 35). The grand total
   * is already net of it; the platform funds it, so seller
   * payouts never see it.
   */
  loyalty_discount_poisha: number;
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
  /**
   * Loyalty points to redeem (Wave 35). The server clamps the
   * request to the redeemable maximum (balance, capped at half
   * the post-coupon goods) — the placed order reports the
   * points actually used.
   */
  loyalty_points?: number;
}

/** One loyalty ledger movement (GET /v1/loyalty history, Wave 35). */
export interface LoyaltyEntry {
  /** Signed: positive for earn/refund, negative for redeem/clawback. */
  delta: number;
  /** earn | redeem | adjust */
  reason: string;
  ref_id: string | null;
  /** When an earned point stops being spendable; null on non-earn rows. */
  expires_at: string | null;
  created_at: string;
}

/** GET /v1/loyalty — the buyer's points balance, rules and history. */
export interface LoyaltyView {
  available_points: number;
  lifetime_earned: number;
  lifetime_redeemed: number;
  /** Net goods poisha that earn one point (100 taka = 10_000). */
  earn_poisha_per_point: number;
  /** Poisha one redeemed point buys (1 taka = 100). */
  point_value_poisha: number;
  /** Redemption cap, as a percent of post-coupon goods (50). */
  max_redeem_percent_of_goods: number;
  /** Days an earned point stays spendable (365). */
  expiry_days: number;
  history: LoyaltyEntry[];
}

/** GET /v1/loyalty — my loyalty points (Wave 35). */
export function getLoyalty(token: string): Promise<LoyaltyView> {
  return call("/v1/loyalty", { token });
}

function apiBase(): string {
  return process.env.NEXT_PUBLIC_API_BASE_URL ?? "http://localhost:8080";
}

async function call<T>(
  path: string,
  options: {
    method?: string;
    token?: string | null;
    body?: unknown;
    headers?: Record<string, string>;
  } = {}
): Promise<T> {
  const headers: Record<string, string> = {};
  if (options.body !== undefined) headers["Content-Type"] = "application/json";
  if (options.token) headers["Authorization"] = `Bearer ${options.token}`;
  if (options.headers) Object.assign(headers, options.headers);
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

/**
 * The multipart sibling of `call` (Wave 51): sends a FormData
 * body (the browser sets the boundary Content-Type itself) and
 * applies the same error convention. Used by every upload.
 */
async function callForm<T>(
  path: string,
  form: FormData,
  options: { method?: string; token?: string | null } = {}
): Promise<T> {
  const headers: Record<string, string> = {};
  if (options.token) headers["Authorization"] = `Bearer ${options.token}`;
  const res = await fetch(`${apiBase()}${path}`, {
    method: options.method ?? "POST",
    headers,
    body: form,
  });
  const data = (await res.json().catch(() => ({}))) as { error?: string } & T;
  if (!res.ok) {
    throw new Error(data.error ?? `API returned ${res.status}`);
  }
  return data;
}

// ---- Product media (Wave 51) ----

/** GET /v1/seller/products/{id}/images — the owner's full image list. */
export function listProductImages(
  token: string,
  productId: string
): Promise<ProductImageView[]> {
  return call(`/v1/seller/products/${encodeURIComponent(productId)}/images`, {
    token,
  });
}

/**
 * POST /v1/seller/products/{id}/images — upload one image file
 * (multipart). The API sniffs, decodes, re-encodes and derives
 * the WebP sizes; the first image of a product becomes primary.
 */
export function uploadProductImage(
  token: string,
  productId: string,
  file: File,
  options?: { altText?: string; variantId?: string }
): Promise<ProductImageView> {
  const form = new FormData();
  form.append("file", file);
  if (options?.altText) form.append("alt_text", options.altText);
  if (options?.variantId) form.append("variant_id", options.variantId);
  return callForm(`/v1/seller/products/${encodeURIComponent(productId)}/images`, form, {
    token,
  });
}

/**
 * The progress-reporting sibling of `uploadProductImage`
 * (Wave 51): fetch cannot report upload progress, so this one
 * call rides XMLHttpRequest and invokes `onProgress` with the
 * sent fraction (0..1) as the file goes up. Same endpoint,
 * same error convention.
 */
export function uploadProductImageWithProgress(
  token: string,
  productId: string,
  file: File,
  onProgress: (fraction: number) => void,
  options?: { altText?: string; variantId?: string }
): Promise<ProductImageView> {
  const form = new FormData();
  form.append("file", file);
  if (options?.altText) form.append("alt_text", options.altText);
  if (options?.variantId) form.append("variant_id", options.variantId);
  return new Promise((resolve, reject) => {
    const xhr = new XMLHttpRequest();
    xhr.open(
      "POST",
      `${apiBase()}/v1/seller/products/${encodeURIComponent(productId)}/images`
    );
    xhr.setRequestHeader("Authorization", `Bearer ${token}`);
    xhr.upload.onprogress = (event) => {
      if (event.lengthComputable && event.total > 0) {
        onProgress(event.loaded / event.total);
      }
    };
    xhr.onload = () => {
      let data: { error?: string } & ProductImageView = {} as ProductImageView;
      try {
        data = JSON.parse(xhr.responseText) as { error?: string } & ProductImageView;
      } catch {
        // fall through to the status-based error below
      }
      if (xhr.status >= 200 && xhr.status < 300) {
        resolve(data);
      } else {
        reject(new Error(data.error ?? `API returned ${xhr.status}`));
      }
    };
    xhr.onerror = () => reject(new Error("আপলোড ব্যর্থ — Upload failed"));
    xhr.send(form);
  });
}

/** POST /v1/seller/products/{id}/images/from-url — attach a hotlinked image. */
export function attachProductImageUrl(
  token: string,
  productId: string,
  url: string,
  options?: { altText?: string; variantId?: string }
): Promise<ProductImageView> {
  return call(`/v1/seller/products/${encodeURIComponent(productId)}/images/from-url`, {
    method: "POST",
    token,
    body: {
      url,
      alt_text: options?.altText,
      variant_id: options?.variantId,
    },
  });
}

/**
 * PATCH /v1/seller/products/{id}/images — batch update
 * (order, primary, alt text, variant assignment) in one call.
 * Returns the full re-read list.
 */
export function updateProductImages(
  token: string,
  productId: string,
  items: ProductImageUpdate[]
): Promise<ProductImageView[]> {
  return call(`/v1/seller/products/${encodeURIComponent(productId)}/images`, {
    method: "PATCH",
    token,
    body: { items },
  });
}

/**
 * DELETE /v1/seller/products/{id}/images/{imageId} — removes
 * the image and its stored objects; returns the remaining list.
 */
export function deleteProductImage(
  token: string,
  productId: string,
  imageId: string
): Promise<ProductImageView[]> {
  return call(
    `/v1/seller/products/${encodeURIComponent(productId)}/images/${encodeURIComponent(imageId)}`,
    { method: "DELETE", token }
  );
}

/** GET /v1/seller/media — every image across the seller's stores. */
export function listSellerMedia(
  token: string,
  productId?: string,
  source?: "upload" | "external",
  sort?: "newest" | "oldest" | "largest"
): Promise<SellerMediaPage> {
  const params = new URLSearchParams();
  if (productId) params.set("product_id", productId);
  if (source) params.set("source", source);
  if (sort) params.set("sort", sort);
  const qs = params.toString();
  return call(`/v1/seller/media${qs ? `?${qs}` : ""}`, { token });
}

/** The bulk URL attach outcome (Wave 51 extension). */
export interface BulkAttachOutcome {
  created: ProductImageView[];
  failed: { filename: string; error: string }[];
}

/** POST …/images/from-urls — attach many external images at once. */
export function attachProductImageUrls(
  token: string,
  productId: string,
  urls: string[]
): Promise<BulkAttachOutcome> {
  return call(`/v1/seller/products/${productId}/images/from-urls`, {
    token,
    method: "POST",
    body: { urls },
  });
}

/** POST /v1/seller/media/bulk-delete — delete many images at once. */
export function bulkDeleteSellerMedia(
  token: string,
  imageIds: string[]
): Promise<{ deleted: number; skipped: number }> {
  return call("/v1/seller/media/bulk-delete", {
    method: "POST",
    token,
    body: { image_ids: imageIds },
  });
}

/**
 * POST /v1/seller/store/branding-image?kind=logo|banner —
 * upload a store logo or banner through the media pipeline
 * (replaces the hotlink-only branding fields with real files).
 */
export function uploadBrandingImage(
  token: string,
  kind: "logo" | "banner",
  file: File
): Promise<Store> {
  const form = new FormData();
  form.append("file", file);
  return callForm(`/v1/seller/store/branding-image?kind=${kind}`, form, { token });
}

/** POST /v1/reviews/{id}/image — attach (or replace) my review's photo. */
export function attachReviewImage(
  token: string,
  reviewId: string,
  file: File
): Promise<Review> {
  const form = new FormData();
  form.append("file", file);
  return callForm(`/v1/reviews/${encodeURIComponent(reviewId)}/image`, form, {
    token,
  });
}

/** DELETE /v1/reviews/{id}/image — remove my review's photo. */
export function deleteReviewImage(token: string, reviewId: string): Promise<Review> {
  return call(`/v1/reviews/${encodeURIComponent(reviewId)}/image`, {
    method: "DELETE",
    token,
  });
}

/**
 * PUT /v1/seller/products/{id}/images/{imageId} — replace an
 * uploaded image's photo in place (Wave 51 extension): the
 * row keeps its position, primary flag, alt text and
 * variant; only the bytes change.
 */
export function replaceProductImage(
  token: string,
  productId: string,
  imageId: string,
  file: File
): Promise<ProductImageView> {
  const form = new FormData();
  form.append("file", file);
  return callForm(
    `/v1/seller/products/${encodeURIComponent(productId)}/images/${encodeURIComponent(imageId)}`,
    form,
    { method: "PUT", token }
  );
}

/**
 * POST /v1/seller/products/{id}/images/bulk — upload many
 * files in one request (Wave 51 extension); each file lands
 * or fails on its own and the answer names every failure.
 */
export function bulkUploadProductImages(
  token: string,
  productId: string,
  files: File[]
): Promise<BulkUploadResult> {
  const form = new FormData();
  for (const file of files) form.append("file", file);
  return callForm(`/v1/seller/products/${encodeURIComponent(productId)}/images/bulk`, form, {
    token,
  });
}

/**
 * POST /v1/seller/products/{id}/images/from-web — the
 * server FETCHES the URL (SSRF-guarded) and stores the
 * image (Wave 51 extension): the durable form of the
 * hotlink attach.
 */
export function attachProductImageFromWeb(
  token: string,
  productId: string,
  url: string,
  options?: { altText?: string; variantId?: string }
): Promise<ProductImageView> {
  return call(`/v1/seller/products/${encodeURIComponent(productId)}/images/from-web`, {
    method: "POST",
    token,
    body: {
      url,
      alt_text: options?.altText,
      variant_id: options?.variantId,
    },
  });
}

/** GET /v1/admin/media — the newest images across all stores (admin). */
export function listAdminMedia(
  token: string,
  storeSlug?: string
): Promise<AdminMediaItem[]> {
  const qs = storeSlug ? `?store=${encodeURIComponent(storeSlug)}` : "";
  return call(`/v1/admin/media${qs}`, { token });
}

/** DELETE /v1/admin/media/{imageId} — audited moderation delete (admin). */
export function deleteAdminMedia(token: string, imageId: string): Promise<void> {
  return call(`/v1/admin/media/${encodeURIComponent(imageId)}`, {
    method: "DELETE",
    token,
  });
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

/**
 * GET /v1/products — central marketplace listing (active
 * products). Wave 53 adds `sort` (the search sort keys;
 * price sorts order by the EFFECTIVE price, flash
 * included, exactly like the browse).
 */
export function listProducts(params?: {
  q?: string;
  store_slug?: string;
  sort?: SearchSort;
  page?: number;
  limit?: number;
  token?: string | null;
}): Promise<ProductList> {
  const search = new URLSearchParams();
  if (params?.q) search.set("q", params.q);
  if (params?.store_slug) search.set("store_slug", params.store_slug);
  if (params?.sort) search.set("sort", params.sort);
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

/**
 * Sort orders the search endpoint understands (Wave 15;
 * Wave 53 adds `sold` — best sellers first, by the API's
 * delivered-units definition).
 */
export type SearchSort =
  | "relevance"
  | "price_asc"
  | "price_desc"
  | "newest"
  | "rating"
  | "sold";

/** One category row of a browse response's facet block. */
export interface SearchFacetCategory {
  category: string;
  count: number;
}

/** One store row of a browse response's facet block. */
export interface SearchFacetStore {
  slug: string;
  name: string;
  count: number;
}

/** One rating threshold row ("4★ & up") of the facet block. */
export interface SearchFacetRating {
  min_rating: number;
  count: number;
}

/** One band of the price facet's histogram. */
export interface SearchPriceBucket {
  min_poisha: number;
  max_poisha: number;
  count: number;
}

/**
 * The facet block of a browse response (Wave 53). The
 * convention: every group counts the result set filtered
 * by all the OTHER groups — the category counts do not
 * narrow when a category is picked (they are how the
 * shopper widens), while the store counts do. `price` is
 * the effective-price span of the set filtered by
 * everything except the price bounds, split into
 * histogram bands over the same set; bounds are null
 * when that set is empty.
 */
export interface SearchFacets {
  categories: SearchFacetCategory[];
  stores: SearchFacetStore[];
  price: {
    min_poisha: number | null;
    max_poisha: number | null;
    histogram: SearchPriceBucket[];
  };
  ratings: SearchFacetRating[];
}

/**
 * One page of search results: the shared Product shape
 * plus the pagination envelope and (Wave 53) the facet
 * block. `limit` and `per_page` are the same page size
 * under both spellings.
 */
export interface SearchResult {
  items: Product[];
  total: number;
  page: number;
  per_page: number;
  limit: number;
  facets: SearchFacets;
}

/**
 * GET /v1/search — the faceted browse (Wave 15's trigram
 * search, Wave 53's facets): text over title / Bangla
 * title / description, composed with repeatable category
 * and store filters (OR within a group), effective-price
 * bounds, a rating floor, stock and on-sale flags. Every
 * price the response carries is the EFFECTIVE price (the
 * live flash price when one resolves). Public — no token.
 */
export function searchProducts(params?: {
  q?: string;
  store_slug?: string;
  category?: string;
  categories?: string[];
  stores?: string[];
  min_price_poisha?: number;
  max_price_poisha?: number;
  min_rating?: number;
  in_stock?: boolean;
  on_sale?: boolean;
  sort?: SearchSort;
  page?: number;
  per_page?: number;
  limit?: number;
}): Promise<SearchResult> {
  const search = new URLSearchParams();
  if (params?.q) search.set("q", params.q);
  if (params?.store_slug) search.set("store_slug", params.store_slug);
  if (params?.category) search.append("category", params.category);
  for (const category of params?.categories ?? []) {
    search.append("category", category);
  }
  for (const store of params?.stores ?? []) {
    search.append("store", store);
  }
  if (params?.min_price_poisha !== undefined)
    search.set("min_price_poisha", String(params.min_price_poisha));
  if (params?.max_price_poisha !== undefined)
    search.set("max_price_poisha", String(params.max_price_poisha));
  if (params?.min_rating !== undefined)
    search.set("min_rating", String(params.min_rating));
  if (params?.in_stock) search.set("in_stock", "true");
  if (params?.on_sale) search.set("on_sale", "true");
  if (params?.sort) search.set("sort", params.sort);
  if (params?.page) search.set("page", String(params.page));
  if (params?.per_page) search.set("per_page", String(params.per_page));
  if (params?.limit) search.set("limit", String(params.limit));
  const qs = search.toString();
  return call(`/v1/search${qs ? `?${qs}` : ""}`);
}

/**
 * Typeahead suggestions (Wave 15; v2 in Wave 53): product
 * suggestions carry the EFFECTIVE price, the category and
 * the primary image alongside the title, so the dropdown
 * can render a real mini-card. Stores are unchanged.
 */
export interface SearchSuggestions {
  products: {
    id: string;
    title: string;
    price_poisha: number;
    image_url: string | null;
    category: string | null;
  }[];
  categories: { category: string; product_count: number }[];
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

/**
 * GET /v1/products/{id} — one product, with its rating
 * summary and (Wave 33) its active variants.
 */
export function getProduct(
  id: string,
  token?: string | null
): Promise<ProductDetail> {
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
  /** The reviewer's photo, gallery size (Wave 51); null when none. */
  image_url: string | null;
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

/** One product question (Wave 25). The asker is shown only by
 * masked phone — never the full number. */
export interface ProductQuestion {
  id: string;
  product_id: string;
  body: string;
  asker_display: string;
  answer_body: string | null;
  answered_at: string | null;
  created_at: string;
  is_answered: boolean;
}

export interface QuestionList {
  items: ProductQuestion[];
  total: number;
  page: number;
  limit: number;
}

/** A question as the seller inbox sees it: the public view plus
 * its product and the hidden (moderation) flag. */
export interface SellerQuestion extends ProductQuestion {
  product: { id: string; title: string; store_slug: string };
  is_hidden: boolean;
}

export interface SellerQuestionList {
  items: SellerQuestion[];
  total: number;
  page: number;
  limit: number;
}

/** GET /v1/products/{id}/questions — public question list, newest
 * first, hidden questions excluded. */
export function getProductQuestions(
  productId: string,
  params?: { page?: number; limit?: number }
): Promise<QuestionList> {
  const search = new URLSearchParams();
  if (params?.page) search.set("page", String(params.page));
  if (params?.limit) search.set("limit", String(params.limit));
  const qs = search.toString();
  return call(`/v1/products/${encodeURIComponent(productId)}/questions${qs ? `?${qs}` : ""}`);
}

/**
 * POST /v1/products/{id}/questions — ask the seller a question.
 * The API rejects asking about your own store's product (400)
 * and questions on non-visible products (404).
 */
export function askQuestion(
  token: string,
  productId: string,
  body: string
): Promise<ProductQuestion> {
  return call(`/v1/products/${encodeURIComponent(productId)}/questions`, {
    method: "POST",
    token,
    body: { body },
  });
}

/** GET /v1/seller/questions?answered= — the seller Q&A inbox
 * across all their stores; hidden questions included, marked. */
export function listSellerQuestions(
  token: string,
  answered: "false" | "true" | "all"
): Promise<SellerQuestionList> {
  return call(`/v1/seller/questions?answered=${answered}`, { token });
}

/**
 * POST /v1/seller/questions/{id}/answer — answer a question on
 * your store's product. Answering again CORRECTS the answer.
 */
export function answerQuestion(
  token: string,
  questionId: string,
  body: string
): Promise<ProductQuestion> {
  return call(`/v1/seller/questions/${encodeURIComponent(questionId)}/answer`, {
    method: "POST",
    token,
    body: { body },
  });
}

/** POST /v1/seller/questions/{id}/hide — hide a question from
 * the public list (it stays in the inbox, marked hidden). */
export function hideQuestion(
  token: string,
  questionId: string
): Promise<SellerQuestion> {
  return call(`/v1/seller/questions/${encodeURIComponent(questionId)}/hide`, {
    method: "POST",
    token,
  });
}

/** POST /v1/seller/questions/{id}/unhide — undo a hide. */
export function unhideQuestion(
  token: string,
  questionId: string
): Promise<SellerQuestion> {
  return call(`/v1/seller/questions/${encodeURIComponent(questionId)}/unhide`, {
    method: "POST",
    token,
  });
}

/** GET /v1/cart — the signed-in buyer's cart. */
export function getCart(token: string): Promise<Cart> {
  return call("/v1/cart", { token });
}

/**
 * POST /v1/cart/items — add (or accumulate) a product in the
 * cart. `variantId` (Wave 33) picks the variant the line
 * buys; a product with active variants requires one.
 */
export function addCartItem(
  token: string,
  productId: string,
  quantity: number,
  variantId?: string | null
): Promise<Cart> {
  return call("/v1/cart/items", {
    method: "POST",
    token,
    body: { product_id: productId, quantity, variant_id: variantId ?? null },
  });
}

/**
 * PATCH /v1/cart/items/{productId} — set a line's quantity
 * (0 removes). The line is (product, variant) since Wave 33;
 * omitting `variantId` addresses the variant-less line.
 */
export function updateCartItem(
  token: string,
  productId: string,
  quantity: number,
  variantId?: string | null
): Promise<Cart> {
  return call(`/v1/cart/items/${encodeURIComponent(productId)}`, {
    method: "PATCH",
    token,
    body: { quantity, variant_id: variantId ?? null },
  });
}

/** DELETE /v1/cart/items/{productId} — remove one (product, variant) line. */
export function removeCartItem(
  token: string,
  productId: string,
  variantId?: string | null
): Promise<Cart> {
  const qs = variantId
    ? `?variant_id=${encodeURIComponent(variantId)}`
    : "";
  return call(`/v1/cart/items/${encodeURIComponent(productId)}${qs}`, {
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
  /** Loyalty points redeemed at checkout (Wave 35). */
  loyalty_points_used: number;
  /** The discount those points bought (Wave 35); shown as its own line. */
  loyalty_discount_poisha: number;
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

/** The issuing store's block on an invoice (Wave 22). */
export interface InvoiceStore {
  name: string;
  slug: string;
  verified_tier: VerificationTier;
  /** Wave 18 brand colour (#RRGGBB) for the print accent; null when unset. */
  brand_color: string | null;
}

/** An invoice's money block (Wave 22) — this sub-order only, in poisha. */
export interface InvoiceMoney {
  goods_subtotal_poisha: number;
  discount_poisha: number;
  delivery_charge_poisha: number;
  /** Goods − discount + delivery: this slice's total. */
  grand_total_poisha: number;
  /** This slice's share of any advance the COD order collected (Wave 17). */
  advance_paid_poisha: number;
  /** Cash still due on delivery for this slice; 0 for online orders. */
  cod_amount_due_poisha: number;
}

/**
 * GET /v1/{buyer|seller|admin}/sub-orders/{id}/invoice (Wave 22) —
 * the per-seller invoice for one sub-order. The number is
 * sequential PER STORE (INV-000001 is every store's first), the
 * first read of a delivered/returned sub-order issues it, and
 * later reads return the same invoice.
 */
export interface Invoice {
  id: string;
  sub_order_id: string;
  order_id: string;
  invoice_number: string;
  /** The store-local sequence behind `invoice_number`. */
  seq: number;
  issued_at: string;
  /** delivered, or returned when the sale later came back. */
  sub_order_status: string;
  store: InvoiceStore;
  buyer: {
    name: string;
    phone: string;
    address: string;
    zone: string;
  };
  items: OrderItem[];
  money: InvoiceMoney;
  payment_method: string;
  payment_status: string;
}

/** GET /v1/buyer/sub-orders/{id}/invoice — the buyer's copy. */
export function getBuyerInvoice(
  token: string,
  subOrderId: string
): Promise<Invoice> {
  return call(
    `/v1/buyer/sub-orders/${encodeURIComponent(subOrderId)}/invoice`,
    { token }
  );
}

/** GET /v1/seller/sub-orders/{id}/invoice — the seller's copy. */
export function getSellerInvoice(
  token: string,
  subOrderId: string
): Promise<Invoice> {
  return call(
    `/v1/seller/sub-orders/${encodeURIComponent(subOrderId)}/invoice`,
    { token }
  );
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

/**
 * POST /v1/seller/sub-orders/{id}/shipment with a manual courier
 * (Wave 19) — Sundarban, SA Paribahan, Karatoa and Carrybee have
 * no APIs, so the booking writes the shipment directly: the
 * seller books at the counter under the platform's master
 * account and records the receipt number (`manualTrackingId`,
 * when already in hand). `manualCourier` is the KEY from the
 * manual entries of GET /v1/couriers ("sundarban", …).
 */
export function createManualShipment(
  token: string,
  subOrderId: string,
  manualCourier: string,
  manualTrackingId?: string
): Promise<Shipment> {
  const body: {
    courier: string;
    manual_courier: string;
    manual_tracking_id?: string;
  } = { courier: "manual", manual_courier: manualCourier };
  if (manualTrackingId) body.manual_tracking_id = manualTrackingId;
  return call(`/v1/seller/sub-orders/${encodeURIComponent(subOrderId)}/shipment`, {
    method: "POST",
    token,
    body,
  });
}

/**
 * POST /v1/seller/shipments/{id}/manual-status — record one
 * seller-entered status for a manual shipment (Wave 19). The
 * API checks the move against the domain transition table and
 * syncs the sub-order (delivery / return + restock) exactly
 * like a courier-reported status; an illegal move is a 409.
 */
export function setManualShipmentStatus(
  token: string,
  shipmentId: string,
  status: string
): Promise<Shipment> {
  return call(
    `/v1/seller/shipments/${encodeURIComponent(shipmentId)}/manual-status`,
    { method: "POST", token, body: { status } }
  );
}

/**
 * POST /v1/seller/shipments/{id}/manual-tracking — record (or
 * correct) the counter receipt number of a manual shipment
 * after booking (Wave 19). It becomes the consignment id too.
 */
export function setManualTracking(
  token: string,
  shipmentId: string,
  trackingId: string
): Promise<Shipment> {
  return call(
    `/v1/seller/shipments/${encodeURIComponent(shipmentId)}/manual-tracking`,
    { method: "POST", token, body: { tracking_id: trackingId } }
  );
}

/** A fulfillment work bucket (Wave 40): the domain
 * classification of a sub-order's work state. */
export type FulfillmentBucket =
  | "to_confirm"
  | "to_ship"
  | "in_transit"
  | "done"
  | "attention";

/** Bangla label for a fulfillment bucket (display only). */
export function fulfillmentBucketLabel(bucket: string): string {
  switch (bucket) {
    case "to_confirm":
      return "কনফার্ম বাকি";
    case "to_ship":
      return "বুকিং বাকি";
    case "in_transit":
      return "পথে আছে";
    case "done":
      return "সম্পন্ন";
    case "attention":
      return "মনোযোগ দিন";
    default:
      return bucket;
  }
}

/** Per-bucket tallies over a store's queue (Wave 40), plus how
 * many actionable items have breached their SLA. */
export interface FulfillmentCounts {
  to_confirm: number;
  to_ship: number;
  in_transit: number;
  done: number;
  attention: number;
  breached: number;
}

/** The shipment on a queued sub-order (Wave 40). */
export interface FulfillmentShipment {
  id: string;
  courier: string;
  consignment_id: string;
  status: string;
  cod_amount_poisha: number;
  manual_courier_name: string | null;
  manual_tracking_id: string | null;
}

/** One row of the fulfillment queue (Wave 40). */
export interface FulfillmentItem {
  sub_order_id: string;
  order_id: string;
  store_id: string;
  status: string;
  bucket: FulfillmentBucket;
  /** Whole hours since the sub-order was placed. */
  age_hours: number;
  sla_breached: boolean;
  created_at: string;
  items_count: number;
  goods_subtotal_poisha: number;
  discount_poisha: number;
  loyalty_discount_poisha: number;
  delivery_charge_poisha: number;
  /** Cash due on delivery — the booked shipment's stored
   * amount, or the booking computation's preview while
   * unbooked; the same number either way. */
  cod_due_poisha: number;
  delivery_name: string;
  delivery_phone: string;
  delivery_zone: string;
  order_payment_status: string;
  shipment: FulfillmentShipment | null;
}

/** GET /v1/seller/fulfillment — the work queue (Wave 40). */
export interface FulfillmentQueue {
  store_id: string;
  store_name: string;
  sla: { confirm_hours: number; ship_hours: number };
  counts: FulfillmentCounts;
  items: FulfillmentItem[];
}

/**
 * GET /v1/seller/fulfillment — one store's work queue:
 * bucket-classified sub-orders, oldest first, with SLA ages
 * and exact per-bucket counts. Without a store id the
 * caller's own earliest store is described.
 */
export function listFulfillment(
  token: string,
  storeId?: string
): Promise<FulfillmentQueue> {
  const query = storeId ? `?store_id=${encodeURIComponent(storeId)}` : "";
  return call(`/v1/seller/fulfillment${query}`, { token });
}

/** One id's real outcome in a bulk booking (Wave 40). */
export interface BulkBookItemResult {
  sub_order_id: string;
  ok: boolean;
  shipment: Shipment | null;
  error: string | null;
}

/** POST /v1/seller/fulfillment/bulk-book response (Wave 40). */
export interface BulkBookResult {
  store_id: string;
  results: BulkBookItemResult[];
  booked: number;
  failed: number;
}

/**
 * POST /v1/seller/fulfillment/bulk-book — book up to 20 of
 * one store's confirmed sub-orders in one call. Each id runs
 * the single-booking path on its own; the result reports
 * every item's real outcome (a failure never rolls back the
 * others). `courier` mirrors the single endpoint: a name,
 * "auto" for smart routing, or "manual" with its fields.
 */
export function bulkBookShipments(
  token: string,
  input: {
    storeId?: string;
    subOrderIds: string[];
    courier?: string;
    areaId?: number;
    manualCourier?: string;
    manualTrackingId?: string;
  }
): Promise<BulkBookResult> {
  const body: Record<string, unknown> = {
    sub_order_ids: input.subOrderIds,
  };
  if (input.storeId) body.store_id = input.storeId;
  if (input.courier) body.courier = input.courier;
  if (input.areaId !== undefined) body.area_id = input.areaId;
  if (input.manualCourier) body.manual_courier = input.manualCourier;
  if (input.manualTrackingId) body.manual_tracking_id = input.manualTrackingId;
  return call(`/v1/seller/fulfillment/bulk-book`, {
    method: "POST",
    token,
    body,
  });
}

/** Everything one courier label must say (Wave 40). */
export interface FulfillmentLabel {
  sub_order_id: string;
  order_id: string;
  placed_at: string;
  store_name: string;
  store_slug: string;
  seller_phone: string;
  delivery_name: string;
  delivery_phone: string;
  delivery_address: string;
  delivery_zone: string;
  courier: string;
  courier_display_name: string;
  consignment_id: string;
  manual_tracking_id: string | null;
  cod_amount_poisha: number;
  items_count: number;
}

/** GET /v1/seller/fulfillment/labels response (Wave 40):
 * labels for booked ids, named reasons for the rest. */
export interface FulfillmentLabels {
  store_id: string;
  labels: FulfillmentLabel[];
  skipped: { sub_order_id: string | null; reason: string }[];
}

/**
 * GET /v1/seller/fulfillment/labels — label DATA for the
 * print view (rendering is the page's job). Only booked
 * sub-orders get a label; every other id comes back in
 * `skipped` with its reason.
 */
export function getFulfillmentLabels(
  token: string,
  ids: string[],
  storeId?: string
): Promise<FulfillmentLabels> {
  const search = new URLSearchParams();
  search.set("ids", ids.join(","));
  if (storeId) search.set("store_id", storeId);
  return call(`/v1/seller/fulfillment/labels?${search.toString()}`, { token });
}

/** A store's money summary (Wave 9): earnings from DELIVERED
 * sub-orders only, minus what is paid out or waiting in requests. */
export interface Earnings {
  earned_poisha: number;
  paid_out_poisha: number;
  pending_poisha: number;
  /**
   * Total taken from the balance outside payouts (Wave 26):
   * sponsored-promotion purchases. Part of the available
   * balance equation: earned − paid out − pending − charges.
   */
  charges_total_poisha: number;
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
  /** Low-stock alerts (Wave 23), worst first, at most ten. */
  low_stock_items: LowStockItem[];
  /** Product questions still awaiting an answer (Wave 25);
   * hidden questions are excluded. */
  unanswered_questions: number;
  /** Fulfillment queue tallies over the dashboard's scope
   * (Wave 40) — the summary card linking into the queue. */
  fulfillment: FulfillmentCounts;
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
  /** The Idempotency-Key the request carried (Wave 44), or null. */
  idempotency_key: string | null;
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

/**
 * GET /v1/seller/dashboard — store, counts, earnings, recent orders.
 * Wave 34: an optional store id scopes the dashboard to that one
 * store (the caller's role in it is enough to view); without it the
 * caller's own whole-activity dashboard comes back, as before.
 */
export function getSellerDashboard(
  token: string,
  storeId?: string
): Promise<SellerDashboard> {
  const query = storeId ? `?store_id=${encodeURIComponent(storeId)}` : "";
  return call(`/v1/seller/dashboard${query}`, { token });
}

/** A store the signed-in seller serves, with their role in it (Wave 34). */
export interface SellerStore {
  id: string;
  slug: string;
  name: string;
  /** active | suspended */
  status: string;
  verified_tier: VerificationTier;
  /** owner | manager | staff — the caller's role in THIS store. */
  my_role: string;
}

/** GET /v1/seller/stores — every store the caller owns or serves on. */
export function listSellerStores(token: string): Promise<SellerStore[]> {
  return call("/v1/seller/stores", { token });
}

/** Bangla label for a store team role (Wave 34). */
export function storeRoleLabel(role: string): string {
  switch (role) {
    case "owner":
      return "মালিক — Owner";
    case "manager":
      return "ম্যানেজার — Manager";
    case "staff":
      return "স্টাফ — Staff";
    default:
      return role;
  }
}

/** One member of a store's team (Wave 34); the phone is masked. */
export interface StoreMember {
  store_id: string;
  user_id: string;
  phone_masked: string;
  /** manager | staff — the owner is never a member row. */
  role: string;
  created_at: string;
}

/** GET /v1/seller/stores/{id}/members — the team (owner + manager may view). */
export function listStoreMembers(
  token: string,
  storeId: string
): Promise<StoreMember[]> {
  return call(`/v1/seller/stores/${encodeURIComponent(storeId)}/members`, {
    token,
  });
}

/**
 * POST /v1/seller/stores/{id}/members — seat an EXISTING account
 * by phone (owner only). A number with no account is a 404 from
 * the API; there is no invite-by-SMS yet.
 */
export function addStoreMember(
  token: string,
  storeId: string,
  phone: string,
  role: string
): Promise<StoreMember> {
  return call(`/v1/seller/stores/${encodeURIComponent(storeId)}/members`, {
    method: "POST",
    token,
    body: { phone, role },
  });
}

/** PATCH /v1/seller/stores/{id}/members/{userId} — change a role (owner only). */
export function updateStoreMemberRole(
  token: string,
  storeId: string,
  userId: string,
  role: string
): Promise<StoreMember> {
  return call(
    `/v1/seller/stores/${encodeURIComponent(storeId)}/members/${encodeURIComponent(userId)}`,
    { method: "PATCH", token, body: { role } }
  );
}

/** DELETE /v1/seller/stores/{id}/members/{userId} — remove a member (owner only). */
export function removeStoreMember(
  token: string,
  storeId: string,
  userId: string
): Promise<void> {
  return call(
    `/v1/seller/stores/${encodeURIComponent(storeId)}/members/${encodeURIComponent(userId)}`,
    { method: "DELETE", token }
  );
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
 *
 * Wave 44: `idempotencyKey` goes out as the `Idempotency-Key`
 * header. The caller mints ONE key per form session and keeps
 * it across retries, so a double submit or a retried request
 * returns the original payout (200) instead of creating a
 * duplicate; the API answers 409 if the key was already used
 * for a different amount.
 */
export function requestPayout(
  token: string,
  amountPoisha: number,
  idempotencyKey: string
): Promise<Payout> {
  return call("/v1/seller/payouts", {
    method: "POST",
    token,
    body: { amount_poisha: amountPoisha },
    headers: { "Idempotency-Key": idempotencyKey },
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
// Wave 23 — inventory & the stock audit trail. Every stock change
// (checkout, cancel/return restock, or a manual adjustment here)
// writes an audit row on the API side; these types mirror it.

/** One audited stock movement (Wave 23). */
export interface StockAdjustment {
  id: string;
  product_id: string;
  /** The variant whose stock moved (Wave 33); null for product-level moves. */
  variant_id: string | null;
  store_id: string;
  /** seller | system */
  actor: string;
  /** The adjusting seller for manual moves; null for system moves. */
  actor_user_id: string | null;
  delta: number;
  /** On-hand stock right after the movement. */
  new_stock: number;
  /** restock | correction | damage | count_fix | other (seller),
   * or order | cancel_restock | return_restock | rto_restock (system). */
  reason: string;
  note: string | null;
  created_at: string;
}

export interface StockHistory {
  items: StockAdjustment[];
  total: number;
  page: number;
  per_page: number;
}

/** The adjust-stock answer: the product after the move + its audit row. */
export interface AdjustStockResult {
  product: Product;
  adjustment: StockAdjustment;
}

/** One product in the seller inventory overview (Wave 23). */
export interface InventoryItem {
  product_id: string;
  title: string;
  store_slug: string;
  stock: number;
  /** The seller's own threshold; null when the default applies. */
  low_stock_threshold: number | null;
  /** The threshold actually applied (own value or the default 5). */
  effective_threshold: number;
  /** in_stock | low | out_of_stock */
  stock_status: string;
}

export interface Inventory {
  items: InventoryItem[];
  summary: {
    products: number;
    out_of_stock: number;
    /** Only the `low` state; out-of-stock is counted separately. */
    low_stock: number;
  };
}

/** One dashboard low-stock alert row (Wave 23). */
export interface LowStockItem {
  product_id: string;
  title: string;
  stock: number;
  /** low | out_of_stock */
  stock_status: string;
}

/** Bangla label for a stock health status (Wave 23). */
export function stockStatusLabel(status: string): string {
  switch (status) {
    case "in_stock":
      return "স্টক আছে";
    case "low":
      return "কম আছে";
    case "out_of_stock":
      return "স্টক শেষ";
    default:
      return status;
  }
}

/** Bangla label for a stock movement reason (Wave 23). */
export function stockReasonLabel(reason: string): string {
  switch (reason) {
    case "restock":
      return "নতুন স্টক এসেছে";
    case "correction":
      return "সংশোধন";
    case "damage":
      return "নষ্ট / ক্ষতি";
    case "count_fix":
      return "গণনা মিলানো";
    case "other":
      return "অন্যান্য";
    case "order":
      return "অর্ডার";
    case "cancel_restock":
      return "বাতিলে ফেরত";
    case "return_restock":
      return "রিটার্নে ফেরত";
    case "rto_restock":
      return "কুরিয়ার ফেরতে ফেরত";
    default:
      return reason;
  }
}

/** GET /v1/seller/inventory — every own product with its stock health. */
export function getSellerInventory(token: string): Promise<Inventory> {
  return call("/v1/seller/inventory", { token });
}

/**
 * POST /v1/seller/products/{id}/adjust-stock — one manual,
 * audited stock move. The API validates the delta and reason,
 * refuses a move below zero, and returns the audit row.
 */
export function adjustStock(
  token: string,
  productId: string,
  input: { delta: number; reason: string; note?: string }
): Promise<AdjustStockResult> {
  return call(`/v1/seller/products/${encodeURIComponent(productId)}/adjust-stock`, {
    method: "POST",
    token,
    body: input,
  });
}

/** GET /v1/seller/products/{id}/stock-history — the audit trail, newest first. */
export function getStockHistory(
  token: string,
  productId: string,
  params?: { page?: number; per_page?: number }
): Promise<StockHistory> {
  const search = new URLSearchParams();
  if (params?.page) search.set("page", String(params.page));
  if (params?.per_page) search.set("per_page", String(params.per_page));
  const qs = search.toString();
  return call(
    `/v1/seller/products/${encodeURIComponent(productId)}/stock-history${qs ? `?${qs}` : ""}`,
    { token }
  );
}

/**
 * PUT /v1/seller/products/{id}/low-stock-threshold — set the
 * product's warning level, or clear it (null) back to the
 * platform default.
 */
export function setLowStockThreshold(
  token: string,
  productId: string,
  threshold: number | null
): Promise<Product> {
  return call(
    `/v1/seller/products/${encodeURIComponent(productId)}/low-stock-threshold`,
    { method: "PUT", token, body: { threshold } }
  );
}

// ---------------------------------------------------------------------------
// Wave 33 — product variants. A variant is priced as base +
// delta (the API refuses any combination whose final price
// would not be positive) and stocked independently: while a
// product has active variants, its stock lives on them.
// Deactivation is the only delete — the row (and the order
// history pointing at it) stays.
// ---------------------------------------------------------------------------

/** GET /v1/seller/products/{id}/variants — all variants, inactive included. */
export function listSellerVariants(
  token: string,
  productId: string
): Promise<SellerVariant[]> {
  return call(
    `/v1/seller/products/${encodeURIComponent(productId)}/variants`,
    { token }
  );
}

/** POST /v1/seller/products/{id}/variants — create one variant. */
export function createVariant(
  token: string,
  productId: string,
  input: {
    name: string;
    sku?: string;
    /** Difference against the base price, in poisha (may be negative). */
    price_delta_poisha?: number;
    /** Starting stock; writes no audit row, like product creation. */
    stock?: number;
    sort?: number;
  }
): Promise<SellerVariant> {
  return call(`/v1/seller/products/${encodeURIComponent(productId)}/variants`, {
    method: "POST",
    token,
    body: input,
  });
}

/** PATCH /v1/seller/products/{id}/variants/{variantId} — edit a variant. */
export function updateVariant(
  token: string,
  productId: string,
  variantId: string,
  patch: {
    name?: string;
    sku?: string;
    price_delta_poisha?: number;
    is_active?: boolean;
    sort?: number;
  }
): Promise<SellerVariant> {
  return call(
    `/v1/seller/products/${encodeURIComponent(productId)}/variants/${encodeURIComponent(variantId)}`,
    { method: "PATCH", token, body: patch }
  );
}

/**
 * DELETE /v1/seller/products/{id}/variants/{variantId} —
 * deactivate a variant (soft delete; the row stays for order
 * history, and a PATCH with is_active reactivates it).
 */
export function deactivateVariant(
  token: string,
  productId: string,
  variantId: string
): Promise<SellerVariant> {
  return call(
    `/v1/seller/products/${encodeURIComponent(productId)}/variants/${encodeURIComponent(variantId)}`,
    { method: "DELETE", token }
  );
}

/**
 * POST /v1/seller/products/{id}/variants/{variantId}/adjust-stock —
 * one manual, audited stock move on the VARIANT (same reason
 * vocabulary and below-zero refusal as product adjust-stock).
 */
export function adjustVariantStock(
  token: string,
  productId: string,
  variantId: string,
  input: { delta: number; reason: string; note?: string }
): Promise<VariantAdjustStockResult> {
  return call(
    `/v1/seller/products/${encodeURIComponent(productId)}/variants/${encodeURIComponent(variantId)}/adjust-stock`,
    { method: "POST", token, body: input }
  );
}

// ---------------------------------------------------------------------------
// Wave 26 — sponsored product promotions. A promotion is prepaid
// from the seller's earnings balance (there is no other way to
// pay), runs a whole number of days, and while live the product
// sorts first in the default listings carrying the স্পন্সরড
// badge. Cancelling stops the placement immediately and refunds
// NOTHING — the charge stands; the UI says so before purchase.
// ---------------------------------------------------------------------------

/** The public promotion policy (GET /v1/promotions/policy). */
export interface PromotionPolicy {
  price_per_day_poisha: number;
  max_days: number;
  currency_note: string;
}

/** One promotion as the seller sees it (list + cancel views). */
export interface SellerPromotion {
  id: string;
  product_id: string;
  product_title: string;
  store_id: string;
  starts_at: string;
  ends_at: string;
  days: number;
  cost_poisha: number;
  /** active | cancelled — expiry is read off display_status. */
  status: string;
  /** active (live now) | expired | cancelled — API-derived. */
  display_status: string;
  created_at: string;
}

/** The purchase receipt (POST /v1/seller/products/{id}/promote). */
export interface PromoteResult {
  promotion: {
    id: string;
    product_id: string;
    store_id: string;
    seller_user_id: string;
    starts_at: string;
    ends_at: string;
    days: number;
    cost_poisha: number;
    status: string;
    created_at: string;
  };
  cost_poisha: number;
  available_after_poisha: number;
}

/** Bangla-first label for a promotion's display status. */
export function promotionStatusLabel(displayStatus: string): string {
  switch (displayStatus) {
    case "active":
      return "সক্রিয় — চলছে";
    case "expired":
      return "মেয়াদ শেষ";
    case "cancelled":
      return "বাতিল করা হয়েছে";
    default:
      return displayStatus;
  }
}

/** GET /v1/promotions/policy — the current per-day price + cap. */
export function getPromotionPolicy(): Promise<PromotionPolicy> {
  return call("/v1/promotions/policy");
}

/**
 * POST /v1/seller/products/{id}/promote — buy `days` of
 * sponsorship for an own active product, prepaid from the
 * earnings balance. 400 when the balance cannot cover it,
 * 409 while another promotion for the product is live.
 */
export function promoteProduct(
  token: string,
  productId: string,
  days: number
): Promise<PromoteResult> {
  return call(`/v1/seller/products/${encodeURIComponent(productId)}/promote`, {
    method: "POST",
    token,
    body: { days },
  });
}

/** GET /v1/seller/promotions — the caller's promotions, newest first. */
export function listMyPromotions(
  token: string
): Promise<{ items: SellerPromotion[] }> {
  return call("/v1/seller/promotions", { token });
}

/**
 * POST /v1/seller/promotions/{id}/cancel — stop a live
 * promotion now. The charge is NOT refunded.
 */
export function cancelPromotion(
  token: string,
  promotionId: string
): Promise<SellerPromotion> {
  return call(
    `/v1/seller/promotions/${encodeURIComponent(promotionId)}/cancel`,
    { method: "POST", token }
  );
}

// ---------------------------------------------------------------------------
// Wave 36 — seller subscription plans. A plan is prepaid from
// the earnings balance for a 30-day period (no separate
// payment), discounts the store's checkout commission, and
// raises its product cap. Subscribing and cancelling renewal
// are owner-only on the API; managers may view.
// ---------------------------------------------------------------------------

/** One plan in the catalogue (GET /v1/seller/subscription/plans). */
export interface SubscriptionPlan {
  /** free | pro | business */
  key: string;
  /** Poisha per 30-day period (0 on free). */
  price_poisha: number;
  /** Basis points off the store's commission rate. */
  commission_discount_bps: number;
  /** Max products for a store on this plan; a huge sentinel = unlimited. */
  product_cap: number;
  period_days: number;
}

/** A store's subscription as the API reports it (effective plan). */
export interface SubscriptionView {
  store_id: string;
  /** The plan whose benefits apply right now. */
  plan: string;
  /** Stored status: active | cancelled | expired | free (no row). */
  status: string;
  started_at: string | null;
  expires_at: string | null;
  auto_renew: boolean;
  commission_discount_bps: number;
  product_cap: number;
  price_poisha: number;
}

/** The subscribe receipt: the new view + what was charged. */
export interface SubscribeResult {
  subscription: SubscriptionView;
  charged_poisha: number;
  available_after_poisha: number;
}

/** Bangla label for a subscription plan key (Wave 36). */
export function planLabel(plan: string): string {
  switch (plan) {
    case "pro":
      return "প্রো — Pro";
    case "business":
      return "বিজনেস — Business";
    case "free":
      return "ফ্রি — Free";
    default:
      return plan;
  }
}

/** Is this product cap the unlimited sentinel (business plan)? */
export function isUnlimitedCap(cap: number): boolean {
  return cap >= Number.MAX_SAFE_INTEGER;
}

export function getSubscriptionPlans(
  token: string
): Promise<{ items: SubscriptionPlan[] }> {
  return call("/v1/seller/subscription/plans", { token });
}

export function getMySubscription(
  token: string,
  storeId?: string
): Promise<SubscriptionView> {
  const query = storeId ? `?store_id=${encodeURIComponent(storeId)}` : "";
  return call(`/v1/seller/subscription${query}`, { token });
}

export function subscribePlan(
  token: string,
  plan: string,
  storeId?: string
): Promise<SubscribeResult> {
  const query = storeId ? `?store_id=${encodeURIComponent(storeId)}` : "";
  return call(`/v1/seller/subscription${query}`, {
    method: "POST",
    token,
    body: { plan },
  });
}

export function cancelSubscription(
  token: string,
  storeId?: string
): Promise<SubscriptionView> {
  const query = storeId ? `?store_id=${encodeURIComponent(storeId)}` : "";
  return call(`/v1/seller/subscription/cancel${query}`, {
    method: "POST",
    token,
  });
}

// ---------------------------------------------------------------------------
// Wave 37 — third-party / mobile API keys. A key acts as its
// owner, narrowed by scopes; only its hash is stored, so the
// full key exists ONLY in the create response — the list and
// revoke calls below can never return it. Key management is
// JWT-only on the API (a key cannot manage keys).
// ---------------------------------------------------------------------------

/** One API key as its owner may see it (GET /v1/api-keys). */
export interface ApiKeyView {
  id: string;
  name: string;
  /** First 12 characters of the full key — display only. */
  key_prefix: string;
  /** Canonical scope set: ["read"] or ["read", "write"]. */
  scopes: string[];
  /** active | revoked | expired (derived on the API). */
  status: string;
  last_used_at: string | null;
  expires_at: string | null;
  created_at: string;
}

/** The create response: the key view PLUS the full key, once. */
export interface CreatedApiKey extends ApiKeyView {
  /** The full key — shown once; it can never be read back. */
  key: string;
}

/** Bangla label for an API key status (Wave 37). */
export function apiKeyStatusLabel(status: string): string {
  switch (status) {
    case "active":
      return "সক্রিয় — Active";
    case "revoked":
      return "বাতিল করা — Revoked";
    case "expired":
      return "মেয়াদ শেষ — Expired";
    default:
      return status;
  }
}

/** GET /v1/api-keys — my keys (metadata + prefixes only). */
export function listApiKeys(token: string): Promise<{ items: ApiKeyView[] }> {
  return call("/v1/api-keys", { token });
}

/** POST /v1/api-keys — mint a key; the response carries it once. */
export function createApiKey(
  token: string,
  name: string,
  scopes: string[],
  expiresInDays?: number | null
): Promise<CreatedApiKey> {
  return call("/v1/api-keys", {
    method: "POST",
    token,
    body: { name, scopes, expires_in_days: expiresInDays ?? null },
  });
}

/** DELETE /v1/api-keys/{id} — revoke a key (idempotent). */
export function revokeApiKey(token: string, id: string): Promise<ApiKeyView> {
  return call(`/v1/api-keys/${encodeURIComponent(id)}`, {
    method: "DELETE",
    token,
  });
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
  /** The store's logo URL (Wave 51 ext); null when unset. */
  logo_url: string | null;
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
  /** The Idempotency-Key the request carried (Wave 44), or null. */
  idempotency_key: string | null;
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
  /**
   * KYC payout gate (Wave 21): when true, a store must hold at
   * least the basic verification tier to request a payout.
   */
  kyc_payout_gate: boolean;
  /**
   * Per-day sponsored-promotion price in poisha (Wave 26) —
   * the effective value new promotions are charged at.
   */
  promotion_price_per_day_poisha: number;
  /** Per-product image cap (Wave 51 extension). */
  media_max_images_per_product: number;
  /** Per-store media quota in megabytes (Wave 51 extension). */
  media_store_quota_mb: number;
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
  /** Photo URL lifted from the post text (Wave 51 ext). */
  image_url: string | null;
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
  /**
   * Photo URL to attach as the product's first image (Wave 51).
   * A bad URL never fails the import — the product is created
   * without the image.
   */
  image_url?: string;
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
    kyc_payout_gate?: boolean;
    promotion_price_per_day_poisha?: number;
    media_max_images_per_product?: number;
    media_store_quota_mb?: number;
  }
): Promise<AdminSettings> {
  return call("/v1/admin/settings", { method: "PUT", token, body: patch });
}

// ---------------------------------------------------------------------------
// Wave 21 — seller KYC & verification tiers
// ---------------------------------------------------------------------------

/**
 * The seller's own verification state (GET /v1/seller/store/
 * verification). `status` is none until the first application.
 * The NID appears ONLY masked — the API never echoes a full
 * NID back, not even to the seller who typed it.
 */
export interface SellerVerification {
  /** none | pending | approved | rejected */
  status: string;
  tier: VerificationTier;
  nid_masked: string | null;
  trade_license_number: string | null;
  document_url: string | null;
  /** The reviewer's reason, when the application was rejected. */
  reviewer_note: string | null;
  submitted_at: string | null;
  reviewed_at: string | null;
}

/** What a seller submits to apply for verification. */
export interface SubmitVerificationInput {
  nid_number: string;
  trade_license_number?: string;
  document_url?: string;
}

/**
 * One row of the admin review queue (GET /v1/admin/
 * verifications). Owner phone and NID are masked here too;
 * the full NID lives behind the separate full-view call.
 */
export interface AdminVerification {
  store_id: string;
  store_slug: string;
  store_name: string;
  owner_phone_masked: string;
  /** pending | approved | rejected */
  status: string;
  tier: VerificationTier;
  nid_masked: string;
  has_trade_license: boolean;
  document_url: string | null;
  reviewer_note: string | null;
  submitted_at: string;
  reviewed_at: string | null;
}

/**
 * The admin full view (GET /v1/admin/verifications/{storeId}/
 * full): the ONLY API response carrying a complete NID
 * number, so a reviewer can check it against the document.
 */
export interface AdminVerificationFull {
  store_id: string;
  nid_number: string;
}

/** GET /v1/seller/store/verification — the caller's own KYC state. */
export function getSellerVerification(
  token: string
): Promise<SellerVerification> {
  return call("/v1/seller/store/verification", { token });
}

/**
 * PUT /v1/seller/store/verification — submit (or resubmit) the
 * caller's application. A still-pending application is
 * replaced; an approved store gets a 409.
 */
export function submitSellerVerification(
  token: string,
  input: SubmitVerificationInput
): Promise<SellerVerification> {
  return call("/v1/seller/store/verification", {
    method: "PUT",
    token,
    body: input,
  });
}

/** GET /v1/admin/verifications — the review queue (default: pending). */
export function listAdminVerifications(
  token: string,
  status?: string
): Promise<AdminVerification[]> {
  const qs = status ? `?status=${encodeURIComponent(status)}` : "";
  return call(`/v1/admin/verifications${qs}`, { token });
}

/**
 * GET /v1/admin/verifications/{storeId}/full — reveal the full
 * NID for one application. Reviewer-only; call it only on an
 * explicit reveal action, never while listing the queue.
 */
export function getAdminVerificationFull(
  token: string,
  storeId: string
): Promise<AdminVerificationFull> {
  return call(
    `/v1/admin/verifications/${encodeURIComponent(storeId)}/full`,
    { token }
  );
}

/** POST /v1/admin/verifications/{storeId}/approve — grant a tier. */
export function approveVerification(
  token: string,
  storeId: string,
  tier: "basic" | "verified"
): Promise<SellerVerification> {
  return call(
    `/v1/admin/verifications/${encodeURIComponent(storeId)}/approve`,
    { method: "POST", token, body: { tier } }
  );
}

/** POST /v1/admin/verifications/{storeId}/reject — reject with a note. */
export function rejectVerification(
  token: string,
  storeId: string,
  note: string
): Promise<SellerVerification> {
  return call(
    `/v1/admin/verifications/${encodeURIComponent(storeId)}/reject`,
    { method: "POST", token, body: { note } }
  );
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
  /**
   * Wave 19: for a manual shipment, the courier's display name
   * and the counter receipt number (null until the seller
   * records it). Both null for API couriers.
   */
  manual_courier_name: string | null;
  manual_tracking_id: string | null;
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

/** The answer to a wishlist add/remove (Wave 24): the
 * resulting state of that product on the caller's list. */
export interface WishlistToggle {
  wishlisted: boolean;
  product_id: string;
}

/** The caller's saved products as full product views (Wave 24). */
export interface Wishlist {
  items: Product[];
}

/** Just the saved product ids (Wave 24) — the heart toggles' read. */
export interface WishlistIds {
  product_ids: string[];
}

/** POST /v1/wishlist/{productId} — save a product for later.
 * Idempotent: saving an already-saved product is a 200. */
export function addToWishlist(
  token: string,
  productId: string
): Promise<WishlistToggle> {
  return call(`/v1/wishlist/${encodeURIComponent(productId)}`, {
    method: "POST",
    token,
  });
}

/** DELETE /v1/wishlist/{productId} — unsave a product.
 * Idempotent: removing an absent product is a 200 too. */
export function removeFromWishlist(
  token: string,
  productId: string
): Promise<WishlistToggle> {
  return call(`/v1/wishlist/${encodeURIComponent(productId)}`, {
    method: "DELETE",
    token,
  });
}

/** GET /v1/wishlist — the caller's saved products, newest first. */
export function getWishlist(token: string): Promise<Wishlist> {
  return call("/v1/wishlist", { token });
}

/** GET /v1/wishlist/ids — only the saved ids, for heart states. */
export function getWishlistIds(token: string): Promise<WishlistIds> {
  return call("/v1/wishlist/ids", { token });
}

/** Related products for one product (Wave 24): the shared
 * Product shape in rank order (category, store, rating). */
export interface RelatedProducts {
  items: Product[];
}

/** GET /v1/products/{id}/related — public; 404 when the
 * product itself is not publicly visible. */
export function getRelatedProducts(
  productId: string,
  limit?: number
): Promise<RelatedProducts> {
  const search = new URLSearchParams();
  if (limit) search.set("limit", String(limit));
  const qs = search.toString();
  return call(
    `/v1/products/${encodeURIComponent(productId)}/related${qs ? `?${qs}` : ""}`
  );
}

/** One also-bought companion (Wave 24): the product view plus
 * how many distinct orders pair it with the base product. */
export interface AlsoBoughtItem extends Product {
  co_count: number;
}

/** The also-bought rail (Wave 24); honestly empty when the
 * product has no cross-store co-purchases yet. */
export interface AlsoBought {
  items: AlsoBoughtItem[];
}

/** GET /v1/products/{id}/also-bought — public co-purchase rail. */
export function getAlsoBought(
  productId: string,
  limit?: number
): Promise<AlsoBought> {
  const search = new URLSearchParams();
  if (limit) search.set("limit", String(limit));
  const qs = search.toString();
  return call(
    `/v1/products/${encodeURIComponent(productId)}/also-bought${qs ? `?${qs}` : ""}`
  );
}

/**
 * The personalised rail (Wave 24). `personalized` false means
 * the answer is the top-rated fallback — the caller has no
 * category signals (delivered purchases / wishlist) yet.
 */
export interface Recommendations {
  personalized: boolean;
  items: Product[];
}

/** GET /v1/recommendations — the caller's discovery rail. */
export function getRecommendations(
  token: string,
  limit?: number
): Promise<Recommendations> {
  const search = new URLSearchParams();
  if (limit) search.set("limit", String(limit));
  const qs = search.toString();
  return call(`/v1/recommendations${qs ? `?${qs}` : ""}`, { token });
}

/**
 * Seller analytics (Wave 27). Every figure is computed by the
 * API from the ledger tables over the last `days` Asia/Dhaka
 * calendar days (the API clamps the window to 7–90): goods
 * exclude cancelled sub-orders, earnings are the DELIVERED
 * sub-orders' payout snapshots, and `return_rate_bps` is
 * returned / (delivered + returned) in basis points.
 */
export interface SellerAnalyticsTotals {
  sub_orders: number;
  goods_poisha: number;
  earnings_poisha: number;
  delivered: number;
  returned: number;
  cancelled: number;
  return_rate_bps: number;
}

/** One day of the seller series (a Dhaka calendar day). */
export interface SellerDailyRow {
  date: string;
  sub_orders: number;
  goods_poisha: number;
  earnings_poisha: number;
}

/** One top-selling product of the window (Wave 27). */
export interface TopProduct {
  product_id: string;
  title: string;
  units: number;
  goods_poisha: number;
}

/** One status bucket of a breakdown (Wave 27). */
export interface StatusCount {
  status: string;
  count: number;
}

/** One courier bucket of a breakdown (Wave 27). */
export interface CourierCount {
  courier: string;
  count: number;
}

export interface SellerAnalytics {
  days: number;
  totals: SellerAnalyticsTotals;
  daily: SellerDailyRow[];
  top_products: TopProduct[];
  status_breakdown: StatusCount[];
  courier_breakdown: CourierCount[];
}

/** GET /v1/seller/analytics?days= — the caller's own stores only. */
export function getSellerAnalytics(
  token: string,
  days?: number
): Promise<SellerAnalytics> {
  const qs = days ? `?days=${days}` : "";
  return call(`/v1/seller/analytics${qs}`, { token });
}

/**
 * GET /v1/seller/analytics/export.csv?days= — the daily series
 * as a CSV file (the `orders` column counts sub-orders).
 * Returns the raw Blob for a download click; unlike `call`
 * this does not parse JSON.
 */
export async function downloadSellerAnalyticsCsv(
  token: string,
  days?: number
): Promise<Blob> {
  const qs = days ? `?days=${days}` : "";
  const res = await fetch(`${apiBase()}/v1/seller/analytics/export.csv${qs}`, {
    headers: { Authorization: `Bearer ${token}` },
  });
  if (!res.ok) {
    throw new Error(`CSV ডাউনলোড করা যায়নি (HTTP ${res.status})`);
  }
  return res.blob();
}

/**
 * Data portability exports (Wave 31): the caller's own record
 * as one downloadable JSON document — for a seller, the store,
 * the full catalogue (drafts included), sub-orders with items,
 * payouts, coupons, promotions and the lifetime charges total;
 * for a buyer, the profile (own phone in full), orders with
 * sub-orders and items, reviews, wishlist and asked questions.
 * Like the analytics CSV download, these return the Blob and
 * the page saves it under a local name; direct API callers get
 * the server's canonical filename from Content-Disposition
 * (the domain `export_filename`).
 */
export async function downloadSellerExport(token: string): Promise<Blob> {
  const res = await fetch(`${apiBase()}/v1/seller/export`, {
    headers: { Authorization: `Bearer ${token}` },
  });
  if (!res.ok) {
    throw new Error(`এক্সপোর্ট ডাউনলোড করা যায়নি (HTTP ${res.status})`);
  }
  return res.blob();
}

export async function downloadBuyerExport(token: string): Promise<Blob> {
  const res = await fetch(`${apiBase()}/v1/buyer/export`, {
    headers: { Authorization: `Bearer ${token}` },
  });
  if (!res.ok) {
    throw new Error(`এক্সপোর্ট ডাউনলোড করা যায়নি (HTTP ${res.status})`);
  }
  return res.blob();
}

/**
 * Platform analytics (Wave 27, admin). GMV is goods − discount
 * + delivery over NON-cancelled sub-orders; commission is
 * goods − discount − seller payout over DELIVERED sub-orders
 * only; the window is the last `days` Asia/Dhaka days.
 */
export interface AdminAnalyticsTotals {
  gmv_poisha: number;
  orders: number;
  sub_orders: number;
  commission_poisha: number;
  new_users: number;
  new_stores: number;
  payouts: {
    requested_count: number;
    requested_poisha: number;
    paid_poisha: number;
  };
  charges_poisha: number;
  coupons: {
    redemptions: number;
    discount_poisha: number;
  };
}

/** One top store of the window, by GMV (Wave 27). */
export interface TopStore {
  store_id: string;
  slug: string;
  name: string;
  gmv_poisha: number;
  sub_orders: number;
}

/** One day of the platform series (a Dhaka calendar day). */
export interface AdminDailyRow {
  date: string;
  orders: number;
  gmv_poisha: number;
}

export interface AdminAnalytics {
  days: number;
  totals: AdminAnalyticsTotals;
  top_stores: TopStore[];
  courier_breakdown: CourierCount[];
  daily: AdminDailyRow[];
}

/** GET /v1/admin/analytics?days= — admin only (the API answers 403 otherwise). */
export function getAdminAnalytics(
  token: string,
  days?: number
): Promise<AdminAnalytics> {
  const qs = days ? `?days=${days}` : "";
  return call(`/v1/admin/analytics${qs}`, { token });
}

/**
 * One admin audit-log entry (Wave 28): a consequential admin
 * action, written by the API inside the action's own
 * transaction. The acting admin is identified by MASKED phone
 * only — the audit API never returns a full number. `detail`
 * is the action's non-secret context (the changed setting
 * keys, the granted verification tier, the payout transition
 * endpoints); it never contains values of secrets.
 */
export interface AdminAuditEntry {
  id: string;
  admin_user_id: string;
  admin_phone_masked: string;
  action: string;
  target_type: string;
  target_id: string | null;
  detail: Record<string, unknown> | null;
  created_at: string;
}

export interface AdminAuditList {
  items: AdminAuditEntry[];
  total: number;
  page: number;
  limit: number;
}

/** GET /v1/admin/audit-log — admin only (the API answers 403 otherwise). */
export function getAdminAuditLog(
  token: string,
  params?: { page?: number; perPage?: number; action?: string }
): Promise<AdminAuditList> {
  const search = new URLSearchParams();
  if (params?.page) search.set("page", String(params.page));
  if (params?.perPage) search.set("per_page", String(params.perPage));
  if (params?.action) search.set("action", params.action);
  const qs = search.toString();
  return call(`/v1/admin/audit-log${qs ? `?${qs}` : ""}`, { token });
}

/**
 * Notification outbox counts (Wave 29): how many queued SMS
 * notifications sit in each status. `failed` is the health
 * signal — rows that exhausted their 3 send attempts (in a
 * deployment without an SMS gateway, every row lands there
 * with `sms_not_configured` recorded).
 */
export interface NotificationCounts {
  pending: number;
  sending: number;
  sent: number;
  failed: number;
}

/**
 * One outbox row as the admin sees it (Wave 29). The phone is
 * MASKED — the API never returns the full number here.
 */
export interface AdminNotification {
  id: string;
  kind: string;
  phone_masked: string;
  status: string;
  attempts: number;
  last_error: string | null;
  created_at: string;
  sent_at: string | null;
}

export interface AdminNotificationList {
  counts: NotificationCounts;
  items: AdminNotification[];
}

/** GET /v1/admin/notifications — admin only (the API answers 403 otherwise). */
export function getAdminNotifications(
  token: string,
  params?: { status?: string; limit?: number }
): Promise<AdminNotificationList> {
  const search = new URLSearchParams();
  if (params?.status) search.set("status", params.status);
  if (params?.limit) search.set("limit", String(params.limit));
  const qs = search.toString();
  return call(`/v1/admin/notifications${qs ? `?${qs}` : ""}`, { token });
}

// ---------------------------------------------------------------------------
// Wave 43 — observability: the admin metrics snapshot. Point-in-time
// counts from GET /v1/admin/metrics (admin only); the definitions
// live on the API — orders_today GMV is the Wave 27 formula (goods
// − discount + delivery over the day's non-cancelled sub-orders),
// pending payouts are the `requested` queue, open returns are the
// non-terminal requests, and a non-zero `sending` backlog that
// never drains is the stuck-worker signal.
// ---------------------------------------------------------------------------

/** Point-in-time platform health snapshot (GET /v1/admin/metrics). */
export interface AdminMetrics {
  orders_today: { count: number; gmv_poisha: number };
  /** Parent-order counts per status (placed/confirmed/delivered/cancelled). */
  orders_by_status: Record<string, number>;
  pending_payouts: { count: number; amount_poisha: number };
  open_returns: number;
  notification_backlog: {
    pending: number;
    sending: number;
    sent: number;
    failed: number;
  };
  stale_shipments: number;
  users_total: number;
  stores_total: number;
  products_active: number;
  /** Media footprint (Wave 51): product-image counts + stored bytes. */
  media: {
    images_total: number;
    stored_images: number;
    external_images: number;
    stored_bytes: number;
    /** The five heaviest stores by stored bytes (Wave 51 ext). */
    top_stores: {
      store_id: string;
      store_slug: string;
      store_name: string;
      images: number;
      stored_bytes: number;
    }[];
  };
  /** Merchandising engagement (Wave 52): banner clicks + collection views. */
  merchandising: {
    banner_clicks_total: number;
    collection_views_total: number;
    banner_clicks_today: number;
    collection_views_today: number;
  };
}

/** GET /v1/admin/metrics — admin only (the API answers 403 otherwise). */
export function getAdminMetrics(token: string): Promise<AdminMetrics> {
  return call("/v1/admin/metrics", { token });
}

// ---------------------------------------------------------------------------
// Merchandising (Wave 52): home banners, collections, flash
// sales, the home feed, and the event beacon. The admin
// writes are JWT-admin only on the API side (API keys are
// refused on the whole /v1/admin/* prefix); the feed and the
// collection landing read are public.
// ---------------------------------------------------------------------------

/** One home banner as the API serves it (admin + feed share the shape). */
export interface HomeBanner {
  id: string;
  title: string;
  subtitle: string | null;
  link_url: string | null;
  /** Display URL: the sized upload or the hotlink; null = text-only. */
  image_url: string | null;
  position: number;
  starts_at: string | null;
  ends_at: string | null;
  is_active: boolean;
  created_at: string;
  updated_at: string;
}

export interface BannerList {
  items: HomeBanner[];
  total: number;
  page: number;
  limit: number;
}

/** The banner write payload (create: title required; update: all optional). */
export interface BannerInput {
  title?: string;
  subtitle?: string | null;
  link_url?: string | null;
  external_image_url?: string | null;
  position?: number;
  starts_at?: string | null;
  ends_at?: string | null;
  is_active?: boolean;
}

/** GET /v1/admin/banners — every banner, carousel order. */
export function listAdminBanners(
  token: string,
  params?: { page?: number; limit?: number }
): Promise<BannerList> {
  const search = new URLSearchParams();
  if (params?.page) search.set("page", String(params.page));
  if (params?.limit) search.set("limit", String(params.limit));
  const qs = search.toString();
  return call(`/v1/admin/banners${qs ? `?${qs}` : ""}`, { token });
}

/** POST /v1/admin/banners. */
export function createAdminBanner(
  token: string,
  input: BannerInput & { title: string }
): Promise<HomeBanner> {
  return call("/v1/admin/banners", { method: "POST", token, body: input });
}

/** PATCH /v1/admin/banners/:id — set-only optional fields. */
export function updateAdminBanner(
  token: string,
  id: string,
  input: BannerInput
): Promise<HomeBanner> {
  return call(`/v1/admin/banners/${id}`, { method: "PATCH", token, body: input });
}

/** DELETE /v1/admin/banners/:id (uploaded objects are removed too). */
export function deleteAdminBanner(
  token: string,
  id: string
): Promise<{ deleted: boolean; id: string }> {
  return call(`/v1/admin/banners/${id}`, { method: "DELETE", token });
}

/**
 * POST /v1/admin/banners/:id/image — upload the banner's
 * image through the media pipeline (multipart, the `file`
 * field). Becomes the banner's one image source.
 */
export function uploadAdminBannerImage(
  token: string,
  id: string,
  file: File
): Promise<HomeBanner> {
  const form = new FormData();
  form.append("file", file);
  return callForm(`/v1/admin/banners/${id}/image`, form, { token });
}

/** DELETE /v1/admin/banners/:id/image — remove the uploaded image. */
export function deleteAdminBannerImage(
  token: string,
  id: string
): Promise<HomeBanner> {
  return call(`/v1/admin/banners/${id}/image`, { method: "DELETE", token });
}

/** One curated collection (membership lives in the detail shape). */
export interface MerchCollection {
  id: string;
  slug: string;
  title: string;
  subtitle: string | null;
  is_active: boolean;
  position: number;
  created_at: string;
  updated_at: string;
}

/** A collection plus its products in membership order. */
export interface MerchCollectionDetail extends MerchCollection {
  products: Product[];
}

export interface MerchCollectionList {
  items: MerchCollection[];
  total: number;
  page: number;
  limit: number;
}

export interface CollectionInput {
  slug?: string;
  title?: string;
  subtitle?: string | null;
  is_active?: boolean;
  position?: number;
}

/** GET /v1/admin/collections — every collection, home order. */
export function listAdminCollections(
  token: string,
  params?: { page?: number; limit?: number }
): Promise<MerchCollectionList> {
  const search = new URLSearchParams();
  if (params?.page) search.set("page", String(params.page));
  if (params?.limit) search.set("limit", String(params.limit));
  const qs = search.toString();
  return call(`/v1/admin/collections${qs ? `?${qs}` : ""}`, { token });
}

/** POST /v1/admin/collections. */
export function createAdminCollection(
  token: string,
  input: CollectionInput & { slug: string; title: string }
): Promise<MerchCollection> {
  return call("/v1/admin/collections", { method: "POST", token, body: input });
}

/** GET /v1/admin/collections/:id — the collection + its products. */
export function getAdminCollection(
  token: string,
  id: string
): Promise<MerchCollectionDetail> {
  return call(`/v1/admin/collections/${id}`, { token });
}

/** PATCH /v1/admin/collections/:id. */
export function updateAdminCollection(
  token: string,
  id: string,
  input: CollectionInput
): Promise<MerchCollection> {
  return call(`/v1/admin/collections/${id}`, {
    method: "PATCH",
    token,
    body: input,
  });
}

/** DELETE /v1/admin/collections/:id (membership cascades). */
export function deleteAdminCollection(
  token: string,
  id: string
): Promise<{ deleted: boolean; id: string }> {
  return call(`/v1/admin/collections/${id}`, { method: "DELETE", token });
}

/**
 * PUT /v1/admin/collections/:id/products — replace the
 * membership with this ordered list (the list IS the order).
 */
export function setAdminCollectionProducts(
  token: string,
  id: string,
  productIds: string[]
): Promise<MerchCollectionDetail> {
  return call(`/v1/admin/collections/${id}/products`, {
    method: "PUT",
    token,
    body: { product_ids: productIds },
  });
}

/** GET /v1/collections/:slug — the public landing data (404 when unpublished). */
export function getCollection(slug: string): Promise<MerchCollectionDetail> {
  return call(`/v1/collections/${encodeURIComponent(slug)}`);
}

/** One flash sale as the admin lists serve it. */
export interface FlashSale {
  id: string;
  title: string;
  starts_at: string;
  ends_at: string;
  is_active: boolean;
  item_count: number;
  created_at: string;
  updated_at: string;
}

export interface FlashSaleList {
  items: FlashSale[];
  total: number;
  page: number;
  limit: number;
}

/** One flash sale item in the admin editor: terms + live cap accounting. */
export interface FlashSaleItemView {
  product_id: string;
  product_title: string;
  base_price_poisha: number;
  sale_price_poisha: number;
  stock_cap: number | null;
  sold_at_sale: number;
  cap_remaining: number | null;
}

/** A flash sale plus its item views. */
export interface FlashSaleDetail extends FlashSale {
  items: FlashSaleItemView[];
}

export interface FlashSaleInput {
  title?: string;
  starts_at?: string;
  ends_at?: string;
  is_active?: boolean;
}

/** One item of the replace-items payload (prices in poisha). */
export interface FlashSaleItemInput {
  product_id: string;
  sale_price_poisha: number;
  stock_cap?: number | null;
}

/** GET /v1/admin/flash-sales — newest window first. */
export function listAdminFlashSales(
  token: string,
  params?: { page?: number; limit?: number }
): Promise<FlashSaleList> {
  const search = new URLSearchParams();
  if (params?.page) search.set("page", String(params.page));
  if (params?.limit) search.set("limit", String(params.limit));
  const qs = search.toString();
  return call(`/v1/admin/flash-sales${qs ? `?${qs}` : ""}`, { token });
}

/** POST /v1/admin/flash-sales. */
export function createAdminFlashSale(
  token: string,
  input: FlashSaleInput & { title: string; starts_at: string; ends_at: string }
): Promise<FlashSale> {
  return call("/v1/admin/flash-sales", { method: "POST", token, body: input });
}

/** GET /v1/admin/flash-sales/:id — the sale + item views. */
export function getAdminFlashSale(
  token: string,
  id: string
): Promise<FlashSaleDetail> {
  return call(`/v1/admin/flash-sales/${id}`, { token });
}

/** PATCH /v1/admin/flash-sales/:id. */
export function updateAdminFlashSale(
  token: string,
  id: string,
  input: FlashSaleInput
): Promise<FlashSale> {
  return call(`/v1/admin/flash-sales/${id}`, {
    method: "PATCH",
    token,
    body: input,
  });
}

/** DELETE /v1/admin/flash-sales/:id (items cascade). */
export function deleteAdminFlashSale(
  token: string,
  id: string
): Promise<{ deleted: boolean; id: string }> {
  return call(`/v1/admin/flash-sales/${id}`, { method: "DELETE", token });
}

/**
 * PUT /v1/admin/flash-sales/:id/items — replace the item
 * list. Every price is validated against the product's
 * current base price on the API (a genuine reduction only).
 */
export function setAdminFlashSaleItems(
  token: string,
  id: string,
  items: FlashSaleItemInput[]
): Promise<FlashSaleDetail> {
  return call(`/v1/admin/flash-sales/${id}/items`, {
    method: "PUT",
    token,
    body: { items },
  });
}

/** One category tile of the home feed. */
export interface HomeCategory {
  category: string;
  product_count: number;
  image_url: string | null;
}

/** One flash card of the home feed: the product view + the sale terms. */
export interface HomeFlashItem extends Product {
  sale_price_poisha: number;
  stock_cap: number | null;
  sold_at_sale: number;
}

/** The feed's flash sale block (null in the feed when no sale is live). */
export interface HomeFlashSale {
  id: string;
  title: string;
  starts_at: string;
  ends_at: string;
  items: HomeFlashItem[];
}

/** One collection section of the home feed. */
export interface HomeCollectionSection extends MerchCollection {
  products: Product[];
}

/** One featured (verified) store tile of the home feed. */
export interface FeaturedStore {
  id: string;
  slug: string;
  name: string;
  logo_url: string | null;
  banner_url: string | null;
  brand_color: string | null;
  verified_tier: string;
  product_count: number;
}

/** GET /v1/home — everything the home page renders, in one response. */
export interface HomeFeed {
  banners: HomeBanner[];
  categories: HomeCategory[];
  flash_sale: HomeFlashSale | null;
  collections: HomeCollectionSection[];
  new_arrivals: Product[];
  top_rated: Product[];
  best_sellers: Product[];
  promoted: Product[];
  featured_stores: FeaturedStore[];
}

/** GET /v1/home — the public home feed. */
export function getHomeFeed(): Promise<HomeFeed> {
  return call("/v1/home");
}

/** GET /v1/categories — the full category list (Wave 53). */
export interface CategoryList {
  categories: HomeCategory[];
}

/**
 * GET /v1/categories — every category label in the
 * catalogue with its product count and cover image, from
 * the same aggregate the home feed's tiles use (one
 * vocabulary, two surfaces). Sorted by count desc.
 */
export function getCategories(): Promise<CategoryList> {
  return call("/v1/categories");
}

/**
 * A saved search's filter set (Wave 53): exactly the
 * browse's filters minus pagination. The API stores the
 * canonical form — lists deduplicated, the sort spelled
 * out — so what comes back can differ slightly from what
 * was sent.
 */
export interface SavedSearchFilters {
  q: string | null;
  categories: string[];
  store_slugs: string[];
  min_price_poisha: number | null;
  max_price_poisha: number | null;
  min_rating: number | null;
  in_stock: boolean;
  on_sale: boolean;
  sort: string | null;
}

/** One saved search (Wave 53): the buyer's own browse state. */
export interface SavedSearch {
  id: string;
  name: string;
  filters: SavedSearchFilters;
  created_at: string;
}

/** The saved-search list envelope: the caller's own, newest first. */
export interface SavedSearchList {
  items: SavedSearch[];
  total: number;
}

/** GET /v1/buyer/saved-searches — the caller's saved searches. */
export function listSavedSearches(token: string): Promise<SavedSearchList> {
  return call("/v1/buyer/saved-searches", { token });
}

/** POST /v1/buyer/saved-searches — save the current browse state. */
export function createSavedSearch(
  token: string,
  name: string,
  filters: SavedSearchFilters
): Promise<SavedSearch> {
  return call("/v1/buyer/saved-searches", {
    method: "POST",
    token,
    body: { name, filters },
  });
}

/** DELETE /v1/buyer/saved-searches/{id} — remove one (owner only). */
export function deleteSavedSearch(token: string, id: string): Promise<void> {
  return call(`/v1/buyer/saved-searches/${encodeURIComponent(id)}`, {
    method: "DELETE",
    token,
  });
}

/** One row of the admin top-searches list (Wave 53). */
export interface TopSearch {
  query_text: string;
  searches: number;
  avg_results: number;
}

/** One row of the admin zero-result list (Wave 53). */
export interface ZeroResultSearch {
  query_text: string;
  searches: number;
  last_searched_at: string;
}

/**
 * GET /v1/admin/search-analytics (Wave 53): the trailing
 * window's search totals, most-run query texts and
 * zero-result texts. The underlying log carries no user
 * id by design — this is catalogue-gap data, not tracking.
 */
/** One row of the admin top filter-combinations list (Wave 53). */
export interface TopFilterCombination {
  filters: SavedSearchFilters;
  searches: number;
  avg_results: number;
}

export interface SearchAnalytics {
  window_days: number;
  total_searches: number;
  top_searches: TopSearch[];
  zero_result_searches: ZeroResultSearch[];
  top_filter_combinations: TopFilterCombination[];
}

/** GET /v1/admin/search-analytics — admin only; `days` widens the window. */
export function getSearchAnalytics(
  token: string,
  days?: number
): Promise<SearchAnalytics> {
  const qs = days ? `?days=${days}` : "";
  return call(`/v1/admin/search-analytics${qs}`, { token });
}

/** One popular search text (Wave 53). */
export interface PopularSearch {
  query_text: string;
  searches: number;
}

/** GET /v1/search/popular — the public popular-searches payload. */
export interface PopularSearches {
  window_days: number;
  searches: PopularSearch[];
}

/**
 * GET /v1/search/popular — the most-run search texts of
 * the trailing window, from the identifier-free analytics
 * log. Powers the search box's popular list.
 */
export function getPopularSearches(): Promise<PopularSearches> {
  return call("/v1/search/popular");
}

/**
 * GET /v1/admin/home-preview?at= — the feed as of a
 * scheduled instant (admin only), for verifying a campaign
 * before it goes live. Omit `at` for "now".
 */
export function getAdminHomePreview(
  token: string,
  at?: string
): Promise<HomeFeed> {
  const qs = at ? `?at=${encodeURIComponent(at)}` : "";
  return call(`/v1/admin/home-preview${qs}`, { token });
}

export type MerchEventKind = "banner_click" | "collection_view";

/**
 * POST /v1/merch/events — the fire-and-forget engagement
 * beacon. Callers must never await it on a navigation path
 * and never surface its failure: a lost count is acceptable,
 * a blocked click is not.
 */
export function recordMerchEvent(kind: MerchEventKind, targetId: string): void {
  void call("/v1/merch/events", {
    method: "POST",
    body: { kind, target_id: targetId },
  }).catch(() => {
    // The beacon is best-effort by design (see the API's
    // migration 0047 note): counts inform, they never gate.
  });
}

/** One of the seller's products inside the live flash sale (Wave 52). */
export interface SellerFlashItem {
  product_id: string;
  product_title: string;
  store_slug: string;
  base_price_poisha: number;
  sale_price_poisha: number;
  stock_cap: number | null;
  sold_at_sale: number;
  cap_remaining: number | null;
}

/** The live flash sale from the seller's seat (Wave 52). */
export interface SellerFlashSale {
  sale_id: string;
  title: string;
  starts_at: string;
  ends_at: string;
  items: SellerFlashItem[];
}

/**
 * GET /v1/seller/flash-sale — the caller's products inside
 * the flash sale live right now, with the cap accounting.
 * Null when no sale is live.
 */
export function getSellerFlashSale(
  token: string
): Promise<SellerFlashSale | null> {
  return call("/v1/seller/flash-sale", { token });
}

/** One recent merchandising event (Wave 52), target title resolved. */
export interface MerchEventView {
  id: number;
  kind: MerchEventKind;
  target_id: string;
  target_title: string | null;
  created_at: string;
}

/** GET /v1/admin/merch-events — the most recent engagement events. */
export function listAdminMerchEvents(
  token: string,
  limit?: number
): Promise<MerchEventView[]> {
  const qs = limit ? `?limit=${limit}` : "";
  return call(`/v1/admin/merch-events${qs}`, { token });
}
