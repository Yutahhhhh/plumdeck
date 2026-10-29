import { useEffect, useRef, useState } from "react";
import { Loader2, Pause, Play } from "lucide-react";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { playService, type RecordingEntry, type RecordingExportFormat, type RecordingFormatOption } from "@/services/play";
import { formatTime } from "./SoftwareDeck";

type Props = {
  recording: RecordingEntry | null;
  onClose: () => void;
  /** Stop deck/library audio before starting the recording preview. */
  onBeforePreview?: () => Promise<void>;
  /** 保存・破棄のどちらでも、録音一覧を取り直すために呼ぶ。 */
  onSettled: () => void;
};

type DialogFormat = RecordingExportFormat | "original";
type DialogFormatOption = Omit<RecordingFormatOption, "value"> & { value: DialogFormat };

/**
 * 録音を止めた直後に開く。狙いどおりのミックスが録れているかを実際に聴いて
 * 確かめてから、アーティスト名とミックス名を決めて保存する。名前はファイル名
 * にもなる。破棄を選べば音声ごと消える。
 */
export function RecordingSaveDialog({ recording, onClose, onBeforePreview, onSettled }: Props) {
  const [artist, setArtist] = useState("");
  const [title, setTitle] = useState("");
  const [pending, setPending] = useState<"save" | "discard" | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [playing, setPlaying] = useState(false);
  const [previewPending, setPreviewPending] = useState(false);
  const [position, setPosition] = useState(0);
  const [format, setFormat] = useState<DialogFormat>("original");
  const [formats, setFormats] = useState<DialogFormatOption[]>([]);
  const [formatsLoading, setFormatsLoading] = useState(false);
  const audio = useRef<HTMLAudioElement>(null);
  const formatsGeneration = useRef(0);
  const previewGeneration = useRef(0);
  const settling = useRef(false);

  useEffect(() => {
    if (!recording) return;
    setArtist(recording.artist ?? "");
    setTitle(recording.title ?? "");
    setError(null); setPending(null); setPlaying(false); setPreviewPending(false); setPosition(0);
    const sourceExtension = recording.filepath.split(".").pop()?.toLowerCase() ?? "";
    const sourceFormat = sourceExtension === "wav" || sourceExtension === "flac" ? sourceExtension : "original";
    const sourceOption: DialogFormatOption = {
      value: sourceFormat,
      extension: sourceExtension ? `.${sourceExtension}` : "",
      label: `${sourceExtension.toUpperCase() || "元の形式"}（元の形式）`,
    };
    setFormat(sourceFormat);
    setFormats([sourceOption]);
    setFormatsLoading(true);
    const generation = ++formatsGeneration.current;
    void playService.recordingFormats().then(({ formats: available }) => {
      if (formatsGeneration.current !== generation) return;
      setFormats(available.some((option) => option.value === sourceFormat) ? available : [sourceOption, ...available]);
    }).catch((cause: unknown) => {
      if (formatsGeneration.current === generation) setError(cause instanceof Error ? cause.message : String(cause));
    }).finally(() => {
      if (formatsGeneration.current === generation) setFormatsLoading(false);
    });
    return () => { formatsGeneration.current += 1; previewGeneration.current += 1; audio.current?.pause(); };
  }, [recording]);

  if (!recording) return null;
  const durationMs = recording.duration_ms || 0;

  const settle = async (task: () => Promise<unknown>, kind: "save" | "discard") => {
    if (settling.current) return;
    settling.current = true;
    previewGeneration.current += 1;
    setPending(kind); setError(null);
    audio.current?.pause();
    try {
      await task();
      onSettled();
      onClose();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      settling.current = false;
      setPending(null);
    }
  };

  const close = () => {
    previewGeneration.current += 1;
    audio.current?.pause();
    setPreviewPending(false);
    onClose();
  };

  const togglePreview = async () => {
    const element = audio.current;
    if (!element || previewPending) return;
    if (!element.paused) { element.pause(); return; }
    const generation = ++previewGeneration.current;
    setPreviewPending(true); setError(null);
    try {
      await onBeforePreview?.();
      if (previewGeneration.current !== generation || audio.current !== element) return;
      await element.play();
    } catch (cause) {
      if (previewGeneration.current === generation) setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      if (previewGeneration.current === generation) setPreviewPending(false);
    }
  };

  const save = () => settle(() => playService.nameRecording(
    recording.id, artist.trim(), title.trim(), format === "original" ? undefined : format,
  ), "save");
  const selectedFormat = formats.find((option) => option.value === format);

  return <Dialog open onOpenChange={(open) => { if (!open && !pending) close(); }}>
    <DialogContent className="dj-recording-dialog border-[#34383f] bg-[#202327] text-[#e2e5e9]"
      onEscapeKeyDown={(event) => { if (pending) event.preventDefault(); }}
      onPointerDownOutside={(event) => { if (pending) event.preventDefault(); }}>
      <DialogHeader>
        <DialogTitle className="text-[#f1f3f5]">録音を保存</DialogTitle>
        <DialogDescription className="text-[#aeb3bb]">再生するとデッキを一時停止し、システムの標準出力からプレビューします。確認して名前を付けてください。破棄すると音声ファイルごと削除されます。</DialogDescription>
      </DialogHeader>

      <div className="dj-recording-preview">
        <button type="button" className="dj-recording-play" aria-label={playing ? "プレビューを一時停止" : "プレビューを再生"}
          disabled={Boolean(pending) || previewPending} onClick={() => void togglePreview()}>{previewPending ? <Loader2 className="animate-spin" /> : playing ? <Pause /> : <Play />}</button>
        <input type="range" aria-label="再生位置" min={0} max={Math.max(1, durationMs)} step={100} value={Math.min(position, durationMs)}
          disabled={Boolean(pending)}
          onChange={(event) => { const next = Number(event.target.value); setPosition(next); if (audio.current) audio.current.currentTime = next / 1000; }} />
        <span className="dj-num">{formatTime(position)} / {formatTime(durationMs)}</span>
      </div>
      <audio ref={audio} src={playService.recordingAudioUrl(recording.id)} preload="metadata"
        onPlay={() => setPlaying(true)} onPause={() => setPlaying(false)} onEnded={() => setPlaying(false)}
        onTimeUpdate={(event) => setPosition(event.currentTarget.currentTime * 1000)}
        onError={() => setError("録音を再生できません。ファイルが移動または削除された可能性があります。")} />

      <label className="dj-recording-field"><span>アーティスト名</span>
        <input autoFocus aria-label="アーティスト名" value={artist} maxLength={200} disabled={Boolean(pending)}
          placeholder="任意" onChange={(event) => setArtist(event.target.value)} /></label>
      <label className="dj-recording-field"><span>ミックス名</span>
        <input aria-label="ミックス名" value={title} maxLength={200} disabled={Boolean(pending)}
          placeholder="必須" onChange={(event) => setTitle(event.target.value)}
          onKeyDown={(event) => { if (event.key === "Enter" && title.trim() && formats.length) { event.preventDefault(); void save(); } }} /></label>
      <label className="dj-recording-field"><span>保存形式{formatsLoading ? " · 変換形式を確認中…" : ""}</span>
        <Select value={format} disabled={Boolean(pending) || !formats.length} onValueChange={(value) => setFormat(value as DialogFormat)}>
          <SelectTrigger aria-label="保存形式" className="border-[#34383f] bg-[#17191c] text-[#d0d4da] [&>span]:!text-xs [&>span]:!text-[#d0d4da]"><SelectValue /></SelectTrigger>
          <SelectContent className="border-[#3a3e45] bg-[#202327] text-[#d0d4da]">{formats.map((option) => <SelectItem className="focus:bg-[#2b4569] focus:text-white" key={option.value} value={option.value}>{option.label}</SelectItem>)}</SelectContent>
        </Select>
      </label>
      <p className="dj-recording-filename">保存名: <b>{(artist.trim() ? `${artist.trim()} - ` : "") + (title.trim() || "（ミックス名）")}{selectedFormat?.extension ?? `.${format}`}</b></p>

      {error && <div className="dj-recording-error" role="alert">{error}</div>}

      <DialogFooter className="dj-recording-actions">
        <button type="button" className="is-destructive" disabled={Boolean(pending)}
          onClick={() => void settle(() => playService.discardRecording(recording.id), "discard")}>
          {pending === "discard" ? <Loader2 className="animate-spin" /> : null}破棄
        </button>
        <button type="button" disabled={Boolean(pending)} onClick={close}>あとで</button>
        <button type="button" className="is-primary" disabled={Boolean(pending) || !title.trim() || !formats.length}
          onClick={() => void save()}>
          {pending === "save" ? <Loader2 className="animate-spin" /> : null}保存
        </button>
      </DialogFooter>
    </DialogContent>
  </Dialog>;
}
