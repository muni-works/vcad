/**
 * vcad Fabricate — store for quotes + orders.
 *
 * In-memory only (per process) in the local build.
 */

import type { AuthUser } from "../auth-user.js";
import type {
  AuthorizationStatus,
  DebitResult,
  Order,
  OrderReceiptStatus,
  OrderState,
  Quote,
  QuoteEconomics,
  SpendAuthorization,
} from "./types.js";

export interface OrderFilter {
  status?: OrderState;
  limit?: number;
}

/** Parameters for an atomic wallet debit (forwarded to the debit_wallet RPC). */
export interface DebitParams {
  userId: string;
  amountMinor: number;
  orderId: string;
  authorizationId: string;
  idempotencyKey: string;
}

/** Enrichment columns a state transition may carry alongside state+events
 *  (place_order records its receipt verdict; authorize_spend links the
 *  proposed authorization). Optional so plain transitions stay one-argument. */
export interface OrderStatePatch {
  receipt_status?: OrderReceiptStatus;
  authorization_id?: string | null;
}

export interface FabricateStore {
  saveQuote(quote: Quote, econ: QuoteEconomics, userId: string): Promise<void>;
  /** Read a persisted quote, ownership-scoped (BOM lines link quotes by id). */
  getQuote(quoteId: string, userId: string): Promise<Quote | null>;
  saveOrder(order: Order, fabCostMinor: number, userId: string): Promise<void>;
  getOrder(orderId: string, userId: string): Promise<Order | null>;
  listOrders(userId: string, filter: OrderFilter): Promise<Order[]>;
  // ── money plane (Phase 4, flag-gated) ──
  /** Persist a proposed spend authorization (status pending_human). */
  createAuthorization(authz: SpendAuthorization, userId: string): Promise<void>;
  /** Read an authorization, ownership-scoped. */
  getAuthorization(id: string, userId: string): Promise<SpendAuthorization | null>;
  /** Flip an authorization's status (e.g. elicitation decline → revoked).
   *  Never a substitute for the human approval flow — approval stays
   *  web-app/out-of-band; this records lifecycle outcomes. */
  setAuthorizationStatus(id: string, userId: string, status: AuthorizationStatus): Promise<void>;
  /** Atomic, balance-floored, idempotent debit. The ONLY way credits leave. */
  debit(p: DebitParams): Promise<DebitResult>;
  /** Transition an order's state, append a lifecycle event, and optionally
   *  patch enrichment fields (receipt_status / authorization_id). */
  setOrderState(
    orderId: string,
    userId: string,
    state: OrderState,
    note: string,
    patch?: OrderStatePatch,
  ): Promise<void>;
  /** Read the caller's prepaid wallet balance (minor units), or null when it
   *  can't be read (no wallet row, store outage) — callers must treat null as
   *  "unknown", never as zero. */
  getWalletBalance(userId: string): Promise<number | null>;
}

// ── In-memory (module-global so it survives across calls in one process) ──

const memQuotes = new Map<string, { quote: Quote; econ: QuoteEconomics }>();
const memOrders = new Map<string, { order: Order; fabCostMinor: number }>();
const memAuthz = new Map<string, SpendAuthorization>();
const memWallets = new Map<string, number>(); // userId → balance (minor)
// userId::key → the original request + its result, for replay-match idempotency.
const memDebits = new Map<
  string,
  { result: DebitResult; orderId: string; authorizationId: string; amountMinor: number }
>();

const memKey = (userId: string, id: string): string => `${userId}::${id}`;

export class InMemoryFabricateStore implements FabricateStore {
  async saveQuote(quote: Quote, econ: QuoteEconomics, userId: string): Promise<void> {
    memQuotes.set(memKey(userId, quote.quote_id), { quote, econ });
  }
  async getQuote(quoteId: string, userId: string): Promise<Quote | null> {
    return memQuotes.get(memKey(userId, quoteId))?.quote ?? null;
  }
  async saveOrder(order: Order, fabCostMinor: number, userId: string): Promise<void> {
    memOrders.set(memKey(userId, order.order_id), { order, fabCostMinor });
  }
  async getOrder(orderId: string, userId: string): Promise<Order | null> {
    return memOrders.get(memKey(userId, orderId))?.order ?? null;
  }
  async listOrders(userId: string, filter: OrderFilter): Promise<Order[]> {
    const prefix = `${userId}::`;
    const out: Order[] = [];
    for (const [key, val] of memOrders) {
      if (!key.startsWith(prefix)) continue;
      if (filter.status && val.order.state !== filter.status) continue;
      out.push(val.order);
    }
    out.sort((a, b) => (a.created_at < b.created_at ? 1 : -1));
    return typeof filter.limit === "number" ? out.slice(0, filter.limit) : out;
  }

