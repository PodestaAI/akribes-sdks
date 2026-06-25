"""Token-safety guard for URL embedding.

Refuse to put a non-scoped token in a URL query string. Scoped tokens
(``akribes_tk_…`` / ``aura_tk_…``) are short-lived and revokable, so
appearing in access logs is acceptable. The raw ``<secret>`` half of
``AKRIBES_SERVICE_TOKEN_<NAME>=*:<secret>`` is long-lived and
wildcard-Admin in production deployments — a single leak equals full
platform compromise (PENTEST CRITICAL-02). Backends that use a service
token must authenticate via the ``Authorization`` header, not via
``?token=`` in a URL that reverse-proxy access logs and OTel
``http.url`` span attributes will capture.
"""

from __future__ import annotations


def is_scoped_token(token: str) -> bool:
    """Return ``True`` when *token* looks like a DB-stored **scoped** token.

    Scoped tokens carry the ``akribes_tk_`` prefix (or the legacy
    ``aura_tk_``); everything else — most importantly the raw secret half
    of ``AKRIBES_SERVICE_TOKEN_<NAME>=*:<secret>`` — returns ``False``.

    This is the non-throwing companion to :func:`assert_token_safe_in_url`.
    It lets callers branch on a precondition the SDK otherwise only signals
    by raising, e.g. choosing header-bearer vs ``?token=`` auth, surfacing
    an "expires" affordance only for scoped tokens, or catching a
    misconfigured service-token secret early — without a ``try/except``::

        if is_scoped_token(token):
            params["token"] = token        # short-lived, log-leakage OK
        else:
            headers["Authorization"] = f"Bearer {token}"  # header-only

    This is a *shape* check on the prefix, not a validity check: it does not
    contact the server, so a well-formed-but-revoked scoped token still
    returns ``True``.
    """
    return token.startswith("akribes_tk_") or token.startswith("aura_tk_")


def assert_token_safe_in_url(token: str) -> None:
    """Raise ``ValueError`` if *token* must not be embedded in a URL.

    Acceptable: ``akribes_tk_…`` and the legacy ``aura_tk_…`` prefixes.
    Everything else (raw service-token secrets, JWTs, opaque bearers)
    is rejected so the caller is forced onto the header-bearer path.
    """
    if is_scoped_token(token):
        return
    raise ValueError(
        "Refusing to put a non-scoped token in the URL query string. "
        "Scoped tokens (akribes_tk_…) may be passed in ?token= because "
        "they are short-lived and revokable; service tokens (the secret "
        "half of AKRIBES_SERVICE_TOKEN_<NAME>=*:secret) MUST use header "
        "bearer auth and never appear in URLs that hit access logs."
    )
