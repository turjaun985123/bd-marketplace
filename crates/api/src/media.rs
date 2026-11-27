//! Product media storage + processing (Wave 51).
//!
//! The media system in one module, in three layers:
//!
//! - The [`MediaStore`] trait — the ONE storage contract:
//!   put / get / delete an object by key, and name its
//!   public URL. Two drivers implement it: [`LocalDiskStore`]
//!   (files under `MEDIA_DIR`, served by this API's own
//!   `/media/*` route — the default, zero external services)
//!   and [`S3Store`] (a minimal hand-rolled AWS SigV4 client
//!   for any S3-compatible endpoint — MinIO first of all).
//!   [`store_from_env`] is the ONE selection rule, mirroring
//!   the notify crate's `sender_from_env`: `MEDIA_STORE`
//!   picks the driver, `local` unless it says `s3`.
//! - The processing pipeline ([`process_upload`]): an
//!   upload's bytes are size-checked, sniffed and
//!   kind-resolved by the pure domain rules, decoded with
//!   the `image` crate, and re-encoded — the original with
//!   its metadata stripped (a re-encode keeps no EXIF), plus
//!   one WebP per derived width the domain planner allows
//!   (never an upscale). Object keys are deterministic from
//!   the image's base key: `<base>-orig` and
//!   `<base>-<width>.webp`, so views, deletes and the serve
//!   route never need a key table.
//! - The URL builder ([`sized_url`] / [`product_image_url`]):
//!   every product surface's `image_url` is computed from
//!   the stored base key + original width by these two
//!   functions and nothing else, so the local and S3 drivers
//!   can never disagree about what a view should carry.
//!
//! Honesty rules, same as everywhere in this codebase: the
//! S3 driver selected without its full credential set fails
//! every operation with a typed `media_not_configured`
//! error — it never pretends an upload landed. The local
//! driver is the default precisely because it needs no
//! credentials to be real.

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use domain::{
    best_derived_width, plan_derived_sizes, resolve_image_kind, validate_image_dimensions,
    validate_image_size, validate_media_key, ImageKind, GALLERY_IMAGE_WIDTH,
};
use hmac::{Hmac, Mac};
use image::{DynamicImage, ImageFormat};
use sha2::{Digest, Sha256};

use crate::ApiError;

type HmacSha256 = Hmac<Sha256>;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// A media storage failure. `code` is the stable machine
/// word the API error carries (`media_not_configured` for
/// the honest unconfigured-S3 case, `media_io` / `media_http`
/// for operational failures); `message` is the human detail.
/// Operational detail is logged, never leaked raw to clients
/// (see the `From` conversion).
#[derive(Debug)]
pub struct MediaError {
    pub code: &'static str,
    pub message: String,
}

impl MediaError {
    /// The S3 driver was selected but its credential set is
    /// incomplete — the typed refusal every operation of an
    /// unconfigured store returns.
    pub fn not_configured(message: impl Into<String>) -> Self {
        MediaError {
            code: "media_not_configured",
            message: message.into(),
        }
    }

    /// A local filesystem failure (context + the OS error).
    pub fn io(context: &str, err: std::io::Error) -> Self {
        MediaError {
            code: "media_io",
            message: format!("{context}: {err}"),
        }
    }

    /// An S3 HTTP failure (context + what the endpoint said).
    pub fn http(context: &str, detail: impl Into<String>) -> Self {
        MediaError {
            code: "media_http",
            message: format!("{context}: {}", detail.into()),
        }
    }
}

impl From<MediaError> for ApiError {
    fn from(err: MediaError) -> Self {
        if err.code == "media_not_configured" {
            return ApiError::service_unavailable(format!(
                "media_not_configured: {}",
                err.message
            ));
        }
        eprintln!("media storage error ({}): {}", err.code, err.message);
        ApiError::new(
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            format!("media storage error ({})", err.code),
        )
    }
}

// ---------------------------------------------------------------------------
// Object keys
// ---------------------------------------------------------------------------

/// The base key every object of one stored image derives
/// from: `products/<product_id>/<image_id>` for product
/// photos, `reviews/<review_id>/photo` for a review photo,
/// `stores/<store_id>/<kind>` for branding. Built by the
/// caller from ids it minted; always passes
/// `validate_media_key`.
pub fn original_key(base_key: &str) -> String {
    format!("{base_key}-orig")
}

/// The object key of one derived width of an image.
pub fn derived_key(base_key: &str, width: u32) -> String {
    format!("{base_key}-{width}.webp")
}

/// Every object key one stored image owns: the original
/// plus one per derived width planned from its original
/// width (a null/unknown width plans the full standard set
/// instead — deletes are idempotent, so naming a key that
/// was never written is harmless). THE list the delete
/// paths iterate, so an image's objects are removed by
/// exactly the rule that created them.
pub fn all_object_keys(base_key: &str, orig_width: Option<i32>) -> Vec<String> {
    let mut keys = vec![original_key(base_key)];
    let widths: Vec<u32> = match orig_width {
        Some(width) if width > 0 => plan_derived_sizes(width as u32, width as u32)
            .into_iter()
            .map(|size| size.width)
            .collect(),
        _ => domain::DERIVED_IMAGE_WIDTHS.to_vec(),
    };
    keys.extend(widths.into_iter().map(|width| derived_key(base_key, width)));
    keys
}

/// The public base every stored object's URL starts with:
/// `/media/` for the local driver (this API serves the
/// bytes itself), `<endpoint>/<bucket>/` for a fully
/// configured S3 driver. Resolved once from the same
/// environment [`store_from_env`] reads, so the URLs the
/// views carry always match the driver the uploads use.
/// An incompletely configured S3 selection falls back to
/// `/media/` — uploads through it fail honestly anyway, so
/// no view can carry a URL the store would have written.
pub fn public_base() -> &'static str {
    static BASE: OnceLock<String> = OnceLock::new();
    BASE.get_or_init(|| match s3_config_from_env() {
        Some(config) if media_store_kind() == "s3" => {
            format!(
                "{}/{}/",
                config.endpoint.trim_end_matches('/'),
                config.bucket
            )
        }
        _ => "/media/".to_string(),
    })
}

/// The URL of one stored object key under the active
/// driver's public base.
pub fn public_url_for_key(key: &str) -> String {
    format!("{}{key}", public_base())
}

/// The URL a surface should render for one stored image at
/// `target` display width: the best derived size the domain
/// planner allows for the original's width, or the original
/// itself when no derived size exists (small originals are
/// never upscaled). External images never come through
/// here — their URL is used verbatim.
pub fn sized_url(base_key: &str, orig_width: Option<i32>, target: u32) -> String {
    let width = orig_width.filter(|w| *w > 0).map(|w| w as u32);
    match width.and_then(|w| best_derived_width(w, target)) {
        Some(derived) => public_url_for_key(&derived_key(base_key, derived)),
        None => public_url_for_key(&original_key(base_key)),
    }
}

/// THE product/card `image_url` computation (Wave 51):
/// given the primary image's stored facts (its base key,
/// its external URL, its original width — the three
/// `primary_image_*` columns of `products::PRODUCT_COLUMNS`),
/// the gallery-width URL, or `None` when the product has no
/// primary image at all.
pub fn product_image_url(
    storage_key: Option<&str>,
    external_url: Option<&str>,
    orig_width: Option<i32>,
) -> Option<String> {
    if let Some(url) = external_url {
        return Some(url.to_string());
    }
    storage_key.map(|key| sized_url(key, orig_width, GALLERY_IMAGE_WIDTH))
}

// ---------------------------------------------------------------------------
// The store trait + selection
// ---------------------------------------------------------------------------

