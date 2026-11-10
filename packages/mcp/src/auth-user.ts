/**
 * Caller identity threaded through tool contexts.
 *
 * The hosted build filled this from an OAuth bearer token. The local build has
 * no accounts, so it is always null; the type stays so per-user keying in the
 * in-process stores keeps one shape.
 */
export interface AuthUser {
  sub: string;
  email: string;
}
