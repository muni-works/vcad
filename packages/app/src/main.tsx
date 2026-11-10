import "./lib/crypto-polyfill";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { initLocale } from "@/stores/locale-store";

initLocale();

// Unregister service workers in dev mode to prevent stale cache issues
if (import.meta.env.DEV && "serviceWorker" in navigator) {
  navigator.serviceWorker.getRegistrations().then((registrations) => {
    for (const registration of registrations) {
      registration.unregister();
      console.log("[dev] Unregistered stale service worker");
    }
  });
}
import { App } from "./App";
import { RetirementPage } from "./components/RetirementPage";
import {
  isProtectedPath,
  planRetirementRoute,
  retirementRedirectsEnabled,
  type RetirementRoute,
} from "./lib/retirement";
import "./index.css";
import { installLastOpenedTracker } from "./lib/last-opened";

installLastOpenedTracker();

// Dev-only automation hook: expose the core stores so headless harnesses
// (and quick console debugging) can drive the app without UI scripting.
// Never shipped — guarded by import.meta.env.DEV.
if (import.meta.env.DEV) {
  void Promise.all([import("@vcad/core"), import("@/stores/electronics-store")]).then(
    ([core, electronics]) => {
      (window as unknown as Record<string, unknown>).__vcadDev = {
        useDocumentStore: core.useDocumentStore,
        useCoreElectronicsStore: core.useCoreElectronicsStore,
        useElectronicsStore: electronics.useElectronicsStore,
      };
    },
  );
}

// vcad.io retirement redirects (docs/launch/web-retirement.md). OFF unless the
// build sets VITE_VCAD_RETIREMENT_REDIRECTS=1; when off this is always null.
const retirementRoute: Exclude<RetirementRoute, { kind: "keep" }> | null = (() => {
  if (!retirementRedirectsEnabled()) return null;
  // Belt and braces: /desktop/* (Sparkle appcast) is static and never redirected.
  if (isProtectedPath(window.location.pathname)) return null;
  const route = planRetirementRoute(window.location);
  return route.kind === "keep" ? null : route;
})();

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    {retirementRoute ? <RetirementPage route={retirementRoute} /> : <App />}
  </StrictMode>,
);

// Vercel Analytics + Speed Insights — prod only, silently skip if blocked
if (import.meta.env.PROD) {
  import("@vercel/analytics").then(({ inject }) => inject()).catch(() => {});
  import("@vercel/speed-insights").then(({ injectSpeedInsights }) => injectSpeedInsights()).catch(() => {});
}

// Defer analytics until after first paint
const initAnalytics = () => {
  const posthogKey = import.meta.env.VITE_POSTHOG_KEY;
  const posthogHost =
    import.meta.env.VITE_POSTHOG_HOST || "https://us.i.posthog.com";
  if (posthogKey) {
    import("posthog-js").then(({ default: posthog }) => {
      posthog.init(posthogKey, {
        api_host: posthogHost,
        person_profiles: "identified_only",
        capture_pageview: true,
        capture_pageleave: true,
        session_recording: {
          maskAllInputs: false,
          maskInputOptions: { password: true },
        },
      });
    });
  }
};
if ("requestIdleCallback" in window) {
  requestIdleCallback(initAnalytics);
} else {
  setTimeout(initAnalytics, 1000);
}
