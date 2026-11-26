/** @type {import('next').NextConfig} */
const nextConfig = {
  // Wave 20: standalone output so the production Docker image
  // (apps/web/Dockerfile) can run the traced server bundle
  // without copying node_modules into the runtime image.
  output: "standalone",

  // Wave 28: the web app's own security headers. This is the
  // safe subset of the API's header set (see the API's
  // `security_headers` middleware): the API serves JSON only,
  // so it can also send `Content-Security-Policy:
  // default-src 'none'` — this app serves HTML/JS/CSS, where
  // that policy would break the site itself, so no CSP is
  // set here yet (a real web CSP is future work, noted in
  // the README's Wave 28 section).
  async headers() {
    return [
      {
        source: "/:path*",
        headers: [
          { key: "X-Content-Type-Options", value: "nosniff" },
          { key: "X-Frame-Options", value: "DENY" },
          { key: "Referrer-Policy", value: "no-referrer" },
          {
            key: "Strict-Transport-Security",
            value: "max-age=31536000",
          },
        ],
      },
    ];
  },
};

module.exports = nextConfig;
