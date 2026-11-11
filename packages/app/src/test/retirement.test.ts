import { describe, expect, it } from "vitest";
import { gunzipSync, gzipSync, strFromU8, strToU8 } from "fflate";
import {
  appLinkForDocumentText,
  isProtectedPath,
  planRetirementRoute,
  retirementRedirectsEnabled,
  shutdownLabel,
} from "@/lib/retirement";

const loc = (pathname: string, search = "", hash = "") => ({ pathname, search, hash });
const b64url = (b: Uint8Array) =>
  btoa(String.fromCharCode(...b)).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
const docOf = (appUrl: string) => {
  const d = new URL(appUrl).searchParams.get("doc")!.replace(/-/g, "+").replace(/_/g, "/");
  return JSON.parse(strFromU8(gunzipSync(Uint8Array.from(atob(d), (c) => c.charCodeAt(0)))));
};

describe("retirement flag", () => {
  it("defaults off", () => {
    expect(retirementRedirectsEnabled({})).toBe(true);
    expect(retirementRedirectsEnabled({ DEV: true })).toBe(false);
    expect(retirementRedirectsEnabled({ DEV: true, VITE_VCAD_RETIREMENT_REDIRECTS: "1" })).toBe(true);
    expect(retirementRedirectsEnabled({ VITE_VCAD_RETIREMENT_REDIRECTS: "0" })).toBe(false);
    expect(retirementRedirectsEnabled({ VITE_VCAD_RETIREMENT_REDIRECTS: "1" })).toBe(true);
  });
  it("has a shutdown placeholder", () => {
    expect(shutdownLabel(null)).toMatch(/announced/);
    expect(shutdownLabel("March 1, 2027")).toBe("March 1, 2027");
  });
});

describe("isProtectedPath", () => {
  it("covers /desktop and below only", () => {
    expect(isProtectedPath("/desktop/appcast.xml")).toBe(true);
    expect(isProtectedPath("/desktopx")).toBe(false);
  });
});

describe("planRetirementRoute", () => {
  it("never touches the Sparkle appcast or /desktop", () => {
    for (const p of ["/desktop/appcast.xml", "/desktop/", "/desktop", "/desktop/index.html", "/Desktop/appcast.xml"]) {
      expect(planRetirementRoute(loc(p))).toEqual({ kind: "keep" });
      expect(planRetirementRoute(loc(p, "?ir=x", "#/new?doc=x"))).toEqual({ kind: "keep" });
    }
    expect(planRetirementRoute(loc("/desktopx"))).not.toEqual({ kind: "keep" });
  });

  it("keeps legal pages and the export page", () => {
    expect(planRetirementRoute(loc("/privacy"))).toEqual({ kind: "keep" });
    expect(planRetirementRoute(loc("/export/"))).toEqual({ kind: "export" });
  });

  it("explains retired flows", () => {
    expect(planRetirementRoute(loc("/"))).toEqual({ kind: "notice", reason: "editor" });
    expect(planRetirementRoute(loc("/cli-auth", "?code=x"))).toEqual({ kind: "notice", reason: "cli-auth" });
    expect(planRetirementRoute(loc("/authorize/123e4567-e89b-12d3-a456-426614174000"))).toEqual({
      kind: "notice",
      reason: "authorize",
    });
    expect(planRetirementRoute(loc("/~abcdefgh12"))).toEqual({ kind: "notice", reason: "local-doc" });
    expect(planRetirementRoute(loc("/d/abcdefgh12"))).toEqual({ kind: "notice", reason: "local-doc" });
    expect(planRetirementRoute(loc("/@cam"))).toEqual({ kind: "notice", reason: "profile" });
  });

  it("routes shares to a fetch-and-hand-off", () => {
    expect(planRetirementRoute(loc("/view/123e4567-e89b-12d3-a456-426614174000"))).toEqual({
      kind: "share",
      token: "123e4567-e89b-12d3-a456-426614174000",
    });
    expect(planRetirementRoute(loc("/@cam/bracket"))).toEqual({ kind: "share", username: "cam", slug: "bracket" });
  });

  it("hands #/new JSON documents to vcad://", () => {
    const json = JSON.stringify({ version: "0.1", nodes: {}, roots: [] });
    const hash = `#/new?doc=${b64url(gzipSync(strToU8(json)))}&name=Part`;
    const r = planRetirementRoute(loc("/", "", hash));
    expect(r.kind).toBe("handoff");
    if (r.kind !== "handoff") return;
    expect(r.name).toBe("Part");
    expect(r.appUrl.startsWith("vcad://open?")).toBe(true);
    expect(new URL(r.appUrl).searchParams.get("name")).toBe("Part");
    expect(docOf(r.appUrl)).toEqual(JSON.parse(json));
  });

  it("flags unreadable inline links", () => {
    expect(planRetirementRoute(loc("/", "", "#/new?doc=!!!"))).toEqual({ kind: "notice", reason: "bad-link" });
  });

  it("converts VCode to JSON for the app", () => {
    const url = appLinkForDocumentText("#{}\n", "x");
    expect(url.startsWith("vcad://open?doc=")).toBe(true);
  });
});
