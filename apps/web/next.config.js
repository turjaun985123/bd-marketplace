/** @type {import('next').NextConfig} */
const nextConfig = {
  // Wave 20: standalone output so the production Docker image
  // (apps/web/Dockerfile) can run the traced server bundle
  // without copying node_modules into the runtime image.
  output: "standalone",
};
module.exports = nextConfig;
