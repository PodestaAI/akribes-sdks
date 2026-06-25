/**
 * Refuse to put a token in a URL query string unless it looks like a
 * scoped token (`akribes_tk_…` / `aura_tk_…`). Long-lived service
 * tokens — the secret half of `AKRIBES_SERVICE_TOKEN_<NAME>=*:secret`
 * configured on trusted backends like puto — must never traverse a
 * reverse-proxy / load-balancer / Forgejo-runner access log via
 * `?token=`, because a leak of a wildcard-Admin service token equals
 * full platform compromise (PENTEST CRITICAL-02).
 *
 * Call this from any SDK code path that appends a token to a URL.
 * It throws synchronously so misuse is caught at the call site, not
 * silently leaked at request time.
 */
/**
 * Return `true` when `token` looks like a DB-stored **scoped** token
 * (`akribes_tk_…`, or the legacy `aura_tk_…`) rather than the raw secret
 * half of a long-lived service token.
 *
 * This is the non-throwing companion to {@link assertTokenSafeInUrl}: it
 * lets callers branch on a precondition the SDK otherwise only signals by
 * throwing. For example, decide whether a token is safe to pass in a
 * `?token=` query string, whether to surface an "expires" affordance in a
 * UI, or whether the caller accidentally configured a service-token secret
 * where a scoped token was expected — all without a `try/catch`:
 *
 * ```ts
 * if (isScopedToken(token)) {
 *   url.searchParams.set('token', token); // short-lived, log-leakage OK
 * } else {
 *   headers['Authorization'] = `Bearer ${token}`; // header-only
 * }
 * ```
 *
 * Note this is a *shape* check on the prefix, not a validity check: it does
 * not contact the server, and a well-formed-but-revoked scoped token still
 * returns `true`.
 */
export function isScopedToken(token: string): boolean {
  return token.startsWith('akribes_tk_') || token.startsWith('aura_tk_');
}

export function assertTokenSafeInUrl(token: string): void {
  if (isScopedToken(token)) {
    return;
  }
  throw new Error(
    'Refusing to put a non-scoped token in the URL query string. ' +
      'Scoped tokens (akribes_tk_…) may be passed in ?token= because they ' +
      'are short-lived and revokable; service tokens (the secret half of ' +
      'AKRIBES_SERVICE_TOKEN_<NAME>=*:secret) MUST use header bearer auth ' +
      'and never appear in URLs that hit access logs.',
  );
}
