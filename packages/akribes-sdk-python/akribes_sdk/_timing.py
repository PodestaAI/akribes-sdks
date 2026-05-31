"""Internal: timedelta normalisation for SDK params that accept either form."""
from __future__ import annotations

from datetime import timedelta
from typing import TypeVar

T = TypeVar("T")


def to_seconds(v: timedelta | float | int) -> float:
    """Coerce a timedelta-or-numeric to seconds (float)."""
    if isinstance(v, timedelta):
        return v.total_seconds()
    return float(v)


def as_list(v: T | list[T] | None) -> list[T] | None:
    """Accept a scalar or a list; always return a list (or None)."""
    if v is None:
        return None
    if isinstance(v, (list, tuple)):
        return list(v)
    return [v]
