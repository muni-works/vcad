import { describe, expect, it } from "vitest";
import {
  compareVersions,
  judge,
  startUpdateCheck,
  takeResultWarning,
  _setUpdateStatus,
} from "../version-check.js";

describe("version-check", () => {
  it("orders published main builds by run number", () => {
    expect(compareVersions("0.11.0-main.9", "0.11.0-main.152")!).toBeLessThan(0);
    expect(compareVersions("0.11.0-main.152", "0.11.0-main.152")).toBe(0);
    expect(compareVersions("0.11.0", "0.11.0-main.152")!).toBeGreaterThan(0);
    expect(compareVersions("0.10.9-main.999", "0.11.0-main.1")!).toBeLessThan(0);
    expect(compareVersions("0.11.0+abc1234", "0.11.0")).toBe(0);
    expect(compareVersions("garbage", "0.11.0")).toBeNull();
  });

  it("judges stale and warns once in results", () => {
    const s = judge("0.11.0-main.100", "0.11.0-main.152");
    expect(s.state).toBe("stale");
    _setUpdateStatus(s);
    expect(takeResultWarning()).toMatch(
      /out of date: running 0\.11\.0-main\.100, newest is 0\.11\.0-main\.152/,
    );
    expect(takeResultWarning()).toBeNull();
    expect(judge("0.11.0-main.152", "0.11.0-main.152").state).toBe("current");
  });

  it("is offline-safe", async () => {
    const failing = (async () => {
      throw new Error("ENOTFOUND");
    }) as unknown as typeof fetch;
    expect((await startUpdateCheck("0.11.0-main.1", failing)).state).toBe("unknown");
    const ok = (async () =>
      new Response(JSON.stringify({ version: "0.11.0-main.5" }))) as unknown as typeof fetch;
    expect((await startUpdateCheck("0.11.0-main.1", ok)).state).toBe("stale");
    _setUpdateStatus({ state: "unknown", running: "", reason: "reset" });
  });
});
