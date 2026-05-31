"""ScriptType[I, O] — typed wrapper around a script name + schema hash.

Used by `akribes types pull` codegen and consumed by ProjectHandle.run():

    from akribes_types.podesta import summarize         # ScriptType[I, O]
    out = await proj.run(summarize, brief="hi")          # IDE knows the input/output shape
"""
from __future__ import annotations
from typing import Generic, TypeVar

I = TypeVar("I")
O = TypeVar("O")


class ScriptType(Generic[I, O]):
    """A typed reference to a published script.

    Carries the script name and a content hash of its schema (inputs +
    workflow_return) at generation time. The hash lets the SDK detect
    schema drift on first use and prompt regeneration before a failed run.

    Construct via `akribes types pull`'s codegen output; not intended for
    hand-written ScriptType instances.

    Schema drift is verified lazily on first use through
    :meth:`ProjectHandle._verify_schema <akribes_sdk._handles.ProjectHandle._verify_schema>`.
    When ``schema_hash`` is non-empty, the first call to :meth:`ProjectHandle.run`
    or :meth:`ProjectHandle.run_and_await` fetches ``/signature`` and compares
    hashes; a mismatch raises :class:`~akribes_sdk.errors.ScriptSchemaChangedError`.
    Subsequent calls with the same (project_id, script_name, schema_hash) tuple
    skip the fetch. Scripts with ``schema_hash=""`` (no codegen hash) skip
    verification entirely.
    """

    __slots__ = ("name", "schema_hash")

    def __init__(self, name: str, *, schema_hash: str = "") -> None:
        self.name = name
        self.schema_hash = schema_hash

    def __repr__(self) -> str:
        h = (self.schema_hash[:8] + "…") if self.schema_hash else "<no-hash>"
        return f"ScriptType(name={self.name!r}, schema_hash={h!r})"

    def __eq__(self, other: object) -> bool:
        if not isinstance(other, ScriptType):
            return NotImplemented
        return self.name == other.name and self.schema_hash == other.schema_hash

    def __hash__(self) -> int:
        return hash((self.name, self.schema_hash))