  async createAuthorization(authz: SpendAuthorization, userId: string): Promise<void> {
    memAuthz.set(memKey(userId, authz.id), { ...authz, user_id: userId });
  }
  async getAuthorization(id: string, userId: string): Promise<SpendAuthorization | null> {
    return memAuthz.get(memKey(userId, id)) ?? null;
  }
  async setAuthorizationStatus(
    id: string,
    userId: string,
    status: AuthorizationStatus,
  ): Promise<void> {
    const a = memAuthz.get(memKey(userId, id));
    if (!a) return;
    memAuthz.set(memKey(userId, id), { ...a, status });
  }

  /**
   * Local mirror of the debit_wallet RPC contract — enough to exercise the
   * place_order logic in tests, enforcing the SAME guards the RPC does:
   * per-(user,key) idempotency WITH replay-match, authorized-only, NOT-expired,
   * amount ceiling, balance floor, one_time consume. The Supabase impl defers
   * to the real SECURITY DEFINER RPC, which is the source of truth.
   */
  async debit(p: DebitParams): Promise<DebitResult> {
    const idk = memKey(p.userId, p.idempotencyKey);
    const prior = memDebits.get(idk);
    if (prior) {
      // A reused key MUST match the original request, or it's an error — mirrors
      // the RPC's idempotency_key_reused guard (027 lines 185-190).
      if (
        prior.orderId !== p.orderId ||
        prior.authorizationId !== p.authorizationId ||
        prior.amountMinor !== p.amountMinor
      ) {
        return { ok: false, reason: "idempotency_key_reused" };
      }
      return { ...prior.result, idempotent: true };
    }

    const authz = memAuthz.get(memKey(p.userId, p.authorizationId));
    if (!authz) return { ok: false, reason: "authz_not_found" };
    if (authz.status === "revoked") return { ok: false, reason: "authz_revoked" };
    if (authz.status !== "authorized") return { ok: false, reason: "authz_not_authorized" };
    if (new Date(authz.expires_at).getTime() <= Date.now()) {
      return { ok: false, reason: "authz_expired" };
    }
    if (p.amountMinor <= 0) return { ok: false, reason: "invalid_amount" };
    if (p.amountMinor > authz.max_amount_minor) {
      return { ok: false, reason: "amount_exceeds_authz" };
    }
    const balance = memWallets.get(p.userId) ?? 0;
    if (balance < p.amountMinor) {
      return { ok: false, reason: "insufficient_funds", balance_minor: balance };
    }
    const after = balance - p.amountMinor;
    memWallets.set(p.userId, after);
    if (authz.kind === "one_time") {
      memAuthz.set(memKey(p.userId, authz.id), { ...authz, status: "consumed" });
    }
    const result: DebitResult = { ok: true, balance_minor: after };
    memDebits.set(idk, {
      result,
      orderId: p.orderId,
      authorizationId: p.authorizationId,
      amountMinor: p.amountMinor,
    });
    return result;
  }

  async setOrderState(
    orderId: string,
    userId: string,
    state: OrderState,
    note: string,
    patch?: OrderStatePatch,
  ): Promise<void> {
    const rec = memOrders.get(memKey(userId, orderId));
    if (!rec) return;
    rec.order.state = state;
    rec.order.events.push({ state, at: new Date().toISOString(), note });
    if (patch?.receipt_status !== undefined) rec.order.receipt_status = patch.receipt_status;
    if (patch?.authorization_id !== undefined) rec.order.authorization_id = patch.authorization_id;
    rec.order.updated_at = new Date().toISOString();
  }

  async getWalletBalance(userId: string): Promise<number | null> {
    return memWallets.get(userId) ?? null;
  }

  // ── test-only seams (NOT on the FabricateStore interface) ──
  /** Seed wallet credit for tests (production top-ups go via credit_wallet). */
  creditWalletForTest(userId: string, minor: number): void {
    memWallets.set(userId, (memWallets.get(userId) ?? 0) + minor);
  }
  /** Simulate the HUMAN web-app approval for tests (never an agent action). */
  approveAuthorizationForTest(id: string, userId: string): boolean {
    const a = memAuthz.get(memKey(userId, id));
    if (!a || a.status !== "pending_human") return false;
    memAuthz.set(memKey(userId, id), { ...a, status: "authorized" });
    return true;
  }
}

/** The store impl: always in-memory in the local build (the hosted Supabase
 *  quote/order/wallet tables were retired with the hosted services). */
export function createFabricateStore(_user?: AuthUser | null): FabricateStore {
  return new InMemoryFabricateStore();
}
/** The owner id used for store scoping — the authenticated user, or "local". */
export function ownerId(user: AuthUser | null): string {
  return user?.sub ?? "local";
}
