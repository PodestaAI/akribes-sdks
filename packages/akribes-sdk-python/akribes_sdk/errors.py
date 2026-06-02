"""Exception hierarchy for the Akribes SDK.

All SDK-raised exceptions derive from :class:`AkribesError`. HTTP-backed errors
carry ``status``, ``body_snippet`` and (for rate-limit / 503) ``retry_after``
so callers can build robust retry policies without re-parsing responses.

Hierarchy::

    AkribesError
    ├── AkribesConnectionError       (network failure, no response)
    ├── AkribesHTTPError             (abstract: got an HTTP response ≥ 400)
    │   ├── AuthError             (401 / 403)
    │   ├── NotFoundError         (404)
    │   ├── TransientError        (502 / 503 / overload — umbrella for 5xx)
    │   │   ├── ServerError500    (500)
    │   │   ├── BadGateway502     (502)
    │   │   ├── ServiceUnavailable503  (503)
    │   │   ├── GatewayTimeout504  (504)
    │   │   └── RateLimitError    (429)
    │   └── AkribesConversionError   (502 + conversion_failed)
    ├── ScriptError               (workflow failed)
    ├── AkribesTimeoutError          (await_result timeout)
    ├── ScriptSchemaChangedError  (client-side contract)
    └── ScriptInputMismatchError  (client-side contract)
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Literal


class AkribesError(Exception):
    """Base error for all Akribes SDK errors."""

    def __init__(self, message: str = "") -> None:
        super().__init__(message)
        self.message = message


class AkribesConnectionError(AkribesError):
    """Failed to connect to the Akribes server (DNS, TCP, TLS, timeout before
    response). There is no HTTP response available in this case."""


class AkribesHTTPError(AkribesError):
    """Abstract parent for errors corresponding to an HTTP response ≥ 400."""

    def __init__(
        self,
        message: str,
        *,
        status: int | None = None,
        body_snippet: str | None = None,
        retry_after: float | None = None,
        execution_id: str | None = None,
    ) -> None:
        super().__init__(message)
        self.status = status
        """HTTP status code, or ``None`` if not available."""
        self.body_snippet = body_snippet
        """First ~500 chars of the response body (handy for logs)."""
        self.retry_after = retry_after
        """Value of the ``Retry-After`` header, in seconds (429 / 503 only)."""
        self.execution_id = execution_id
        """Execution ID this error is associated with, if known."""


class AuthError(AkribesHTTPError):
    """Authentication / authorization failure (HTTP 401 or 403).

    Do not retry — the token is invalid, expired, or lacks the required
    scope. For scoped tokens, mint a replacement via ``client.tokens.mint``.
    """


class NotFoundError(AkribesHTTPError):
    """Requested resource was not found (HTTP 404)."""

    def __init__(
        self,
        message: str,
        *,
        resource_type: str | None = None,
        resource_id: str | None = None,
        status: int | None = 404,
        body_snippet: str | None = None,
    ) -> None:
        super().__init__(message, status=status, body_snippet=body_snippet)
        self.resource_type = resource_type
        self.resource_id = resource_id


class TransientError(AkribesHTTPError):
    """Retriable HTTP error.

    Umbrella for any 5xx where retrying is appropriate. Status-specific
    subclasses (#1296) carry distinct retry semantics:

    - :class:`ServerError500`        — generic 500; start short (1s).
    - :class:`BadGateway502`         — 502; start short (1s).
    - :class:`ServiceUnavailable503` — 503; rate-limit-adjacent (2s).
    - :class:`GatewayTimeout504`     — 504; longer base (4s).

    Use ``isinstance(e, TransientError)`` for the umbrella check, or
    branch on the specific subclass when the per-status cadence matters.
    Always respect ``retry_after`` if set.
    """

    @staticmethod
    def recommended_backoff_seconds(status: int | None) -> float | None:
        """Recommended base backoff (in seconds) for a transient *status*.

        Mirrors the Rust core's ``ErrorKind::base_backoff_ms`` / TS SDK's
        ``recommendedBackoffMs`` (#1296). Returns ``None`` for any status
        the SDK doesn't classify as retriable.
        """
        if status == 429:
            return 2.0
        if status == 500:
            return 1.0
        if status == 502:
            return 1.0
        if status == 503:
            return 2.0
        if status == 504:
            return 4.0
        return None


class ServerError500(TransientError):
    """HTTP 500 — generic internal server error. Origin reported a failure;
    retry with a short exponential backoff. (#1296)
    """


class BadGateway502(TransientError):
    """HTTP 502 — bad gateway. The provider's edge fronted a failing
    origin; retry with a short backoff. (#1296)
    """


class ServiceUnavailable503(TransientError):
    """HTTP 503 — service unavailable. Rate-limit-adjacent; honour
    ``Retry-After`` aggressively, otherwise back off at the rate-limit
    cadence. (#1296)
    """


class GatewayTimeout504(TransientError):
    """HTTP 504 — gateway timeout. Upstream is slow or stuck; start with
    a longer base backoff than for 500/502. (#1296)
    """


class ScriptError(AkribesError):
    """Workflow execution failed for this input.

    ``error_kind`` mirrors the server's classification (see
    :class:`akribes_sdk.models.ErrorKind`). ``execution_id`` identifies the
    failed run for introspection.
    """

    def __init__(
        self,
        message: str,
        *,
        execution_id: str | None = None,
        error_kind: str | None = None,
    ) -> None:
        super().__init__(message)
        self.execution_id = execution_id
        self.error_kind = error_kind


class AkribesTimeoutError(AkribesError):
    """Execution did not complete within the client-supplied timeout."""

    def __init__(self, message: str, *, execution_id: str | None = None) -> None:
        super().__init__(message)
        self.execution_id = execution_id


class AkribesConversionError(AkribesHTTPError):
    """Document conversion via Docling failed (HTTP 502 w/ ``conversion_failed``)."""

    def __init__(
        self,
        message: str,
        reason: str,
        attempts: int,
        *,
        status: int | None = 502,
        body_snippet: str | None = None,
    ) -> None:
        super().__init__(message, status=status, body_snippet=body_snippet)
        self.reason = reason
        self.attempts = attempts


class DocumentConversionError(AkribesError):
    """The conversion pipeline reported a terminal failure for a document.

    Two trigger paths mirror the TS SDK:

    1. The server returned a 4xx/5xx with ``error_type: "conversion_failed"``
       during claim/upload (e.g. the VLM rejected the file, pdfium couldn't
       read it, the conversion service was down). ``document_id`` is empty
       because no document was created.
    2. The server returned 200 with ``conversion_status: "failed"`` from claim
       (the bytes have a poisoned blob row that hasn't been reclaimed yet).

    The message is server-supplied and user-facing — surface it directly in
    the UI instead of swapping in a generic fallback. ``reason`` is a stable
    machine-readable string for branching: ``service_unavailable``,
    ``service_rejected``, ``file_too_large``, ``invalid_file``,
    ``extraction_failed``, ``unsupported_format``."""

    def __init__(
        self,
        document_id: str,
        message: str,
        reason: str | None = None,
    ) -> None:
        super().__init__(message)
        self.document_id = document_id
        self.reason = reason


class IngestTimeoutError(AkribesError):
    """Raised when :meth:`DocumentsClient.ingest` polls past its deadline.

    Carries the partially-resolved ``document_id`` (may be empty if the
    initial claim never succeeded) and ``elapsed_ms``. Honors the
    ``AKRIBES_SDK_INGEST_TIMEOUT_SECS`` env var (default 300 s) at client
    construction; can be overridden per-call via ``poll_timeout_ms``."""

    def __init__(self, message: str, document_id: str, elapsed_ms: int) -> None:
        super().__init__(message)
        self.document_id = document_id
        self.elapsed_ms = elapsed_ms


class IngestProtocolError(AkribesError):
    """Schema-drift signal: the server returned a value the SDK doesn't
    recognize (e.g. ``conversion_status: "unknown"``). Bubble loudly so the
    drift gets caught instead of silently masked."""

    def __init__(self, message: str, received_status: str | None = None) -> None:
        super().__init__(message)
        self.received_status = received_status


# ────────────────────────────────────────────────────────────────────────
# Client-side contract errors (no HTTP response)
# ────────────────────────────────────────────────────────────────────────


class ScriptSchemaChangedError(AkribesError):
    """Script schema changed since codegen — regenerate types to continue.

    Raised when the live server's schema hash for a script differs from the
    hash embedded in the generated :class:`~akribes_sdk.ScriptType`. Regenerate
    via ``akribes types pull --project <name-or-id> --lang python``.
    """

    def __init__(self, script_name: str) -> None:
        super().__init__(
            f'Script "{script_name}" schema has changed since codegen. '
            f"Regenerate types: akribes types pull --project <name-or-id> --lang python"
        )
        self.script_name = script_name


class ScriptInputMismatchError(AkribesError):
    """Input keys don't match the cached script input schema.

    No longer raised by the SDK — the server validates inputs authoritatively
    and returns a 400 with a structured error body. Retained as a public name
    so existing ``isinstance`` / ``except`` clauses keep compiling.
    """

    def __init__(self, script_name: str, missing: list[str], extra: list[str]) -> None:
        parts: list[str] = []
        if missing:
            parts.append(f"missing: {', '.join(missing)}")
        if extra:
            parts.append(f"extra: {', '.join(extra)}")
        super().__init__(f'Script "{script_name}" input mismatch: {"; ".join(parts)}')
        self.script_name = script_name
        self.missing = missing
        self.extra = extra


class RateLimitError(TransientError):
    """The server rate-limited the request (HTTP 429).

    ``retry_after`` is populated from the ``Retry-After`` header when the
    server provides it in seconds. For HTTP-date form it's ``None`` and
    callers should fall back to their default backoff.

    Example::

        try:
            await client.scripts.get("my_script")
        except RateLimitError as e:
            await asyncio.sleep(e.retry_after or 5)
            ...
    """


class AlreadyExistsError(AkribesHTTPError):
    """Resource already exists (HTTP 409 with ``error_type=suite_already_exists``).

    Carries ``existing_id`` so callers can redirect to the existing row.
    """

    def __init__(
        self,
        message: str,
        *,
        status: int | None = 409,
        body_snippet: str | None = None,
        existing_id: int | None = None,
    ) -> None:
        super().__init__(message, status=status, body_snippet=body_snippet)
        self.existing_id = existing_id


AkribesAlreadyExistsError = AlreadyExistsError


class CaseFieldError:
    """One per-field violation in a bench ``case_type_mismatch`` 400 body.

    Mirrors the server's ``contracts::TypeMismatch`` (`{path, message}`) and
    the TS SDK's ``CaseFieldError``."""

    __slots__ = ("path", "message")

    def __init__(self, path: str, message: str) -> None:
        self.path = path
        self.message = message

    def __repr__(self) -> str:  # pragma: no cover - trivial
        return f"CaseFieldError(path={self.path!r}, message={self.message!r})"

    def __eq__(self, other: object) -> bool:
        return (
            isinstance(other, CaseFieldError)
            and other.path == self.path
            and other.message == self.message
        )


class CaseTypeMismatchError(AkribesHTTPError):
    """A bench case payload failed the server's contract validation (HTTP 400
    ``{"error": "case_type_mismatch", "field_errors": [{path, message}]}``).

    Raised by :meth:`Bench.create_case`, :meth:`Bench.patch_case`, and
    :meth:`BenchRuns.promote_execution`. ``field_errors`` lets form UIs
    highlight each offending leaf. Mirrors the TS SDK's
    ``CaseTypeMismatchError``."""

    def __init__(
        self,
        message: str,
        *,
        field_errors: list[CaseFieldError] | None = None,
        status: int | None = 400,
        body_snippet: str | None = None,
    ) -> None:
        super().__init__(message, status=status, body_snippet=body_snippet)
        self.field_errors = field_errors or []


class JudgeContractError(AkribesHTTPError):
    """A bench-run trigger failed the judge-contract pre-flight (HTTP 400
    ``"Judge contract mismatch: …"``). The judge's ``inputs.{expected,actual}``
    cannot read the workflow's ``outputs.*``.

    Raised by :meth:`Bench.trigger_run`. ``breaks`` carries the parsed list of
    incompatible fields when the server message includes them. Mirrors the TS
    SDK's ``JudgeContractError``."""

    def __init__(
        self,
        message: str,
        *,
        breaks: list[str] | None = None,
        status: int | None = 400,
        body_snippet: str | None = None,
    ) -> None:
        super().__init__(message, status=status, body_snippet=body_snippet)
        self.breaks = breaks or []


class AkribesScriptError(ScriptError):
    """Deprecated alias for :class:`ScriptError`."""

    def __init__(self, message: str, execution_id: str | None = None) -> None:
        super().__init__(message, execution_id=execution_id)


# ────────────────────────────────────────────────────────────────────────
# Input-validation helper (#1017) — mirrors TS `tryParseInputValidationErrors`.
# ────────────────────────────────────────────────────────────────────────


@dataclass(frozen=True)
class InputValidationEntry:
    """One entry in the server's ``input_validation_failed`` 400 body.

    Mirrors TS `InputValidationErrorEntry`. The ``code`` literal union matches
    the server's `AKRIBES-E-INPUT-VALIDATION` taxonomy.
    """

    input: str
    """Dotted / bracketed path to the offending field, e.g. ``"payload.b"``,
    ``"items[2].qty"``."""
    code: Literal[
        "missing",
        "wrong_type",
        "unknown_field",
        "unknown_input",
        "disallowed_type",
    ]
    expected: str | None = None
    got: str | None = None


def parse_input_validation_errors(err: object) -> list[InputValidationEntry] | None:
    """Parse a 400 ``input_validation_failed`` body off an
    :class:`AkribesHTTPError`. Returns ``None`` when the error is something
    else or the body doesn't match.

    Mirrors TS ``tryParseInputValidationErrors`` (#1017). Form-style UIs use
    this to map per-field errors back to inputs without regex-matching the
    text message.
    """
    import json as _json

    if not isinstance(err, AkribesHTTPError):
        return None
    if err.status != 400 or not err.body_snippet:
        return None
    try:
        body = _json.loads(err.body_snippet)
    except (ValueError, TypeError):
        return None
    if not isinstance(body, dict):
        return None
    if body.get("error") != "input_validation_failed":
        return None
    entries = body.get("errors")
    if not isinstance(entries, list):
        return None
    out: list[InputValidationEntry] = []
    for e in entries:
        if not isinstance(e, dict):
            continue
        input_name = e.get("input")
        code = e.get("code")
        if not isinstance(input_name, str) or not isinstance(code, str):
            continue
        out.append(
            InputValidationEntry(
                input=input_name,
                code=code,  # type: ignore[arg-type]
                expected=e.get("expected") if isinstance(e.get("expected"), str) else None,
                got=e.get("got") if isinstance(e.get("got"), str) else None,
            )
        )
    return out


__all__ = [
    "AkribesError",
    "AkribesConnectionError",
    "AkribesHTTPError",
    "AuthError",
    "NotFoundError",
    "RateLimitError",
    "TransientError",
    "ServerError500",
    "BadGateway502",
    "ServiceUnavailable503",
    "GatewayTimeout504",
    "ScriptError",
    "AkribesTimeoutError",
    "AkribesConversionError",
    "DocumentConversionError",
    "IngestTimeoutError",
    "IngestProtocolError",
    "ScriptSchemaChangedError",
    "ScriptInputMismatchError",
    "AlreadyExistsError",
    "AkribesAlreadyExistsError",
    "CaseFieldError",
    "CaseTypeMismatchError",
    "JudgeContractError",
    "InputValidationEntry",
    "parse_input_validation_errors",
]


