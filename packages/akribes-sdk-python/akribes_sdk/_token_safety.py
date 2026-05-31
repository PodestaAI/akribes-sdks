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


def assert_token_safe_in_url(token: str) -> None:
    """Raise ``ValueError`` if *token* must not be embedded in a URL.

    Acceptable: ``akribes_tk_…`` and the legacy ``aura_tk_…`` prefixes.
    Everything else (raw service-token secrets, JWTs, opaque bearers)
    is rejected so the caller is forced onto the header-bearer path.
    """
    if token.startswith("akribes_tk_") or token.startswith("aura_tk_"):
        return
    raise ValueError(
        "Refusing to put a non-scoped token in the URL query string. "
        "Scoped tokens (akribes_tk_…) may be passed in ?token= because "
        "they are short-lived and revokable; service tokens (the secret "
        "half of AKRIBES_SERVICE_TOKEN_<NAME>=*:secret) MUST use header "
        "bearer auth and never appear in URLs that hit access logs."
    )
