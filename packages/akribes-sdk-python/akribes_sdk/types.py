"""Typed dataclasses for engine event variants and SDK input type aliases.

This module re-exports the event-type classes that mirror the wire-shape
emitted by :class:`akribes_core::event::EngineEvent`. The canonical
definitions live in :mod:`akribes_sdk.models`; this module exists so
consumers can import event types under a stable ``akribes_sdk.types``
namespace alongside the matching TypeScript SDK's ``types.ts``.

Importing event types here is equivalent to importing them from
:mod:`akribes_sdk.models` or the top-level :mod:`akribes_sdk` package —
the underlying objects are the same identities.

S3 input type aliases
---------------------
These aliases signal to :meth:`~akribes_sdk.resources.Executions.run`
which endpoint to dispatch to::

    from akribes_sdk.types import S3PresignedDoc
    await proj.executions.run("ocr", doc=S3PresignedDoc(presigned_url="https://..."))
"""

from __future__ import annotations

from akribes_sdk.models import (
    AgentOutputEvent,
    ContextCompactedEvent,
    ContextOverflowEvent,
    EngineEvent,
    EngineEventType,
    ErrorEvent,
    LogEvent,
    S3CredentialsRef,
    S3DocumentRef,
    S3PresignedRef,
    SuspendedEvent,
    TaskEndEvent,
    TaskStartEvent,
    ToolApprovalPendingEvent,
    TypedEngineEvent,
    UnknownEvent,
    ValidationFailureEvent,
    WorkflowEndEvent,
    WorkflowStartEvent,
    parse_engine_event,
)

# Friendly aliases for S3 input types
S3Doc = S3DocumentRef             # the union (Presigned | Credentials)
S3PresignedDoc = S3PresignedRef
S3CredentialsDoc = S3CredentialsRef

__all__ = [
    # Engine event types
    "AgentOutputEvent",
    "ContextCompactedEvent",
    "ContextOverflowEvent",
    "EngineEvent",
    "EngineEventType",
    "ErrorEvent",
    "LogEvent",
    "SuspendedEvent",
    "TaskEndEvent",
    "TaskStartEvent",
    "ToolApprovalPendingEvent",
    "TypedEngineEvent",
    "UnknownEvent",
    "ValidationFailureEvent",
    "WorkflowEndEvent",
    "WorkflowStartEvent",
    "parse_engine_event",
    # S3 input type aliases
    "S3Doc",
    "S3PresignedDoc",
    "S3CredentialsDoc",
    "S3DocumentRef",
    "S3PresignedRef",
    "S3CredentialsRef",
]
