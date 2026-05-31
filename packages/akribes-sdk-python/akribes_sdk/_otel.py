"""Internal: optional OpenTelemetry instrumentation.

Behind an extra dep (`pip install 'akribes-sdk[otel]'`); kept off the import
path until opted into so the base SDK has zero OTel runtime dependency.
"""
from __future__ import annotations

from typing import TYPE_CHECKING, Union

if TYPE_CHECKING:
    from opentelemetry.trace import Tracer


OtelArg = Union[bool, "Tracer", None]


def get_tracer(otel: OtelArg) -> "Tracer | None":
    """Resolve an OTel arg to a Tracer instance, or None (instrumentation off).

    - None / False: returns None.
    - True: imports opentelemetry-api and returns trace.get_tracer("akribes_sdk").
      Raises ImportError with a friendly hint if the dep isn't installed.
    - Tracer instance: returned as-is.
    """
    if otel is None or otel is False:
        return None
    if otel is True:
        try:
            from opentelemetry import trace
        except ImportError as exc:
            raise ImportError(
                "otel=True requires the optional 'otel' extra. Install with:\n"
                "    pip install 'akribes-sdk[otel]'\n"
                "Or pass a Tracer instance directly: otel=trace.get_tracer('my-app')"
            ) from exc
        return trace.get_tracer("akribes_sdk")
    # Assume the caller passed a Tracer instance directly. Don't isinstance-check
    # (we don't want to import opentelemetry just to do that); duck-typed.
    return otel  # type: ignore[return-value]


def inject_into_headers(headers: dict[str, str]) -> None:
    """Auto-propagate current span context into headers (W3C traceparent etc.).

    Best-effort; failures are swallowed (tracing must never break a request).
    Used as the default trace_inject when otel=True and no explicit hook was
    provided.
    """
    try:
        from opentelemetry import propagate
        propagate.inject(headers)
    except Exception:
        # Wrong/missing OTel; nothing to inject.
        pass