/// The media storage contract (Wave 51). Keys are the
/// validated path-like names of `domain::validate_media_key`;
/// every driver re-validates on the way in, so no caller —
/// and above all no URL — can name an object outside the
/// store. `get` of a missing key is `Ok(None)`, and `delete`
/// of a missing key is `Ok(())`: deletes are idempotent
/// because the database and the store are not transactional
/// and cleanup paths must be retryable.
#[async_trait]
pub trait MediaStore: Send + Sync {
    /// Store `bytes` under `key` with its content type,
    /// replacing any object already there.
    async fn put(&self, key: &str, bytes: &[u8], content_type: &str) -> Result<(), MediaError>;
    /// Read the object under `key`, or `None` when absent.
    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>, MediaError>;
    /// Remove the object under `key` (absent is success).
    async fn delete(&self, key: &str) -> Result<(), MediaError>;
    /// The public URL this driver serves `key` under.
    fn public_url(&self, key: &str) -> String;
    /// The driver's short name (`local` / `s3`) — the
    /// startup config summary prints it.
    fn driver(&self) -> &'static str;
    /// Whether THIS process serves the driver's bytes at
    /// `/media/*` (true only for the local driver; S3
    /// objects are served by the endpoint itself, and the
    /// route answers 404 so a stale local-style URL fails
    /// plainly instead of pretending).
    fn serves_locally(&self) -> bool {
        false
    }
    /// List every object key under `prefix` (Wave 51
    /// extension — the orphan reaper's enumeration). The
    /// local driver walks its directory; the S3 driver
    /// pages ListObjectsV2. Order is unspecified.
    async fn list(&self, prefix: &str) -> Result<Vec<String>, MediaError>;
}

/// The `MEDIA_STORE` selection value, normalised: `s3`
/// only when the variable says exactly that (any case);
/// every other value — unset included — is the local
/// driver, the one that needs no credentials to be real.
fn media_store_kind() -> &'static str {
    match std::env::var("MEDIA_STORE") {
        Ok(value) if value.trim().eq_ignore_ascii_case("s3") => "s3",
        _ => "local",
    }
}

/// A non-empty environment variable, or `None` (the media
/// module's own copy of the crate's env discipline:
/// whitespace-only counts as unset).
fn env_nonempty(name: &str) -> Option<String> {
    match std::env::var(name) {
        Ok(value) if !value.trim().is_empty() => Some(value.trim().to_string()),
        _ => None,
    }
}

/// THE media store selection (Wave 51), mirroring
/// `notify::sender_from_env`: built once into `AppState`,
/// shared by every upload, delete and serve path so they
/// can never disagree about where objects live.
pub fn store_from_env() -> Arc<dyn MediaStore> {
    match media_store_kind() {
        "s3" => Arc::new(S3Store::from_env()),
        _ => Arc::new(LocalDiskStore::from_env()),
    }
}

// ---------------------------------------------------------------------------
// Local disk driver
// ---------------------------------------------------------------------------

/// The local-disk media store (Wave 51): objects are files
/// under a root directory (`MEDIA_DIR`, default `./media`),
/// the key IS the relative path, and the API serves the
/// bytes itself at `/media/<key>`. The right driver for a
/// single-node deployment and for development; the trait
/// keeps it a config change away from S3.
pub struct LocalDiskStore {
    root: PathBuf,
}

impl LocalDiskStore {
    /// A store rooted at `root` (created lazily on the
    /// first put).
    pub fn new(root: PathBuf) -> Self {
        LocalDiskStore { root }
    }

    /// The store the environment names: `MEDIA_DIR`, or
    /// `./media` relative to the process working directory.
    pub fn from_env() -> Self {
        let root = env_nonempty("MEDIA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("./media"));
        LocalDiskStore::new(root)
    }

    /// The on-disk path for a key: the key validated by the
    /// domain rule (no traversal is representable), then
    /// joined segment by segment under the root.
    fn path_for(&self, key: &str) -> Result<PathBuf, MediaError> {
        validate_media_key(key).map_err(|message| MediaError {
            code: "media_io",
            message: format!("invalid media key '{key}': {message}"),
        })?;
        let mut path = self.root.clone();
        for segment in key.split('/') {
            path.push(segment);
        }
        Ok(path)
    }
}

#[async_trait]
impl MediaStore for LocalDiskStore {
    async fn put(&self, key: &str, bytes: &[u8], _content_type: &str) -> Result<(), MediaError> {
        let path = self.path_for(key)?;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| MediaError::io("create media directory", e))?;
        }
        tokio::fs::write(&path, bytes)
            .await
            .map_err(|e| MediaError::io("write media object", e))
    }

    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>, MediaError> {
        let path = self.path_for(key)?;
        match tokio::fs::read(&path).await {
            Ok(bytes) => Ok(Some(bytes)),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(MediaError::io("read media object", err)),
        }
    }

    async fn delete(&self, key: &str) -> Result<(), MediaError> {
        let path = self.path_for(key)?;
        match tokio::fs::remove_file(&path).await {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(MediaError::io("delete media object", err)),
        }
    }

    fn public_url(&self, key: &str) -> String {
        format!("/media/{key}")
    }

    fn driver(&self) -> &'static str {
        "local"
    }

    fn serves_locally(&self) -> bool {
        true
    }

    async fn list(&self, prefix: &str) -> Result<Vec<String>, MediaError> {
        // Walk the root iteratively (an explicit stack, so
        // depth is bounded by the filesystem, not the call
        // stack), collecting keys in store form ('/'-joined
        // relative paths) under the prefix.
        let mut keys = Vec::new();
        let mut stack = vec![self.root.clone()];
        while let Some(dir) = stack.pop() {
            let mut entries = match tokio::fs::read_dir(&dir).await {
                Ok(entries) => entries,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
                Err(err) => return Err(MediaError::io("list media directory", err)),
            };
            while let Some(entry) = entries
                .next_entry()
                .await
                .map_err(|e| MediaError::io("list media directory", e))?
            {
                let path = entry.path();
                let is_dir = entry
                    .file_type()
                    .await
                    .map_err(|e| MediaError::io("stat media entry", e))?
                    .is_dir();
                if is_dir {
                    stack.push(path);
                    continue;
                }
                if let Ok(relative) = path.strip_prefix(&self.root) {
                    let key = relative
                        .components()
                        .map(|part| part.as_os_str().to_string_lossy().into_owned())
                        .collect::<Vec<String>>()
                        .join("/");
                    if key.starts_with(prefix) {
                        keys.push(key);
                    }
                }
            }
        }
        Ok(keys)
    }
}

// ---------------------------------------------------------------------------
// S3-compatible driver (SigV4, hand-rolled)
// ---------------------------------------------------------------------------

/// The S3 driver's credential set (Wave 51): an endpoint
/// (MinIO or any S3-compatible service), a bucket, and the
/// access key pair. Path-style addressing is used
/// (`<endpoint>/<bucket>/<key>`) — the style MinIO serves
/// by default and S3 also accepts.
#[derive(Debug, Clone)]
pub struct S3Config {
    pub endpoint: String,
    pub bucket: String,
    pub access_key: String,
    pub secret_key: String,
    pub region: String,
    /// Seconds a presigned public GET URL stays valid
    /// (Wave 51 extension); 0 = plain public URLs (the
    /// bucket is world-readable), > 0 = private-bucket
    /// mode where every rendered URL is signed. SigV4
    /// caps presigns at 7 days (604_800 seconds).
    pub presign_seconds: i64,
}

