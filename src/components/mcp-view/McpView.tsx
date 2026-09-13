import { useEffect, useState } from "react";
import {
  Card,
  CardContent,
  CardHeader,
  CardTitle,
  CardDescription,
} from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { Separator } from "@/components/ui/separator";
import { Bot, Copy, RefreshCw, Wrench, CheckCircle2, XCircle } from "lucide-react";
import { mcpService, McpInfo } from "@/services/mcp";
import { getErrorDetail } from "@/services/api-client";
import { toast } from "@/components/ui/toast";

function CopyableCode({ value }: { value: string }) {
  return (
    <div className="flex items-center gap-2 rounded-md border bg-muted/40 px-3 py-2 font-mono text-sm">
      <code className="flex-1 overflow-x-auto whitespace-pre">{value}</code>
      <Button
        variant="ghost"
        size="icon"
        className="h-7 w-7 shrink-0"
        onClick={() => {
          navigator.clipboard.writeText(value);
          toast.success("コピーしました");
        }}
      >
        <Copy className="h-3.5 w-3.5" />
      </Button>
    </div>
  );
}

export function McpView() {
  const [info, setInfo] = useState<McpInfo | null>(null);
  const [loading, setLoading] = useState(true);
  const [status, setStatus] = useState<"idle" | "ok" | "error">("idle");

  const load = async () => {
    setLoading(true);
    try {
      const data = await mcpService.getInfo();
      setInfo(data);
      setStatus("ok");
    } catch (e) {
      setStatus("error");
      toast.error("MCPサーバー情報の取得に失敗しました", getErrorDetail(e));
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    load();
  }, []);

  const configSnippet = info
    ? JSON.stringify(
        {
          mcpServers: {
            plumdeck: {
              type: "http",
              url: info.url,
            },
          },
        },
        null,
        2
      )
    : "";

  return (
    <div className="h-full overflow-y-auto p-6 space-y-6">
      <div className="flex items-center justify-between">
        <div>
          <h1 className="text-2xl font-bold flex items-center gap-2">
            <Bot className="h-6 w-6" />
            MCP
          </h1>
          <p className="text-muted-foreground text-sm mt-1">
            plumdeck の楽曲管理と Junction を、Claude Desktop / Claude Code など外部の
            MCP クライアントから操作できます。チャットで依頼し、結果と現在の状態をこのアプリでも確認します。
          </p>
        </div>
        <div className="flex items-center gap-2">
          {status === "ok" && (
            <Badge variant="outline" className="gap-1 text-green-600 border-green-600/40">
              <CheckCircle2 className="h-3.5 w-3.5" /> 接続可能
            </Badge>
          )}
          {status === "error" && (
            <Badge variant="outline" className="gap-1 text-destructive border-destructive/40">
              <XCircle className="h-3.5 w-3.5" /> 接続不可
            </Badge>
          )}
          <Button variant="outline" size="sm" onClick={load} disabled={loading}>
            <RefreshCw className={`h-4 w-4 mr-1 ${loading ? "animate-spin" : ""}`} />
            再確認
          </Button>
        </div>
      </div>

      <Card>
        <CardHeader>
          <CardTitle className="text-base">接続情報</CardTitle>
          <CardDescription>
            plumdeck のバックエンドが起動している間、以下の URL で MCP サーバー (Streamable HTTP) が待ち受けます。
          </CardDescription>
        </CardHeader>
        <CardContent className="space-y-2">
          <CopyableCode value={info?.url ?? "..."} />
          <p className="text-xs text-muted-foreground">
            ポート番号はアプリの起動設定に連動します（開発時は 8001、本番ビルドは 48123 が既定）。
          </p>
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle className="text-base">設定方法</CardTitle>
          <CardDescription>
            Streamable HTTP に対応する MCP クライアントの設定に、以下を追記してください。
          </CardDescription>
        </CardHeader>
        <CardContent className="space-y-3">
          <pre className="rounded-md border bg-muted/40 p-3 text-xs overflow-x-auto">
            {configSnippet}
          </pre>
          <p className="text-xs text-muted-foreground">
            Claude Code の場合は <code>claude mcp add --transport http plumdeck {info?.url}</code>{" "}
            でも登録できます。設定後、クライアントを再起動すると plumdeck のツールが利用可能になります。
          </p>
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle className="text-base flex items-center gap-2">
            <Wrench className="h-4 w-4" />
            利用可能なツール {info && <Badge variant="secondary">{info.tool_count}</Badge>}
          </CardTitle>
          <CardDescription>
            楽曲検索、セットリスト、分類、歌詞に加え、Junction の接続・DJ順・引き継ぎ・Program・試聴・Liveモニターを操作できます。
          </CardDescription>
        </CardHeader>
        <CardContent className="space-y-4">
          <div className="rounded-md border border-amber-500/30 bg-amber-500/5 px-3 py-2 text-xs text-muted-foreground">
            Junction操作では、最初にAIから音声エンジンの準備と状態取得を実行できます。参加承認、開始、引き継ぎ、録音、退出、終了は実際のセッションへ反映されるため、AIへ対象と意図を明示してください。Junction Liveのデッキ割り当ては表示専用で、会場音声はProgram出力から流れます。
          </div>
          <div className="grid gap-2">
            {info?.tools.map((tool, i) => (
              <div key={tool.name}>
                {i > 0 && <Separator className="my-2" />}
                <div className="flex flex-col gap-0.5">
                  <span className="font-mono text-sm font-medium">{tool.name}</span>
                  <span className="text-xs text-muted-foreground">{tool.description}</span>
                </div>
              </div>
            ))}
            {!loading && info?.tools.length === 0 && (
              <p className="text-sm text-muted-foreground">ツールが見つかりませんでした。</p>
            )}
          </div>
        </CardContent>
      </Card>
    </div>
  );
}
