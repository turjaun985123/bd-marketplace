# BD Marketplace web (Next.js) — Wave 20 deployment image.
#
# NEXT_PUBLIC_API_BASE_URL is a BUILD-TIME value: Next.js
# inlines NEXT_PUBLIC_* variables into the browser bundle, so
# pointing the web app at a different API origin means
# REBUILDING this image with a different --build-arg. (The
# variable name is the one the app has used since Wave 2 —
# see lib/api.ts.)
#
# The build relies on `output: "standalone"` in next.config.js:
# the runner stage copies the traced standalone server plus the
# static assets, not node_modules.

FROM node:20-alpine AS deps
WORKDIR /app
COPY package.json package-lock.json ./
RUN npm ci

FROM node:20-alpine AS builder
WORKDIR /app
COPY --from=deps /app/node_modules ./node_modules
COPY . .
ARG NEXT_PUBLIC_API_BASE_URL=http://localhost:8080
ENV NEXT_PUBLIC_API_BASE_URL=$NEXT_PUBLIC_API_BASE_URL
ENV NEXT_TELEMETRY_DISABLED=1
RUN npm run build

FROM node:20-alpine AS runner
WORKDIR /app
ENV NODE_ENV=production
ENV NEXT_TELEMETRY_DISABLED=1
RUN addgroup --system --gid 1001 nodejs \
    && adduser --system --uid 1001 nextjs
COPY --from=builder /app/public ./public
COPY --from=builder --chown=nextjs:nodejs /app/.next/standalone ./
COPY --from=builder --chown=nextjs:nodejs /app/.next/static ./.next/static
USER nextjs
ENV PORT=3000
ENV HOSTNAME=0.0.0.0
EXPOSE 3000
CMD ["node", "server.js"]