/// The S3 configuration the environment names, or `None`
/// when ANY part of it is missing — a half-configured
/// driver is an unconfigured one (the platform's rule for
/// every credential set since Wave 5).
fn s3_config_from_env() -> Option<S3Config> {
    Some(S3Config {
        endpoint: env_nonempty("MEDIA_S3_ENDPOINT")?,
        bucket: env_nonempty("MEDIA_S3_BUCKET")?,
        access_key: env_nonempty("MEDIA_S3_ACCESS_KEY")?,
        secret_key: env_nonempty("MEDIA_S3_SECRET_KEY")?,
        region: env_nonempty("MEDIA_S3_REGION").unwrap_or_else(|| "us-east-1".to_string()),
        presign_seconds: env_nonempty("MEDIA_S3_PRESIGN_SECONDS")
            .and_then(|raw| raw.parse::<i64>().ok())
            .map(|seconds| seconds.clamp(0, 604_800))
            .unwrap_or(0),
    })
}

/// The S3-compatible media store (Wave 51): PUT / GET /
/// DELETE signed with AWS Signature Version 4, implemented
/// directly over reqwest — the three operations are small
/// and the signing algorithm is a page of HMACs; a full SDK
/// dependency would be heavier than the feature. Selected
/// but unconfigured, every operation fails with
/// `media_not_configured` (see [`MediaError::not_configured`]).
pub struct S3Store {
    config: Option<S3Config>,
    client: reqwest::Client,
}

impl S3Store {
    /// A store over an explicit configuration (or `None`
    /// for the honestly-unconfigured one).
    pub fn new(config: Option<S3Config>) -> Self {
        S3Store {
            config,
            client: reqwest::Client::new(),
        }
    }

    /// The store the environment names.
    pub fn from_env() -> Self {
        S3Store::new(s3_config_from_env())
    }

    /// The configuration, or the typed unconfigured error —
    /// the first thing every operation resolves.
    fn require_config(&self) -> Result<&S3Config, MediaError> {
        self.config.as_ref().ok_or_else(|| {
            MediaError::not_configured(
                "MEDIA_STORE=s3 but the MEDIA_S3_* credential set is incomplete \
                 (endpoint, bucket, access key and secret key are all required)",
            )
        })
    }

    /// The object's path-style URL under the endpoint.
    fn object_url(&self, config: &S3Config, key: &str) -> String {
        format!(
            "{}/{}/{}",
            config.endpoint.trim_end_matches('/'),
            config.bucket,
            key
        )
    }

    /// The endpoint's host header value (`host[:port]`) —
    /// parsed from the configured endpoint by string rules:
    /// the endpoint is `scheme://authority` with no path
    /// (the configuration contract).
    fn host_of(config: &S3Config) -> String {
        let endpoint = config.endpoint.trim_end_matches('/');
        match endpoint.split_once("://") {
            Some((_, authority)) => authority.to_string(),
            None => endpoint.to_string(),
        }
    }

    /// One HMAC-SHA256, hex-free (raw bytes) — the SigV4
    /// building block.
    fn hmac(key: &[u8], data: &[u8]) -> Vec<u8> {
        let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
        mac.update(data);
        mac.finalize().into_bytes().to_vec()
    }

    /// The SigV4 signing key for a date + region:
    /// HMAC chains of ("AWS4" + secret, date, region,
    /// "s3", "aws4_request").
    fn signing_key(config: &S3Config, date: &str) -> Vec<u8> {
        let k_date = Self::hmac(
            format!("AWS4{}", config.secret_key).as_bytes(),
            date.as_bytes(),
        );
        let k_region = Self::hmac(&k_date, config.region.as_bytes());
        let k_service = Self::hmac(&k_region, b"s3");
        Self::hmac(&k_service, b"aws4_request")
    }

    /// The `Authorization` header for one OBJECT request
    /// (SigV4) — delegates to [`S3Store::authorization_for`]
    /// with the path-style object URI and no query string.
    /// The clock is a parameter so tests can pin the output.
    fn authorization(
        &self,
        config: &S3Config,
        method: &str,
        key: &str,
        payload_hash: &str,
        now: DateTime<Utc>,
    ) -> String {
        let canonical_uri = format!("/{}/{}", config.bucket, key);
        self.authorization_for(config, method, &canonical_uri, "", payload_hash, now)
    }

    /// The `Authorization` header for one request (SigV4):
    /// the canonical request is method + URI + canonical
    /// query + the three signed headers (host,
    /// content-sha256, amz-date) + the payload hash; the
    /// signature is the HMAC of the string-to-sign under
    /// the date's key. The clock is a parameter so tests
    /// can pin the output.
    fn authorization_for(
        &self,
        config: &S3Config,
        method: &str,
        canonical_uri: &str,
        canonical_query: &str,
        payload_hash: &str,
        now: DateTime<Utc>,
    ) -> String {
        let date = now.format("%Y%m%d").to_string();
        let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
        let host = Self::host_of(config);
        let canonical_headers =
            format!("host:{host}\nx-amz-content-sha256:{payload_hash}\nx-amz-date:{amz_date}\n");
        let signed_headers = "host;x-amz-content-sha256;x-amz-date";
        let canonical_request = format!(
            "{method}\n{canonical_uri}\n{canonical_query}\n{canonical_headers}{signed_headers}\n{payload_hash}"
        );
        let scope = format!("{date}/{}/s3/aws4_request", config.region);
        let canonical_hash = hex::encode(Sha256::digest(canonical_request.as_bytes()));
        let string_to_sign =
            format!("AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{canonical_hash}");
        let signature = hex::encode(Self::hmac(
            &Self::signing_key(config, &date),
            string_to_sign.as_bytes(),
        ));
        format!(
            "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, \
             Signature={signature}",
            config.access_key
        )
    }

    /// A presigned GET URL for one object (Wave 51
    /// extension): query-string SigV4 — the canonical
    /// request carries the `X-Amz-*` parameters, the
    /// `host` header alone is signed, and the payload is
    /// `UNSIGNED-PAYLOAD` (the signer has no body). The
    /// clock is a parameter so tests can pin the output.
    /// Used for public URLs in private-bucket mode
    /// (`presign_seconds > 0`).
    fn presigned_get_url(&self, config: &S3Config, key: &str, now: DateTime<Utc>) -> String {
        let date = now.format("%Y%m%d").to_string();
        let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
        let scope = format!("{date}/{}/s3/aws4_request", config.region);
        let credential = format!("{}/{}", config.access_key, scope);
        // Canonical query parameters are name-sorted:
        // Algorithm < Credential < Date < Expires <
        // SignedHeaders.
        let canonical_query = format!(
            "X-Amz-Algorithm=AWS4-HMAC-SHA256\
             &X-Amz-Credential={}\
             &X-Amz-Date={amz_date}\
             &X-Amz-Expires={}\
             &X-Amz-SignedHeaders=host",
            sigv4_query_escape(&credential),
            config.presign_seconds,
        );
        let host = Self::host_of(config);
        let canonical_uri = format!("/{}/{}", config.bucket, key);
        let canonical_request = format!(
            "GET\n{canonical_uri}\n{canonical_query}\nhost:{host}\n\nhost\nUNSIGNED-PAYLOAD"
        );
        let canonical_hash = hex::encode(Sha256::digest(canonical_request.as_bytes()));
        let string_to_sign = format!("AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{canonical_hash}");
        let signature = hex::encode(Self::hmac(
            &Self::signing_key(config, &date),
            string_to_sign.as_bytes(),
        ));
        format!(
            "{}?{canonical_query}&X-Amz-Signature={signature}",
            self.object_url(config, key)
        )
    }

