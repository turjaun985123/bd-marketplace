"use client";

import { useEffect, useState } from "react";

/** The Chromium install prompt event (not in the TS DOM lib). */
interface BeforeInstallPromptEvent extends Event {
  prompt: () => Promise<void>;
  userChoice: Promise<{ outcome: "accepted" | "dismissed" }>;
}

/**
 * The "install the app" affordance (Wave 38) — ONE shared
 * component, used by the marketplace home nav and the
 * buyer orders header. It renders nothing until the
 * browser says installing is possible:
 *
 * - Chromium (Android/desktop): captures
 *   `beforeinstallprompt`; the button calls prompt() and
 *   hides whatever the user chose (a dismissed prompt is
 *   the browser's to re-offer, not ours to nag about).
 *   `appinstalled` hides it for good.
 * - iOS Safari has no install prompt event at all, so a
 *   conservative iOS-Safari detection shows a hint line
 *   instead: Share → "Add to Home Screen".
 * - Already installed (display-mode: standalone, or
 *   navigator.standalone on iOS): renders nothing, in
 *   every browser.
 */
export default function InstallAppButton() {
  const [deferredPrompt, setDeferredPrompt] =
    useState<BeforeInstallPromptEvent | null>(null);
  const [showIosHint, setShowIosHint] = useState(false);

  useEffect(() => {
    const nav = window.navigator as Navigator & { standalone?: boolean };
    const isStandalone =
      window.matchMedia("(display-mode: standalone)").matches ||
      nav.standalone === true;
    if (isStandalone) return;

    const onBeforeInstall = (event: Event) => {
      event.preventDefault();
      setDeferredPrompt(event as BeforeInstallPromptEvent);
    };
    const onInstalled = () => {
      setDeferredPrompt(null);
      setShowIosHint(false);
    };
    window.addEventListener("beforeinstallprompt", onBeforeInstall);
    window.addEventListener("appinstalled", onInstalled);

    // iOS Safari: iPhone/iPad UA, a WebKit Safari (no
    // Chrome/Firefox/Edge iOS tokens), and not installed.
    const ua = window.navigator.userAgent;
    const isIos = /iphone|ipad|ipod/i.test(ua);
    const isSafari =
      /safari/i.test(ua) && !/crios|fxios|edgios|opios/i.test(ua);
    if (isIos && isSafari) setShowIosHint(true);

    return () => {
      window.removeEventListener("beforeinstallprompt", onBeforeInstall);
      window.removeEventListener("appinstalled", onInstalled);
    };
  }, []);

  if (deferredPrompt) {
    return (
      <button
        type="button"
        onClick={() => {
          const promptEvent = deferredPrompt;
          setDeferredPrompt(null);
          void promptEvent.prompt();
        }}
      >
        📲 অ্যাপ ইনস্টল করুন — Install app
      </button>
    );
  }

  if (showIosHint) {
    return (
      <span>
        📲 অ্যাপ হিসেবে যোগ করুন: শেয়ার বোতাম → &lsquo;Add to Home
        Screen&rsquo; — Install: Share → &lsquo;Add to Home Screen&rsquo;
      </span>
    );
  }

  return null;
}
