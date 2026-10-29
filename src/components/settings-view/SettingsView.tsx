import { useState, useEffect } from "react";
import { FolderSetting } from "./FolderSetting";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Card,
  CardContent,
  CardHeader,
  CardTitle,
  CardDescription,
} from "@/components/ui/card";
import { Separator } from "@/components/ui/separator";
import {
  Settings as SettingsIcon,
  AlertCircle,
  FileSpreadsheet,
  Globe,
} from "lucide-react";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import {
  settingsService,
  LibraryAnalysisResult,
  MetadataAnalysisResult,
} from "@/services/settings";
import { ImportSection } from "./ImportSection";
import { LibraryImportDialog } from "./LibraryImportDialog";
import { MetadataImportDialog } from "./MetadataImportDialog";
import { useTheme } from "@/components/theme-provider";
import { setKeyNotation, setTrafficLightLevel2, useKeyDisplay } from "@/lib/key-display";
import type { KeyNotationId } from "@/lib/keys.generated";
import { GridReanalysis } from "./GridReanalysis";
import { AiSettingsSection } from "./AiSettingsSection";
import { downloadFile } from "@/lib/download";
import {
  normalizeAnalysisProfile,
  rememberAnalysisProfile,
} from "@/services/analysis-profile";

const supportsLightAnalysis = typeof navigator !== "undefined" && /Windows/i.test(navigator.userAgent);