    /// Send one signed request and return the response,
    /// mapping transport failures to the typed HTTP error.
    async fn send_signed(
        &self,
        config: &S3Config,
        method: reqwest::Method,
        key: &str,
        body: Option<(Vec<u8>, String)>,
    ) -> Result<reqwest::Response, MediaError> {
        let payload_hash = match &body {
            Some((bytes, _)) => hex::encode(Sha256::digest(bytes)),
            None => hex::encode(Sha256::digest([])),
        };
        let now = Utc::now();
        let authorization = self.authorization(
            config,
            method.as_str(),
            key,
            &payload_hash,
            now,
        );
        let mut request = self
            .client
            .request(method, self.object_url(config, key))
            .header("Authorization", authorization)
            .header("x-amz-date", now.format("%Y%m%dT%H%M%SZ").to_string())
            .header("x-amz-content-sha256", payload_hash);
        if let Some((bytes, content_type)) = body {
            request = request.header("Content-Type", content_type).body(bytes);
        }
        request
            .send()
            .await
            .map_err(|e| MediaError::http("S3 request", e.to_string()))
    }

    /// One page of a ListObjectsV2 call (Wave 51
    /// extension): GET the bucket with `list-type=2`, the
    /// prefix and — when continuing — the token, all
    /// signed with the query in the canonical request
    /// (SigV4 signs the query; leaving it out would 403).
    /// Returns the page's keys plus the continuation token
    /// when the listing is truncated.
    async fn list_page(
        &self,
        config: &S3Config,
        prefix: &str,
        continuation: Option<&str>,
    ) -> Result<(Vec<String>, Option<String>), MediaError> {
        // Canonical query parameters are name-sorted:
        // continuation-token < list-type < prefix.
        let mut params: Vec<(String, String)> = Vec::new();
        if let Some(token) = continuation {
            params.push(("continuation-token".to_string(), token.to_string()));
        }
        params.push(("list-type".to_string(), "2".to_string()));
        params.push(("prefix".to_string(), prefix.to_string()));
        let canonical_query = params
            .iter()
            .map(|(name, value)| format!("{name}={}", sigv4_query_escape(value)))
            .collect::<Vec<String>>()
            .join("&");
        let payload_hash = hex::encode(Sha256::digest([]));
        let now = Utc::now();
        let authorization = self.authorization_for(
            config,
            "GET",
            &format!("/{}", config.bucket),
            &canonical_query,
            &payload_hash,
            now,
        );
        let url = format!(
            "{}/{}?{}",
            config.endpoint.trim_end_matches('/'),
            config.bucket,
            canonical_query
        );
        let response = self
            .client
            .get(url)
            .header("Authorization", authorization)
            .header("x-amz-date", now.format("%Y%m%dT%H%M%SZ").to_string())
            .header("x-amz-content-sha256", payload_hash)
            .send()
            .await
            .map_err(|e| MediaError::http("S3 list request", e.to_string()))?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(MediaError::http(
                "S3 list",
                format!("status {status}: {body}"),
            ));
        }
        let xml = response
            .text()
            .await
            .map_err(|e| MediaError::http("S3 list body", e.to_string()))?;
        Ok(parse_list_page(&xml))
    }
}

/// Percent-encode one SigV4 query value: unreserved
/// characters (A-Z a-z 0-9 - _ . ~) pass through, every
/// other byte becomes %XX (uppercase hex).
fn sigv4_query_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// Undo the XML escaping S3 applies to listed keys (and
/// continuation tokens): the five predefined entities.
fn xml_unescape(value: &str) -> String {
    value
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// The text of every `<tag>…</tag>` element in an XML
/// body, in document order — a scanner, not a parser: the
/// ListObjectsV2 shape is flat and stable, and pulling a
/// full XML crate in for three element names would be
/// heavier than the feature.
fn xml_elements(xml: &str, tag: &str) -> Vec<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let mut values = Vec::new();
    let mut rest = xml;
    while let Some(start) = rest.find(&open) {
        let after = &rest[start + open.len()..];
        match after.find(&close) {
            Some(end) => {
                values.push(xml_unescape(&after[..end]));
                rest = &after[end + close.len()..];
            }
            None => break,
        }
    }
    values
}

/// Parse one ListObjectsV2 page: its keys, plus the
/// continuation token when the result is truncated.
fn parse_list_page(xml: &str) -> (Vec<String>, Option<String>) {
    let keys = xml_elements(xml, "Key");
    let truncated = xml_elements(xml, "IsTruncated")
        .first()
        .is_some_and(|value| value == "true");
    let next = if truncated {
        xml_elements(xml, "NextContinuationToken").into_iter().next()
    } else {
        None
    };
    (keys, next)
}

#[async_trait]
impl MediaStore for S3Store {
    async fn put(&self, key: &str, bytes: &[u8], content_type: &str) -> Result<(), MediaError> {
        let config = self.require_config()?;
        validate_media_key(key).map_err(|message| MediaError {
            code: "media_io",
            message: format!("invalid media key '{key}': {message}"),
        })?;
        let response = self
            .send_signed(
                config,
                reqwest::Method::PUT,
                key,
                Some((bytes.to_vec(), content_type.to_string())),
            )
            .await?;
        let status = response.status();
        if status.is_success() {
            Ok(())
        } else {
            let body = response.text().await.unwrap_or_default();
            Err(MediaError::http(
                "S3 put",
                format!("status {status}: {body}"),
            ))
        }
    }

    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>, MediaError> {
        let config = self.require_config()?;
        validate_media_key(key).map_err(|message| MediaError {
            code: "media_io",
            message: format!("invalid media key '{key}': {message}"),
        })?;
        let response = self
            .send_signed(config, reqwest::Method::GET, key, None)
            .await?;
        let status = response.status();
        if status.is_success() {
            let bytes = response
                .bytes()
                .await
                .map_err(|e| MediaError::http("S3 get body", e.to_string()))?;
            Ok(Some(bytes.to_vec()))
        } else if status == reqwest::StatusCode::NOT_FOUND {
            Ok(None)
        } else {
            let body = response.text().await.unwrap_or_default();
            Err(MediaError::http(
                "S3 get",
                format!("status {status}: {body}"),
            ))
        }
    }

    async fn delete(&self, key: &str) -> Result<(), MediaError> {
        let config = self.require_config()?;
        validate_media_key(key).map_err(|message| MediaError {
            code: "media_io",
            message: format!("invalid media key '{key}': {message}"),
        })?;
        let response = self
            .send_signed(config, reqwest::Method::DELETE, key, None)
            .await?;
        let status = response.status();
        // S3 delete is idempotent by design (204 even for a
        // missing key); a 404 is equally a success here.
        if status.is_success() || status == reqwest::StatusCode::NOT_FOUND {
            Ok(())
        } else {
            let body = response.text().await.unwrap_or_default();
            Err(MediaError::http(
                "S3 delete",
                format!("status {status}: {body}"),
            ))
        }
    }

    fn public_url(&self, key: &str) -> String {
        match &self.config {
            // Private-bucket mode: every rendered URL is
            // a fresh presigned GET (Wave 51 extension).
            Some(config) if config.presign_seconds > 0 => {
                self.presigned_get_url(config, key, Utc::now())
            }
            Some(config) => self.object_url(config, key),
            None => format!("/media/{key}"),
        }
    }

