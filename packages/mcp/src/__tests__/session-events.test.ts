import { describe, it, expect } from "vitest";
import {
  NoopSessionEventStore,
  createSessionEventStore,
  type SessionEvent,
} from "../session-store.js";

const kernelEvt = (): SessionEvent => ({
  author: "agent",
  kind: "kernel",
  type: "create",
  payload: { tool: "create", args: {} },
});

describe("NoopSessionEventStore", () => {
  it("append is a no-op and list is empty", async () => {
    const store = new NoopSessionEventStore();
    await expect(store.append("doc_a", kernelEvt())).resolves.toBeUndefined();
    expect(await store.list("doc_a")).toEqual([]);
  });
});

describe("createSessionEventStore factory (local build)", () => {
  it("is always the no-op store, even with Supabase env set", () => {
    process.env.SUPABASE_URL = "https://supa.test/";
    process.env.SUPABASE_SERVICE_ROLE_KEY = "k";
    try {
      expect(createSessionEventStore(null)).toBeInstanceOf(NoopSessionEventStore);
      expect(
        createSessionEventStore({ sub: "user-me", email: "a@b.c" }),
      ).toBeInstanceOf(NoopSessionEventStore);
    } finally {
      delete process.env.SUPABASE_URL;
      delete process.env.SUPABASE_SERVICE_ROLE_KEY;
    }
  });
});
