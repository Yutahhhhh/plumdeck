import { useEffect, useState } from "react";
import { Bot, Check, KeyRound, Loader2, Trash2 } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { aiService, AiProviderId, AiSettings } from "@/services/ai";

const NONE = "none";

/**
 * Optional in-app AI (plan §2.4). Used only for genre cases the on-device
 * classifier is unsure about; MCP clients keep working as before.
 */
export function AiSettingsSection() {
  const [settings, setSettings] = useState<AiSettings | null>(null);
  const [keyInput, setKeyInput] = useState("");
  const [modelInput, setModelInput] = useState("");
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState<{ text: string; error: boolean } | null>(null);
  const [showDetails, setShowDetails] = useState(false);

  const load = async () => {
    try {
      setSettings(await aiService.getSettings());
    } catch {
      setMessage({ text: "AI の設定を読み込めませんでした", error: true });
    }
  };

  useEffect(() => {
    load();
  }, []);

  const current = settings?.providers.find((p) => p.id === settings.provider) ?? null;

  useEffect(() => {
    setModelInput(current?.model ?? "");
    setKeyInput("");
  }, [current?.id, current?.model]);

  const run = async (work: () => Promise<void>, done: string) => {
    setBusy(true);
    setMessage(null);
    try {
      await work();
      await load();
      setMessage({ text: done, error: false });
    } catch (error) {
      setMessage({ text: error instanceof Error ? error.message : "保存できませんでした", error: true });
    } finally {
      setBusy(false);
    }
  };

  const chooseProvider = (value: string) =>
    run(async () => {
      await aiService.updateSettings({ provider: value === NONE ? null : (value as AiProviderId) });
    }, value === NONE ? "AI を使わない設定にしました" : "プロバイダを切り替えました");

  const saveKey = () =>
    current &&
    run(async () => {
      await aiService.saveKey(current.id, keyInput.trim());
      setKeyInput("");
    }, "API キーをキーチェーンに保存しました");

  const deleteKey = () =>
    current && run(() => aiService.deleteKey(current.id), "API キーを削除しました");

  const saveModel = () =>
    current &&
    run(async () => {
      await aiService.updateSettings({ models: { [current.id]: modelInput.trim() } });
    }, "モデルを保存しました");

  return (
    <div className="space-y-4">
      <h3 className="text-sm font-medium text-muted-foreground flex items-center gap-2">
        <Bot className="h-4 w-4" />
        AI（任意）
      </h3>
      <div className="grid gap-4 p-4 border rounded-md bg-muted/20">
        <p className="text-xs text-muted-foreground leading-relaxed">
          自分の API キーを入れると、端末上の分類器が自信を持てなかった曲だけを AI に相談できます。
          送るのは曲名・アーティスト・BPM・音色の特徴・分類器の候補だけで、ファイルのタグのジャンルは送りません。
          キーが無くてもすべての機能が使えます。MCP クライアントからの利用はこれまでどおりです。
        </p>

        <div className="grid gap-2 max-w-md">
          <Label>使う AI</Label>
          <Select value={settings?.provider ?? NONE} onValueChange={chooseProvider} disabled={!settings || busy}>
            <SelectTrigger>
              <SelectValue placeholder="使わない" />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value={NONE}>使わない</SelectItem>
              {settings?.providers.map((p) => (
                <SelectItem key={p.id} value={p.id}>
                  {p.label}
                  {p.has_key ? "（キー保存済み）" : ""}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>

        {current && (
          <>
            <div className="grid gap-2 max-w-md">
              <Label className="flex items-center gap-2">
                <KeyRound className="h-3.5 w-3.5" />
                API キー
                {current.has_key && (
                  <span className="text-xs text-green-600 flex items-center gap-1">
                    <Check className="h-3 w-3" /> 保存済み
                  </span>
                )}
              </Label>
              <div className="flex gap-2">
                <Input
                  type="password"
                  autoComplete="off"
                  spellCheck={false}
                  value={keyInput}
                  onChange={(e) => setKeyInput(e.target.value)}
                  placeholder={current.has_key ? "新しいキーで置き換える" : "キーを貼り付け"}
                />
                <Button onClick={saveKey} disabled={busy || !keyInput.trim()}>
                  保存
                </Button>
                {current.has_key && (
                  <Button variant="outline" size="icon" onClick={deleteKey} disabled={busy} title="キーを削除">
                    <Trash2 className="h-4 w-4" />
                  </Button>
                )}
              </div>
              <p className="text-xs text-muted-foreground">キーは OS のキーチェーンに保存され、画面やログには出ません。</p>
            </div>

            <button
              type="button"
              className="text-xs text-muted-foreground underline underline-offset-2 justify-self-start"
              onClick={() => setShowDetails((v) => !v)}
            >
              {showDetails ? "詳細を閉じる" : "詳細"}
            </button>
            {showDetails && (
              <div className="grid gap-2 max-w-md">
                <Label>モデル</Label>
                <div className="flex gap-2">
                  <Input value={modelInput} onChange={(e) => setModelInput(e.target.value)} placeholder={current.default_model} />
                  <Button variant="outline" onClick={saveModel} disabled={busy || modelInput.trim() === current.model}>
                    保存
                  </Button>
                </div>
                <p className="text-xs text-muted-foreground">既定: {current.default_model}（空にすると既定に戻ります）</p>
              </div>
            )}
          </>
        )}

        {busy && <Loader2 className="h-4 w-4 animate-spin text-muted-foreground" />}
        {message && (
          <p className={`text-xs ${message.error ? "text-destructive" : "text-green-700"}`}>{message.text}</p>
        )}
      </div>
    </div>
  );
}