    fn driver(&self) -> &'static str {
        "s3"
    }

    async fn list(&self, prefix: &str) -> Result<Vec<String>, MediaError> {
        let config = self.require_config()?;
        // An empty prefix lists the whole bucket (the
        // reaper's call); a non-empty one must be a valid
        // key prefix, like every other key the store takes.
        if !prefix.is_empty() {
            validate_media_key(prefix.trim_end_matches('/')).map_err(|message| {
                MediaError {
                    code: "media_io",
                    message: format!("invalid media prefix '{prefix}': {message}"),
                }
            })?;
        }
        // Page through ListObjectsV2 until the listing is
        // complete; the page cap keeps a pathological
        // bucket from looping the reaper forever (100
        // pages × 1000 keys is far past any real store).
        let mut keys = Vec::new();
        let mut continuation: Option<String> = None;
        for _ in 0..100 {
            let (page, next) = self.list_page(config, prefix, continuation.as_deref()).await?;
            keys.extend(page);
            match next {
                Some(token) => continuation = Some(token),
                None => return Ok(keys),
            }
        }
        Ok(keys)
    }
}

// ---------------------------------------------------------------------------
// Processing
// ---------------------------------------------------------------------------

/// One object the pipeline produced: its full key, its
/// bytes, and the content type to store and serve it with.
#[derive(Debug)]
pub struct ProcessedObject {
    pub key: String,
    pub bytes: Vec<u8>,
    pub content_type: &'static str,
}

/// A fully processed upload (Wave 51): the original's
/// facts (for the `product_images` row) and every object to
/// write to the store — the re-encoded original first, then
/// the derived WebPs in plan order.
#[derive(Debug)]
pub struct ProcessedImage {
    pub kind: ImageKind,
    pub width: u32,
    pub height: u32,
    /// Byte length of the ORIGINAL upload (what the seller
    /// sent), stored on the row as `size_bytes`.
    pub size_bytes: i64,
    /// SHA-256 (hex) of the original upload bytes — the
    /// duplicate guard's fingerprint (migration 0042).
    pub content_hash: String,
    pub objects: Vec<ProcessedObject>,
}

/// The EXIF orientation tag (0x0112) of a JPEG upload, or
/// `None` when the file carries none (Wave 51 extension).
/// Phone cameras store most photos rotated and record the
/// correction in EXIF; because the pipeline STRIPS
/// metadata on re-encode, an unapplied orientation would
/// leave every portrait photo sideways forever — so the
/// orientation is read here (a small hand parser over the
/// APP1 segment: TIFF header, IFD0, one SHORT entry) and
/// baked into the pixels by [`apply_orientation`].
pub fn jpeg_exif_orientation(bytes: &[u8]) -> Option<u8> {
    // JPEG: SOI, then length-prefixed segments.
    if bytes.len() < 4 || bytes[0] != 0xFF || bytes[1] != 0xD8 {
        return None;
    }
    let mut offset = 2;
    while offset + 4 <= bytes.len() {
        if bytes[offset] != 0xFF {
            return None;
        }
        let marker = bytes[offset + 1];
        // Standalone markers (RSTn, TEM) carry no length;
        // SOS starts the scan data — EXIF always precedes it.
        if marker == 0xDA || marker == 0xD9 {
            return None;
        }
        if (0xD0..=0xD7).contains(&marker) || marker == 0x01 {
            offset += 2;
            continue;
        }
        let length = u16::from_be_bytes([bytes[offset + 2], bytes[offset + 3]]) as usize;
        if length < 2 || offset + 2 + length > bytes.len() {
            return None;
        }
        let payload = &bytes[offset + 4..offset + 2 + length];
        if marker == 0xE1 && payload.starts_with(b"Exif\0\0") {
            return parse_exif_orientation(&payload[6..]);
        }
        offset += 2 + length;
    }
    None
}

