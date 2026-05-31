from __future__ import annotations

from typing import TYPE_CHECKING, Any

if TYPE_CHECKING:
    from akribes_sdk.client import AkribesClient


class _ApiClient:
    """Carries global HTTP context (no project).

    Wraps an :class:`AkribesClient` and exposes only the global-scope
    surface: ``_base_url`` and ``_request``. Resources that need no
    project context receive one of these at construction time.

    All other client attributes (``token``, ``name``, ``_sse_http``,
    ``_broken_scripts``, ``validate_contract``) are forwarded via
    ``__getattr__`` so resource methods that reach through to the underlying
    client continue to work without any changes inside the resource files.
    """

    __slots__ = ("_client",)

    def __init__(self, client: "AkribesClient") -> None:
        self._client = client

    @property
    def _base_url(self) -> str:
        return self._client.base_url

    async def _request(self, method: str, url: str, **kw: Any):
        return await self._client._request(method, url, **kw)

    def __getattr__(self, name: str) -> Any:
        # Delegate attribute access to the underlying AkribesClient so that
        # resource methods that reach through to client-level state
        # (e.g. self._api.token, self._api.name, self._api.validate_contract,
        # self._api._sse_http, self._api._broken_scripts) keep working
        # without changes inside the individual resource files.
        return getattr(self._client, name)


class _ProjectApiClient(_ApiClient):
    """Adds a project context — ``_project_url`` for project-scoped endpoints.

    Extends :class:`_ApiClient` with a bound ``project_id`` and a
    ``_project_url`` helper that builds project-relative URLs.
    Resources that call ``self._project_url(...)`` receive one of these.
    """

    __slots__ = ("project_id",)

    def __init__(self, client: "AkribesClient", project_id: int) -> None:
        super().__init__(client)
        self.project_id = project_id

    def _project_url(self, *parts: str) -> str:
        segs = "/".join(str(p) for p in parts)
        base = f"{self._base_url}/projects/{self.project_id}"
        return f"{base}/{segs}" if segs else base


class Resource:
    """Resources that only need global URL access.

    Suitable for ``projects.py``, ``tokens.py``, ``me.py``, etc.
    Constructed with an :class:`_ApiClient` (or :class:`_ProjectApiClient`,
    since the latter is a subtype).
    """

    __slots__ = ("_api",)

    def __init__(self, api: _ApiClient) -> None:
        self._api = api

    @property
    def _base_url(self) -> str:
        return self._api._base_url

    async def _request(self, method: str, url: str, **kwargs: Any):
        return await self._api._request(method, url, **kwargs)


class ProjectResource(Resource):
    """Resources that call ``_project_url``. Receives a :class:`_ProjectApiClient`.

    Inherits ``_base_url`` and ``_request`` from :class:`Resource` so the
    global-URL methods on mixed resources (e.g. ``executions.get``,
    ``evals.cancel``) keep working unchanged.
    """

    _api: _ProjectApiClient  # type: ignore[assignment]

    def _project_url(self, *parts: str) -> str:
        return self._api._project_url(*parts)
