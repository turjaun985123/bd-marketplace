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
