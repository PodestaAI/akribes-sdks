from __future__ import annotations

from urllib.parse import quote

from akribes_sdk.models import (
    McpDriftResult,
    McpHealth,
    McpRefreshResult,
    McpServerSummary,
    McpToolSummary,
)
from akribes_sdk._parsers import (
    parse_mcp_server_summary,
    parse_mcp_tool_summary,
    parse_mcp_health,
    parse_mcp_refresh_result,
    parse_mcp_drift_result,
)
from akribes_sdk.resources._base import ProjectResource


class Mcp(ProjectResource):
    """MCP server/tool discovery for a project."""

    async def list_servers(self) -> list[McpServerSummary]:
        res = await self._request("GET", self._project_url("mcp", "servers"))
        return [parse_mcp_server_summary(s) for s in res.json()]

    async def list_tools(self) -> list[McpToolSummary]:
        res = await self._request("GET", self._project_url("mcp", "tools"))
        return [parse_mcp_tool_summary(t) for t in res.json()]

    async def health(self, alias: str) -> McpHealth:
        res = await self._request(
            "GET",
            self._project_url("mcp", "servers", quote(alias, safe=""), "health"),
        )
        return parse_mcp_health(res.json())

    async def refresh(self, alias: str) -> McpRefreshResult:
        """Force a fresh ``tools/list`` against the remote MCP server and
        update the pinned schema in the DB. Returns the new tool count."""
        res = await self._request(
            "POST",
            self._project_url("mcp", "servers", quote(alias, safe=""), "refresh"),
            json={},
        )
        return parse_mcp_refresh_result(res.json())

    async def drift(self, alias: str) -> McpDriftResult:
        """Compare the pinned schema against the remote server's live
        ``tools/list`` and report added/removed tool names."""
        res = await self._request(
            "GET",
            self._project_url("mcp", "servers", quote(alias, safe=""), "drift"),
        )
        return parse_mcp_drift_result(res.json())
