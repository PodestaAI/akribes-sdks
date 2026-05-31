"""Internal: retry policy for HTTP requests."""
from __future__ import annotations

import asyncio
import random
from dataclasses import dataclass, field
from typing import Awaitable, Callable, Literal

import httpx

from akribes_sdk.errors import AkribesConnectionError, RateLimitError, TransientError

_RETRYABLE_DEFAULTS: tuple[type[Exception], ...] = (
    TransientError,
    RateLimitError,
    AkribesConnectionError,
)


@dataclass(frozen=True, slots=True)
class ExponentialBackoff:
    """Exponential backoff with optional full jitter.

    Same curve as the heartbeat and SSE reconnect loops: base 1s, cap 30s,
    approximately doubling per attempt. Mirrors the Rust and TS SDK curves
    so the operator story is the same wherever you connect.

    Example::

        bo = ExponentialBackoff()
        delay = bo.delay(attempt)   # attempt is 1-indexed
    """

    base: float = 1.0
    cap: float = 30.0
    jitter: Literal["full", "none"] = "full"

    def delay(self, attempt: int) -> float:
        """Compute the sleep duration (seconds) before retrying attempt *N* (1-indexed).

        Returns 0.0 for attempt ≤ 0.
        """
        if attempt <= 0:
            return 0.0
        exponent = min(attempt - 1, 20)
        exp_capped = min(self.base * (2**exponent), self.cap)
        return random.random() * exp_capped if self.jitter == "full" else exp_capped


@dataclass(frozen=True, slots=True)
class RetryPolicy:
    """Configurable retry policy for client HTTP requests.

    Defaults: 4 attempts, retries :class:`TransientError` (502/503),
    :class:`RateLimitError` (429), and :class:`AkribesConnectionError`; only
    for idempotent methods (GET/HEAD/OPTIONS) or when the request carries an
    explicit ``Idempotency-Key`` header.

    Example::

        # Custom — 6 attempts, transient only, no rate-limit retries
        policy = RetryPolicy(max_attempts=6, on=(TransientError,))

        # Disable all retries
        policy = RetryPolicy.none()
    """

    max_attempts: int = 4
    on: tuple[type[Exception], ...] = field(default_factory=lambda: _RETRYABLE_DEFAULTS)
    backoff: ExponentialBackoff = field(default_factory=ExponentialBackoff)
    respect_retry_after: bool = True
    idempotent_only: bool = True

    @classmethod
    def none(cls) -> "RetryPolicy":
        """A policy that never retries — equivalent to ``max_attempts=1``."""
        return cls(max_attempts=1)


def _is_idempotent(method: str, has_idempotency_key: bool) -> bool:
    """Return True if the request is safe to retry.

    GET/HEAD/OPTIONS are unconditionally safe. Any method that carries an
    ``Idempotency-Key`` is safe because the server deduplicates those.
    """
    return method.upper() in ("GET", "HEAD", "OPTIONS") or has_idempotency_key


async def with_retry(
    send: Callable[[], Awaitable[httpx.Response]],
    *,
    method: str,
    policy: RetryPolicy,
    has_idempotency_key: bool,
) -> httpx.Response:
    """Drive ``send()`` under *policy*; raise the last exception on exhaustion.

    Parameters
    ----------
    send:
        Zero-argument async callable that performs one HTTP attempt. Must
        raise one of the exception types in ``policy.on`` on transient failure
        (not a raw ``httpx`` exception — the caller is responsible for
        translating those before passing them up to here).
    method:
        HTTP verb string (``"GET"``, ``"POST"``, etc.). Used to decide
        whether the request is safe to retry without an idempotency key.
    policy:
        :class:`RetryPolicy` instance controlling attempt count, error types,
        backoff, and safety checks.
    has_idempotency_key:
        ``True`` when the outbound request carries an ``Idempotency-Key``
        header — makes POST/PUT/PATCH safe to retry.
    """
    safe = _is_idempotent(method, has_idempotency_key)
    last_exc: Exception | None = None

    for attempt in range(1, max(1, policy.max_attempts) + 1):
        try:
            return await send()
        except policy.on as exc:  # type: ignore[misc]
            last_exc = exc
            if attempt >= policy.max_attempts:
                raise
            if policy.idempotent_only and not safe:
                raise
            delay = policy.backoff.delay(attempt)
            if policy.respect_retry_after and isinstance(
                exc, (RateLimitError, TransientError)
            ):
                ra = getattr(exc, "retry_after", None)
                if ra is not None:
                    delay = max(delay, float(ra))
            await asyncio.sleep(delay)

    # Unreachable — the loop either returns or raises inside the body.
    assert last_exc is not None
    raise last_exc