export function SettingsView() {
  const { theme, setTheme } = useTheme();
  const keyDisplay = useKeyDisplay();
  const [settings, setSettings] = useState<{ [key: string]: string }>({});
  const [status, setStatus] = useState<string>("");
  const [isError, setIsError] = useState(false);

  // Import State
  const [importModalOpen, setImportModalOpen] = useState(false);
  const [isAnalyzing, setIsAnalyzing] = useState(false);
  const [analysisResult, setAnalysisResult] = useState<LibraryAnalysisResult | null>(
    null
  );
  const [isImporting, setIsImporting] = useState(false);

  // Metadata Import State
  const [metadataModalOpen, setMetadataModalOpen] = useState(false);
  const [metadataAnalysis, setMetadataAnalysis] =
    useState<MetadataAnalysisResult | null>(null);

  // Export State
  const [exportingSectionId, setExportingSectionId] = useState<string | null>(null);

  useEffect(() => {
    fetchSettings();
  }, []);

  const fetchSettings = async () => {
    try {
      const data = await settingsService.getAll();
      const requestedProfile = normalizeAnalysisProfile(data.analysis_profile);
      const analysisProfile = rememberAnalysisProfile(
        !supportsLightAnalysis && requestedProfile === "light" ? "auto" : requestedProfile,
      );
      if (analysisProfile !== requestedProfile) {
        await settingsService.save("analysis_profile", analysisProfile);
      }
      data.analysis_profile = analysisProfile;
      setSettings(data);
    } catch (e: any) {
      console.error("Failed to fetch settings", e);
      setStatus(`Connection Error: ${e.message}`);
      setIsError(true);
    }
  };

  const saveSetting = async (key: string, value: string) => {
    setStatus("Saving...");
    setIsError(false);
    try {
      await settingsService.save(key, value);
      if (key === "analysis_profile") rememberAnalysisProfile(value);
      setSettings((prev) => ({ ...prev, [key]: value }));
      setStatus(`Saved ${key}`);
      setTimeout(() => setStatus(""), 2000);
    } catch (e: any) {
      console.error("Failed to save setting", e);
      setStatus(`Error: ${e.message}`);
      setIsError(true);
    }
  };

  const handleAnalyze = async (file: File, type: 'library' | 'metadata') => {
    setIsAnalyzing(true);
    if (type === 'library') { setImportModalOpen(true); setAnalysisResult(null); }
    if (type === 'metadata') { setMetadataModalOpen(true); setMetadataAnalysis(null); }

    try {
      const result = await settingsService.analyzeImport(file, type);
      if (type === 'library') setAnalysisResult(result as LibraryAnalysisResult);
      if (type === 'metadata') setMetadataAnalysis(result as MetadataAnalysisResult);
    } catch (error: any) {
      console.error(error);
      if (type === 'library') setImportModalOpen(false);
      if (type === 'metadata') setMetadataModalOpen(false);

      setStatus(`${type} Analysis Failed: ${error.message}`);
      setIsError(true);
    } finally {
      setIsAnalyzing(false);
    }
  };

  const handleExecuteImport = async () => {
    if (!analysisResult) return;
    setIsImporting(true);

    try {
      const payload = {
        new_tracks: analysisResult.new_tracks,
        path_updates: analysisResult.path_updates,
      };

      const data = await settingsService.executeImport(payload, 'library');

      setStatus(data.message);
      setIsError(false);
      setImportModalOpen(false);
    } catch (error: any) {
      console.error(error);
      setStatus(`Import Failed: ${error.message}`);
      setIsError(true);
    } finally {
      setIsImporting(false);
    }
  };

  const handleExecuteMetadataImport = async () => {
    if (!metadataAnalysis) return;
    setIsImporting(true);

    try {
      const payload = {
        updates: metadataAnalysis.updates,
      };

      const data = await settingsService.executeImport(payload, 'metadata');

      setStatus(data.message);
      setIsError(false);
      setMetadataModalOpen(false);
    } catch (error: any) {
      console.error(error);
      setStatus(`Metadata Import Failed: ${error.message}`);
      setIsError(true);
    } finally {
      setIsImporting(false);
    }
  };

  // Helper to safely update settings state
  const updateLocalSetting = (key: string, value: string) => {
    setSettings((prev) => ({ ...prev, [key]: value }));
  };

  const handleExport = async (sectionId: string, url: string, filename: string) => {
    setExportingSectionId(sectionId);
    try {
      await downloadFile(url, filename);
    } finally {
      setExportingSectionId(null);
    }
  };

  const importSections = [
    {
      id: 'library',
      title: 'Data Management (CSV)',
      icon: <FileSpreadsheet className="h-4 w-4" />,
      description: 'Export your library to back up analysis data. Import to restore or migrate data (supports path tracking).',
      onExport: () => handleExport('library', settingsService.getExportUrl('library'), 'plumdeck_library.csv'),
      onFileSelect: (file: File) => handleAnalyze(file, 'library'),
      variant: 'outline' as const,
      exportLabel: 'Export Library to CSV',
      importLabel: 'Import / Restore from CSV',
      isExporting: exportingSectionId === 'library'
    },
    {
      id: 'metadata',
      title: 'Metadata Management (Lightweight CSV)',
      icon: <Globe className="h-4 w-4" />,
      description: 'Use this to bulk update track metadata (Title, Artist, Genre, Verified status) externally. This will NOT affect analysis data.',
      onExport: () => handleExport('metadata', settingsService.getExportUrl('metadata'), 'plumdeck_metadata.csv'),
      onFileSelect: (file: File) => handleAnalyze(file, 'metadata'),
      variant: 'secondary' as const,
      exportLabel: 'Export Metadata CSV',
      importLabel: 'Update Metadata from CSV',
      isExporting: exportingSectionId === 'metadata'
    }
  ];

  return (
    <div className="p-4 h-full overflow-y-auto">
      <Card>
        <CardHeader>
          <CardTitle className="flex items-center gap-2">
            <SettingsIcon className="h-5 w-5" />
            Application Settings
          </CardTitle>
          <CardDescription>
            Manage local application configuration and library data.
          </CardDescription>
        </CardHeader>
        <CardContent className="space-y-6">
          {/* Status Message Area */}
          {status && (
            <div
              className={`text-sm p-2 rounded mb-2 flex items-center gap-2 ${
                isError
                  ? "bg-destructive/10 text-destructive"
                  : "bg-green-100 text-green-700"
              }`}
            >
              {isError && <AlertCircle className="h-4 w-4" />}
              {status}
            </div>
          )}

          {/* Configuration Section */}
          <div className="space-y-4">
            <h3 className="text-sm font-medium text-muted-foreground">
              General Configuration
            </h3>

            <div className="space-y-2">
              <Label>録音の保存形式</Label>
              <Select
                value={settings.recording_format || "WAV"}
                onValueChange={(value) => void saveSetting("recording_format", value)}
              >
                <SelectTrigger>
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  {/* 実際に書き出せることを 1 形式ずつ録音して確認したものだけ。 */}
                  <SelectItem value="WAV">WAV（可逆・最大サイズ）</SelectItem>
                  <SelectItem value="AIFF">AIFF（可逆）</SelectItem>
                  <SelectItem value="FLAC">FLAC（可逆・圧縮）</SelectItem>
                  <SelectItem value="MP3">MP3（非可逆）</SelectItem>
                  <SelectItem value="OGG">OGG Vorbis（非可逆）</SelectItem>
                </SelectContent>
              </Select>
              <p className="text-xs text-muted-foreground">
                次の録音から反映されます。AAC はこのビルドのエンジンでは書き出せないため除いています。
              </p>
            </div>

            <div className="space-y-2">
              <Label htmlFor="recording-directory">録音の保存先</Label>
              <FolderSetting id="recording-directory" value={settings.recording_directory ?? ""} placeholder="標準の録音フォルダー" onSelect={(path) => saveSetting("recording_directory", path)} />
              <p className="text-xs text-muted-foreground">選択すると保存され、次の録音から反映されます。</p>
            </div>

            <div className="space-y-2">
              <Label>Theme</Label>
              <Select value={theme} onValueChange={(val: any) => setTheme(val)}>
                <SelectTrigger>
                  <SelectValue placeholder="Select theme" />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="light">Light</SelectItem>
                  <SelectItem value="dark">Dark</SelectItem>
                  <SelectItem value="system">System</SelectItem>
                </SelectContent>
              </Select>
            </div>

            <div className="space-y-2">
              <Label htmlFor="key-notation">キーの表記</Label>
              <Select value={keyDisplay.notation} onValueChange={(value) => setKeyNotation(value as KeyNotationId)}>
                <SelectTrigger id="key-notation">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="alphanumeric">Alphanumeric（8A）</SelectItem>
                  <SelectItem value="classic">Classic（Am）</SelectItem>
                  <SelectItem value="open_key">Open Key（1m）</SelectItem>
                </SelectContent>
              </Select>
              <label className="flex items-center gap-2 text-xs text-muted-foreground">
                <input type="checkbox" checked={keyDisplay.level2} onChange={(event) => setTrafficLightLevel2(event.target.checked)} />
                Traffic Light で対角・+2 も黄色で示す
              </label>
            </div>

            <div className="space-y-2">
              <Label htmlFor="analysis-profile">解析方法</Label>
              <Select
                value={normalizeAnalysisProfile(settings.analysis_profile)}
                onValueChange={(value) =>
                  void saveSetting(
                    "analysis_profile",
                    !supportsLightAnalysis && value === "light" ? "auto" : normalizeAnalysisProfile(value),
                  )
                }
              >
                <SelectTrigger id="analysis-profile">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="auto">自動（推奨）</SelectItem>
                  <SelectItem value="light" disabled={!supportsLightAnalysis}>軽量（Windowsのみ・低負荷）</SelectItem>
                  <SelectItem value="full">詳細</SelectItem>
                </SelectContent>
              </Select>
              <p className="text-xs text-muted-foreground">
                低スペックのWindowsでは「軽量」を選んでください。ビート操作・SYNC・波形・類似曲検索に必要な情報を保ち、音色や曲の特徴を調べる区間を短くして負荷を抑えます。「自動」は詳細解析から始め、時間がかかる場合だけ軽量へ切り替わります。Macは詳細解析を使用します。
              </p>
            </div>

            <GridReanalysis />

            <div className="space-y-2">
              <Label htmlFor="root_path">Default Root Path</Label>
              <FolderSetting id="root_path" value={settings.root_path ?? ""} placeholder="音楽フォルダーを選択" onSelect={(path) => saveSetting("root_path", path)} />
            </div>

            <div className="space-y-2">
              <Label htmlFor="setlist_default_length">
                セットリスト自動生成のデフォルト曲数
              </Label>
              <div className="flex gap-2">
                <Input
                  id="setlist_default_length"
                  type="number"
                  min={1}
                  placeholder="10"
                  value={settings["setlist_default_length"] ?? ""}
                  onChange={(e) =>
                    updateLocalSetting(
                      "setlist_default_length",
                      e.target.value
                    )
                  }
                  className="w-32"
                />
                <Button
                  onClick={() =>
                    saveSetting(
                      "setlist_default_length",
                      settings["setlist_default_length"] || "10"
                    )
                  }
                >
                  Save
                </Button>
              </div>
              <p className="text-xs text-muted-foreground">
                MCP の generate_auto_setlist
                で曲数未指定時に使われるデフォルト曲数。
              </p>
            </div>
          </div>

          <Separator />

          {/* Optional in-app AI (keys in the OS keychain) */}
          <AiSettingsSection />

          {importSections.map((section) => (
            <div key={section.id}>
              <Separator className="my-4" />
              <ImportSection {...section} />
            </div>
          ))}
        </CardContent>
      </Card>

      <LibraryImportDialog
        open={importModalOpen}
        onOpenChange={setImportModalOpen}
        analysis={analysisResult}
        isAnalyzing={isAnalyzing}
        isImporting={isImporting}
        onExecute={handleExecuteImport}
      />

      <MetadataImportDialog
        open={metadataModalOpen}
        onOpenChange={setMetadataModalOpen}
        analysis={metadataAnalysis}
        isAnalyzing={isAnalyzing}
        isImporting={isImporting}
        onExecute={handleExecuteMetadataImport}
      />
    </div>
  );
}
