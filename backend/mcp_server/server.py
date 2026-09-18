"""plumdeck の MCP サーバーを構築するエントリポイント。
tools 配下の各モジュールをインポートすることで @mcp.tool() のツール登録が走る。
"""

from mcp_server.instance import mcp

from mcp_server.tools import tracks  # noqa: F401
from mcp_server.tools import setlists  # noqa: F401
from mcp_server.tools import genres  # noqa: F401
from mcp_server.tools import lyrics  # noqa: F401
from mcp_server.tools import analysis  # noqa: F401
from mcp_server.tools import wordplay  # noqa: F401
from mcp_server.tools import junction  # noqa: F401
from mcp_server.tools import assist  # noqa: F401

MCP_HTTP_PATH = "/mcp"


def build_mcp_asgi_app(transport_security=None):
    """FastAPI の主アプリにマウントする Streamable HTTP ASGI アプリを構築する。
    呼び出すたびに新しい session_manager が内部に生成される
    (StreamableHTTPSessionManager は run() 完了後に再利用できないため、
    アプリの lifespan が複数回走るケース [例: テストスイート] では毎回作り直す必要がある)。

    transport_security を渡すと DNS リバインディング保護の設定を上書きできる
    (テスト環境の Host ヘッダー検証を緩和するために使用)。
    """
    return mcp.streamable_http_app(
        streamable_http_path=MCP_HTTP_PATH,
        transport_security=transport_security,
    )


class MCPAppHolder:
    """マウント先の ASGI アプリを実行時に差し替え可能にするラッパー。
    FastAPI の lifespan ごとに build_mcp_asgi_app() で新しい session_manager を
    生成し直すため、Mount ルートの実体をここで間接参照させる。
    """

    def __init__(self, transport_security=None):
        self.transport_security = transport_security
        self.app = build_mcp_asgi_app(transport_security)

    def refresh(self):
        self.app = build_mcp_asgi_app(self.transport_security)

    async def __call__(self, scope, receive, send):
        await self.app(scope, receive, send)


mcp_app_holder = MCPAppHolder()