/// Read orientation from a TIFF block (the bytes after
/// `Exif\0\0`): byte order, the 42 magic, IFD0's offset,
/// then the 0x0112 entry's SHORT value.
fn parse_exif_orientation(tiff: &[u8]) -> Option<u8> {
    if tiff.len() < 8 {
        return None;
    }
    let little_endian = match &tiff[0..2] {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let u16_at = |at: usize| -> Option<u16> {
        let raw: [u8; 2] = tiff.get(at..at + 2)?.try_into().ok()?;
        Some(if little_endian {
            u16::from_le_bytes(raw)
        } else {
            u16::from_be_bytes(raw)
        })
    };
    let u32_at = |at: usize| -> Option<u32> {
        let raw: [u8; 4] = tiff.get(at..at + 4)?.try_into().ok()?;
        Some(if little_endian {
            u32::from_le_bytes(raw)
        } else {
            u32::from_be_bytes(raw)
        })
    };
    if u16_at(2)? != 42 {
        return None;
    }
    let ifd_offset = u32_at(4)? as usize;
    let entries = u16_at(ifd_offset)? as usize;
    for index in 0..entries {
        let entry = ifd_offset + 2 + index * 12;
        if u16_at(entry)? == 0x0112 {
            // Type SHORT (3), count 1: the value sits in
            // the first two bytes of the 4-byte value field.
            if u16_at(entry + 2)? != 3 || u32_at(entry + 4)? != 1 {
                return None;
            }
            let value = u16_at(entry + 8)?;
            return u8::try_from(value).ok().filter(|v| (1..=8).contains(v));
        }
    }
    None
}

/// Bake an EXIF orientation into the pixels (Wave 51
/// extension): the stored→display correction table,
/// applied with the image crate's rotate/flip primitives
/// (its `rotate90` is 90° clockwise; the table below is
/// written in those terms).
fn apply_orientation(image: DynamicImage, orientation: u8) -> DynamicImage {
    match orientation {
        2 => image.fliph(),
        3 => image.rotate180(),
        4 => image.flipv(),
        5 => image.fliph().rotate270(),
        6 => image.rotate90(),
        7 => image.fliph().rotate90(),
        8 => image.rotate270(),
        _ => image,
    }
}

/// Encode one image in one format into memory.
fn encode(image: &DynamicImage, format: ImageFormat) -> Result<Vec<u8>, ApiError> {
    let mut buffer = std::io::Cursor::new(Vec::new());
    image
        .write_to(&mut buffer, format)
        .map_err(|e| ApiError::bad_request(format!("the image could not be re-encoded: {e}")))?;
    Ok(buffer.into_inner())
}

/// Process one upload (Wave 51) — THE pipeline every image
/// write path (product upload, review photo, branding)
/// shares:
///
/// 1. The pure domain rules gate the bytes: size, then the
///    kind resolution (sniff + declared-type agreement).
/// 2. The `image` crate decodes; an undecodable body is a
///    bilingual 400 (the sniff passed but the file is
///    truncated or corrupt — the seller can fix that, the
///    platform cannot).
/// 3. Dimensions are validated, then the objects are
///    built: the original re-encoded in its own format
///    (metadata stripped by the re-encode), plus one WebP
///    per planned derived width, resized with Lanczos3.
///
/// `base_key` is the image's deterministic key base (see
/// [`original_key`]); the returned objects carry full keys.
pub fn process_upload(
    base_key: &str,
    bytes: &[u8],
    declared_content_type: Option<&str>,
) -> Result<ProcessedImage, ApiError> {
    validate_media_key(base_key).map_err(|m| ApiError::bad_request(m))?;
    validate_image_size(bytes.len()).map_err(|m| ApiError::bad_request(m))?;
    let kind =
        resolve_image_kind(declared_content_type, bytes).map_err(|m| ApiError::bad_request(m))?;
    let decoded = image::load_from_memory(bytes).map_err(|_| {
        ApiError::bad_request(
            "the image could not be decoded — the file may be truncated or corrupt — \
             ছবিটি পড়া যায়নি, ফাইলটি অসম্পূর্ণ বা নষ্ট হতে পারে",
        )
    })?;
    // Bake a JPEG's EXIF orientation into the pixels
    // before anything measures or re-encodes: the
    // re-encode strips the tag, so this is the only
    // moment the correction can happen.
    let image = if kind == ImageKind::Jpeg {
        match jpeg_exif_orientation(bytes) {
            Some(orientation) => apply_orientation(decoded, orientation),
            None => decoded,
        }
    } else {
        decoded
    };
    let (width, height) = {
        use image::GenericImageView;
        image.dimensions()
    };
    validate_image_dimensions(width, height).map_err(|m| ApiError::bad_request(m))?;

    let format = match kind {
        ImageKind::Jpeg => ImageFormat::Jpeg,
        ImageKind::Png => ImageFormat::Png,
        ImageKind::Webp => ImageFormat::WebP,
    };
    let mut objects = vec![ProcessedObject {
        key: original_key(base_key),
        bytes: encode(&image, format)?,
        content_type: kind.content_type(),
    }];
    for size in plan_derived_sizes(width, height) {
        let resized = image.resize_exact(
            size.width,
            size.height,
            image::imageops::FilterType::Lanczos3,
        );
        objects.push(ProcessedObject {
            key: derived_key(base_key, size.width),
            bytes: encode(&resized, ImageFormat::WebP)?,
            content_type: ImageKind::Webp.content_type(),
        });
    }

    Ok(ProcessedImage {
        kind,
        width,
        height,
        size_bytes: bytes.len() as i64,
        content_hash: hex::encode(Sha256::digest(bytes)),
        objects,
    })
}

/// Write every object of a processed upload to the store,
/// in order. A failure part-way leaves earlier objects
/// written — the caller writes its database row only after
/// this succeeds, and an orphaned object with no row is
/// inert (nothing references its key); the reverse order
/// (row first) could leave a row pointing at nothing,
/// which is the failure the storefront would see.
pub async fn store_processed(
    store: &dyn MediaStore,
    processed: &ProcessedImage,
) -> Result<(), ApiError> {
    for object in &processed.objects {
        store
            .put(&object.key, &object.bytes, object.content_type)
            .await?;
    }
    Ok(())
}

/// Delete every object one stored image owns (the original
/// plus its planned derived sizes — see [`all_object_keys`]).
/// Best-effort per object: the caller has usually already
/// committed to the delete, and one stuck object must not
/// veto the rest of the cleanup.
pub async fn delete_stored_image(
    store: &dyn MediaStore,
    base_key: &str,
    orig_width: Option<i32>,
) {
    for key in all_object_keys(base_key, orig_width) {
        if let Err(err) = store.delete(&key).await {
            eprintln!("media delete of {key} failed ({}): {}", err.code, err.message);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn test_s3_config() -> S3Config {
        S3Config {
            endpoint: "http://minio.local:9000".to_string(),
            bucket: "marketplace-media".to_string(),
            access_key: "test-access-key".to_string(),
            secret_key: "test-secret-key".to_string(),
            region: "us-east-1".to_string(),
            presign_seconds: 0,
        }
    }

    #[test]
    fn object_key_shapes() {
        assert_eq!(original_key("products/p1/i1"), "products/p1/i1-orig");
        assert_eq!(derived_key("products/p1/i1", 600), "products/p1/i1-600.webp");
        // A 700px original owns orig + 600 + 300.
        assert_eq!(
            all_object_keys("products/p1/i1", Some(700)),
            vec![
                "products/p1/i1-orig".to_string(),
                "products/p1/i1-600.webp".to_string(),
                "products/p1/i1-300.webp".to_string(),
            ]
        );
        // An unknown width plans the full standard set.
        assert_eq!(all_object_keys("products/p1/i1", None).len(), 4);
        // A small original owns only its original.
        assert_eq!(all_object_keys("products/p1/i1", Some(250)).len(), 1);
    }

    #[test]
    fn unconfigured_s3_store_public_url_and_driver() {
        let store = S3Store::new(None);
        assert_eq!(store.driver(), "s3");
        assert!(!store.serves_locally());
        // The fallback URL shape is the local one; it is
        // never handed out in practice because every
        // operation of this store fails first.
        assert_eq!(store.public_url("products/p/i-orig"), "/media/products/p/i-orig");
    }

    #[tokio::test]
    async fn unconfigured_s3_store_fails_honestly() {
        let store = S3Store::new(None);
        let err = store
            .put("products/p/i-orig", b"bytes", "image/png")
            .await
            .expect_err("unconfigured put must fail");
        assert_eq!(err.code, "media_not_configured");
        let err = store
            .get("products/p/i-orig")
            .await
            .expect_err("unconfigured get must fail");
        assert_eq!(err.code, "media_not_configured");
        let err = store
            .delete("products/p/i-orig")
            .await
            .expect_err("unconfigured delete must fail");
        assert_eq!(err.code, "media_not_configured");
    }

    #[tokio::test]
    async fn local_store_round_trip_in_a_temp_dir() {
        let root = std::env::temp_dir().join(format!(
            "bdm-media-test-{}",
            Uuid::new_v4().simple()
        ));
        let store = LocalDiskStore::new(root.clone());
        assert_eq!(store.driver(), "local");
        assert!(store.serves_locally());
        assert_eq!(
            store.public_url("products/p/i-600.webp"),
            "/media/products/p/i-600.webp"
        );

        // Missing keys read as None and delete as success.
        assert_eq!(store.get("products/p/i-orig").await.expect("get"), None);
        store.delete("products/p/i-orig").await.expect("delete");

        // Put creates the sharded directories; get reads
        // the exact bytes back; a second put replaces.
        store
            .put("products/p/i-orig", b"first", "image/png")
            .await
            .expect("put");
        assert_eq!(
            store.get("products/p/i-orig").await.expect("get"),
            Some(b"first".to_vec())
        );
        store
            .put("products/p/i-orig", b"second", "image/png")
            .await
            .expect("put replaces");
        assert_eq!(
            store.get("products/p/i-orig").await.expect("get"),
            Some(b"second".to_vec())
        );
        store.delete("products/p/i-orig").await.expect("delete");
        assert_eq!(store.get("products/p/i-orig").await.expect("get"), None);

        // Traversal keys are refused by every operation.
        assert!(store.put("../escape", b"x", "image/png").await.is_err());
        assert!(store.get("../escape").await.is_err());
        assert!(store.delete("../escape").await.is_err());

        tokio::fs::remove_dir_all(&root).await.expect("cleanup");
    }

    #[test]
    fn sigv4_authorization_shape_with_a_pinned_clock() {
        let store = S3Store::new(Some(test_s3_config()));
        let config = test_s3_config();
        let now = DateTime::parse_from_rfc3339("2026-11-27T10:15:30Z")
            .expect("parse")
            .with_timezone(&Utc);
        let payload_hash = hex::encode(Sha256::digest(b"hello"));
        let header = store.authorization(
            &config,
            "PUT",
            "products/p/i-orig",
            &payload_hash,
            now,
        );
        // The credential scope names the pinned date, the
        // region and the service; the signed-header list is
        // the documented trio; the signature is 64 hex.
        assert!(
            header.starts_with(
                "AWS4-HMAC-SHA256 Credential=test-access-key/20261127/us-east-1/s3/aws4_request, \
                 SignedHeaders=host;x-amz-content-sha256;x-amz-date, Signature="
            ),
            "unexpected authorization header: {header}"
        );
        let signature = header.rsplit("Signature=").next().expect("signature");
        assert_eq!(signature.len(), 64);
        assert!(signature.chars().all(|c| c.is_ascii_hexdigit()));
        // Signing is deterministic: same inputs, same header.
        let again = store.authorization(&config, "PUT", "products/p/i-orig", &payload_hash, now);
        assert_eq!(header, again);
        // A different payload signs differently.
        let other_hash = hex::encode(Sha256::digest(b"goodbye"));
        let other = store.authorization(&config, "PUT", "products/p/i-orig", &other_hash, now);
        assert_ne!(header, other);
    }

    #[test]
    fn presigned_get_url_shape_with_a_pinned_clock() {
        let mut config = test_s3_config();
        config.presign_seconds = 3600;
        let store = S3Store::new(Some(config.clone()));
        let now = DateTime::parse_from_rfc3339("2026-11-27T10:15:30Z")
            .expect("parse")
            .with_timezone(&Utc);
        let url = store.presigned_get_url(&config, "products/p/i-600.webp", now);
        // Path-style object URL, then the canonical query
        // in name order, then the signature.
        assert!(
            url.starts_with(
                "http://minio.local:9000/marketplace-media/products/p/i-600.webp\
                 ?X-Amz-Algorithm=AWS4-HMAC-SHA256\
                 &X-Amz-Credential=test-access-key%2F20261127%2Fus-east-1%2Fs3%2Faws4_request\
                 &X-Amz-Date=20261127T101530Z\
                 &X-Amz-Expires=3600\
                 &X-Amz-SignedHeaders=host\
                 &X-Amz-Signature="
            ),
            "unexpected presigned url: {url}"
        );
        let signature = url.rsplit("X-Amz-Signature=").next().expect("signature");
        assert_eq!(signature.len(), 64);
        assert!(signature.chars().all(|c| c.is_ascii_hexdigit()));
        // Deterministic at a pinned clock; a different
        // key signs differently.
        assert_eq!(
            url,
            store.presigned_get_url(&config, "products/p/i-600.webp", now)
        );
        assert_ne!(
            url,
            store.presigned_get_url(&config, "products/p/i-300.webp", now)
        );
        // public_url presigns only in private-bucket mode.
        let private = S3Store::new(Some(config));
        assert!(private
            .public_url("products/p/i-600.webp")
            .contains("X-Amz-Signature="));
        let public = S3Store::new(Some(test_s3_config()));
        assert_eq!(
            public.public_url("products/p/i-600.webp"),
            "http://minio.local:9000/marketplace-media/products/p/i-600.webp"
        );
    }

    #[test]
    fn s3_host_and_object_url_parsing() {
        let config = test_s3_config();
        assert_eq!(S3Store::host_of(&config), "minio.local:9000");
        let store = S3Store::new(Some(config.clone()));
        assert_eq!(
            store.object_url(&config, "products/p/i-orig"),
            "http://minio.local:9000/marketplace-media/products/p/i-orig"
        );
        // A trailing slash on the endpoint is tolerated.
        let mut sloppy = config.clone();
        sloppy.endpoint = "https://s3.example.com/".to_string();
        assert_eq!(S3Store::host_of(&sloppy), "s3.example.com");
        assert_eq!(
            store.object_url(&sloppy, "a/b"),
            "https://s3.example.com/marketplace-media/a/b"
        );
    }

    /// Encode a solid-colour PNG of the given size with the
    /// image crate itself — a real decodable upload body
    /// without embedding a binary fixture here.
    fn png_bytes(width: u32, height: u32) -> Vec<u8> {
        let image = DynamicImage::new_rgb8(width, height);
        let mut buffer = std::io::Cursor::new(Vec::new());
        image
            .write_to(&mut buffer, ImageFormat::Png)
            .expect("encode png");
        buffer.into_inner()
    }

    #[test]
    fn process_upload_derives_planned_sizes_only() {
        // 700x60 PNG: derived 600 + 300 (never 1200), all
        // WebP; the original keeps PNG.
        let processed = process_upload("products/p/i", &png_bytes(700, 60), Some("image/png"))
            .expect("process");
        assert_eq!(processed.kind, ImageKind::Png);
        assert_eq!((processed.width, processed.height), (700, 60));
        let keys: Vec<&str> = processed.objects.iter().map(|o| o.key.as_str()).collect();
        assert_eq!(
            keys,
            vec!["products/p/i-orig", "products/p/i-600.webp", "products/p/i-300.webp"]
        );
        assert_eq!(processed.objects[0].content_type, "image/png");
        assert_eq!(processed.objects[1].content_type, "image/webp");
        // Every derived object really is a WebP (RIFF/WEBP).
        for object in &processed.objects[1..] {
            assert_eq!(domain::sniff_image_kind(&object.bytes), Some(ImageKind::Webp));
        }
    }

    #[test]
    fn process_upload_small_image_plans_no_derived() {
        let processed = process_upload("products/p/i", &png_bytes(120, 90), None)
            .expect("process");
        assert_eq!(processed.objects.len(), 1);
        assert_eq!(processed.objects[0].key, "products/p/i-orig");
    }

    #[test]
    fn process_upload_rejects_non_images_and_mismatches() {
        // Not an image at all.
        assert!(process_upload("products/p/i", b"definitely not an image", None).is_err());
        // A PNG declared as JPEG is a mismatch refusal.
        assert!(
            process_upload("products/p/i", &png_bytes(10, 10), Some("image/jpeg")).is_err()
        );
        // An empty body is refused by the size rule.
        assert!(process_upload("products/p/i", &[], None).is_err());
        // A traversal base key is refused before any work.
        assert!(process_upload("../escape", &png_bytes(10, 10), None).is_err());
    }

    /// Build a JPEG carrying an EXIF orientation tag: an
    /// image-crate JPEG with a hand-built APP1 segment
    /// spliced in right after the SOI marker (a minimal
    /// little-endian TIFF with the single 0x0112 SHORT
    /// entry) — exactly the shape a phone camera writes.
    fn jpeg_with_exif_orientation(width: u32, height: u32, orientation: u8) -> Vec<u8> {
        let image = DynamicImage::new_rgb8(width, height);
        let mut buffer = std::io::Cursor::new(Vec::new());
        image
            .write_to(&mut buffer, ImageFormat::Jpeg)
            .expect("encode jpeg");
        let jpeg = buffer.into_inner();
        let mut tiff = Vec::new();
        tiff.extend_from_slice(b"II");
        tiff.extend_from_slice(&42u16.to_le_bytes());
        tiff.extend_from_slice(&8u32.to_le_bytes());
        tiff.extend_from_slice(&1u16.to_le_bytes());
        tiff.extend_from_slice(&0x0112u16.to_le_bytes());
        tiff.extend_from_slice(&3u16.to_le_bytes());
        tiff.extend_from_slice(&1u32.to_le_bytes());
        tiff.extend_from_slice(&(orientation as u16).to_le_bytes());
        tiff.extend_from_slice(&[0, 0]);
        tiff.extend_from_slice(&0u32.to_le_bytes());
        let mut payload = b"Exif\0\0".to_vec();
        payload.extend_from_slice(&tiff);
        let segment_len = (payload.len() + 2) as u16;
        let mut out = vec![0xFF, 0xD8, 0xFF, 0xE1];
        out.extend_from_slice(&segment_len.to_be_bytes());
        out.extend_from_slice(&payload);
        out.extend_from_slice(&jpeg[2..]);
        out
    }

    #[test]
    fn exif_orientation_is_read_and_baked_in() {
        // No EXIF at all → None, and a truncated file too.
        let plain = {
            let image = DynamicImage::new_rgb8(20, 10);
            let mut buffer = std::io::Cursor::new(Vec::new());
            image
                .write_to(&mut buffer, ImageFormat::Jpeg)
                .expect("encode jpeg");
            buffer.into_inner()
        };
        assert_eq!(jpeg_exif_orientation(&plain), None);
        assert_eq!(jpeg_exif_orientation(&[0xFF, 0xD8]), None);
        assert_eq!(jpeg_exif_orientation(b"not a jpeg"), None);

        // Orientation 6 (rotate 90° CW) on a 60x30 photo:
        // the parser reads it, and processing bakes it in —
        // the stored image is 30x60, and its content hash
        // fingerprints the ORIGINAL (rotated-tag) bytes.
        let tagged = jpeg_with_exif_orientation(60, 30, 6);
        assert_eq!(jpeg_exif_orientation(&tagged), Some(6));
        let processed = process_upload("products/p/i", &tagged, Some("image/jpeg"))
            .expect("process tagged jpeg");
        assert_eq!((processed.width, processed.height), (30, 60));
        assert_eq!(processed.content_hash, hex::encode(Sha256::digest(&tagged)));
        let stored = image::load_from_memory(&processed.objects[0].bytes).expect("decode stored");
        {
            use image::GenericImageView;
            assert_eq!(stored.dimensions(), (30, 60));
        }

        // Orientation 3 (upside down) keeps the dimensions.
        let flipped = jpeg_with_exif_orientation(60, 30, 3);
        let processed = process_upload("products/p/i", &flipped, None).expect("process");
        assert_eq!((processed.width, processed.height), (60, 30));
    }

    #[test]
    fn list_page_parsing_reads_keys_tokens_and_escapes() {
        let xml = "<ListBucketResult><IsTruncated>true</IsTruncated>\
                   <Contents><Key>products/p/a-orig</Key></Contents>\
                   <Contents><Key>products/p/b&amp;c-orig</Key></Contents>\
                   <NextContinuationToken>tok&amp;1</NextContinuationToken>\
                   </ListBucketResult>";
        let (keys, next) = parse_list_page(xml);
        assert_eq!(keys, vec!["products/p/a-orig", "products/p/b&c-orig"]);
        assert_eq!(next.as_deref(), Some("tok&1"));
        let (keys, next) = parse_list_page(
            "<ListBucketResult><IsTruncated>false</IsTruncated></ListBucketResult>",
        );
        assert!(keys.is_empty());
        assert_eq!(next, None);
        assert_eq!(sigv4_query_escape("a/b c&d"), "a%2Fb%20c%26d");
    }

    #[tokio::test]
    async fn local_store_list_walks_the_tree() {
        let root = std::env::temp_dir().join(format!(
            "bdm-media-list-{}",
            Uuid::new_v4().simple()
        ));
        let store = LocalDiskStore::new(root.clone());
        store.put("products/p1/i1-orig", b"a", "image/png").await.expect("put");
        store.put("products/p1/i1-600.webp", b"b", "image/webp").await.expect("put");
        store.put("reviews/r1/photo-orig", b"c", "image/png").await.expect("put");
        let mut products = store.list("products/").await.expect("list products");
        products.sort();
        assert_eq!(products, vec!["products/p1/i1-600.webp", "products/p1/i1-orig"]);
        let mut all = store.list("").await.expect("list all");
        all.sort();
        assert_eq!(all.len(), 3);
        assert!(all.contains(&"reviews/r1/photo-orig".to_string()));
        tokio::fs::remove_dir_all(&root).await.expect("cleanup");
    }

    /// A scripted in-process S3 endpoint for the driver
    /// tests: accepts one request per connection, records
    /// (method, target, authorization), and answers by
    /// shape — PUT 200, GET-object the canned bytes,
    /// DELETE 204, list-type GET the canned XML page.
    async fn start_mock_s3() -> (
        String,
        std::sync::Arc<std::sync::Mutex<Vec<(String, String, String)>>>,
        tokio::task::JoinHandle<()>,
    ) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock s3");
        let address = listener.local_addr().expect("mock s3 address");
        let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let log_in_task = log.clone();
        let server = tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let log = log_in_task.clone();
                tokio::spawn(async move {
                    // Read the header block, then the body
                    // per Content-Length.
                    let mut buffer = Vec::new();
                    let mut chunk = [0u8; 4096];
                    let header_end = loop {
                        let read = socket.read(&mut chunk).await.unwrap_or(0);
                        if read == 0 {
                            return;
                        }
                        buffer.extend_from_slice(&chunk[..read]);
                        if let Some(end) = buffer
                            .windows(4)
                            .position(|w| w == b"\r\n\r\n")
                        {
                            break end + 4;
                        }
                    };
                    let headers = String::from_utf8_lossy(&buffer[..header_end]).to_string();
                    let content_length: usize = headers
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .and_then(|value| value.trim().parse().ok())
                        })
                        .unwrap_or(0);
                    while buffer.len() < header_end + content_length {
                        let read = socket.read(&mut chunk).await.unwrap_or(0);
                        if read == 0 {
                            break;
                        }
                        buffer.extend_from_slice(&chunk[..read]);
                    }
                    let request_line = headers.lines().next().unwrap_or("").to_string();
                    let mut parts = request_line.split_whitespace();
                    let method = parts.next().unwrap_or("").to_string();
                    let target = parts.next().unwrap_or("").to_string();
                    let authorization = headers
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("authorization:")
                                .map(|value| value.trim().to_string())
                        })
                        .unwrap_or_default();
                    log.lock().expect("log lock").push((method.clone(), target.clone(), authorization));
                    let (status, body): (&str, Vec<u8>) = if target.contains("list-type=2") {
                        (
                            "200 OK",
                            b"<ListBucketResult><IsTruncated>false</IsTruncated>\
                              <Contents><Key>products/p/i-orig</Key></Contents>\
                              <Contents><Key>products/p/i-600.webp</Key></Contents>\
                              </ListBucketResult>"
                                .to_vec(),
                        )
                    } else if method == "PUT" {
                        ("200 OK", Vec::new())
                    } else if method == "GET" {
                        ("200 OK", b"hello-image".to_vec())
                    } else {
                        ("204 No Content", Vec::new())
                    };
                    let response = format!(
                        "HTTP/1.1 {status}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = socket.write_all(response.as_bytes()).await;
                    let _ = socket.write_all(&body).await;
                });
            }
        });
        (format!("http://{address}"), log, server)
    }

    #[tokio::test]
    async fn s3_store_round_trip_against_a_mock_endpoint() {
        let (endpoint, log, server) = start_mock_s3().await;
        let store = S3Store::new(Some(S3Config {
            endpoint,
            bucket: "marketplace-media".to_string(),
            access_key: "mock-access".to_string(),
            secret_key: "mock-secret".to_string(),
            region: "us-east-1".to_string(),
            presign_seconds: 0,
        }));
        store
            .put("products/p/i-orig", b"hello-image", "image/png")
            .await
            .expect("put through the signed client");
        let got = store.get("products/p/i-orig").await.expect("get");
        assert_eq!(got, Some(b"hello-image".to_vec()));
        store.delete("products/p/i-orig").await.expect("delete");
        let keys = store.list("products/").await.expect("list");
        assert_eq!(keys, vec!["products/p/i-orig", "products/p/i-600.webp"]);

        let requests = log.lock().expect("log lock").clone();
        assert_eq!(requests.len(), 4, "one request per operation: {requests:?}");
        assert_eq!(requests[0].0, "PUT");
        assert_eq!(requests[1].0, "GET");
        assert_eq!(requests[2].0, "DELETE");
        assert!(requests[3].1.contains("list-type=2"), "list target: {}", requests[3].1);
        for (_, _, authorization) in &requests {
            assert!(
                authorization.starts_with(
                    "AWS4-HMAC-SHA256 Credential=mock-access/"
                ) && authorization.contains("SignedHeaders=host;x-amz-content-sha256;x-amz-date"),
                "every request is SigV4-signed: {authorization}"
            );
        }
        server.abort();
    }
}
