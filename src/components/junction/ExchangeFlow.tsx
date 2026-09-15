import { useRef, useState } from 'react';
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from '@/components/ui/dropdown-menu';
import { primaryExchangeAction, type ExchangeActionId, type ExchangeGuidance } from '@/services/junction/exchange-actions';
import { readExchangeClipboard } from '@/services/junction/client';

interface Props {
  guidance: ExchangeGuidance;
  host: boolean;
  connected?: boolean;
  /** Packets travel through share-musics: no invite/response steps for the DJ to follow. */
  automatic?: boolean;
  busy: boolean;
  error?: string;
  onAction: (action: ExchangeActionId) => void;
  onImport: (text: string) => Promise<void>;
}
/** One next action; secondary actions dismiss on selection, outside click and Escape. */
export function ExchangeFlow({guidance, host, connected, automatic, busy, error, onAction, onImport}: Props) {
  const [textOpen, setTextOpen] = useState(false);
  const [text, setText] = useState('');
  const [inputError, setInputError] = useState('');
  const input = useRef<HTMLTextAreaElement>(null);
  const focusInput = useRef(false);
  const primary = primaryExchangeAction(guidance);
  const secondary = guidance.actions.filter((action) => action.id !== primary?.id);
  const act = (id: ExchangeActionId) => {
    if (id === 'paste_answer' || id === 'paste_invite') {
      focusInput.current = true;
      setTextOpen(true);
      requestAnimationFrame(() => input.current?.focus());
    } else onAction(id);
  };
  const submit = async () => {
    setInputError('');
    try { await onImport(text.trim()); setText(''); setTextOpen(false); }
    catch (cause) { setInputError(cause instanceof Error ? cause.message : String(cause)); }
  };
  return <div className="junction-exchange-flow" aria-busy={busy}>
    {!connected && <>
      {!automatic && <ol className="junction-exchange-steps" aria-label="接続までの手順">
        {['招待', '返答', '接続'].map((label, index) => <li key={label} aria-current={guidance.step === index + 1 ? 'step' : undefined} className={index + 1 < guidance.step ? 'is-done' : ''}>
          <span>{index + 1 < guidance.step ? '✓' : index + 1}</span>{label}
        </li>)}
      </ol>}
      <p className="junction-exchange-next" role="status">{guidance.headline}</p>
      {guidance.hint && <p className="junction-card-note">{guidance.hint}</p>}
    </>}
    <div className="junction-card-actions">
      {!textOpen && primary && <button type="button" className="junction-btn junction-btn-primary" disabled={busy} onClick={() => act(primary.id)}>{primary.label}</button>}
      {secondary.length > 0 && <DropdownMenu modal={false}>
        <DropdownMenuTrigger asChild><button type="button" className="junction-btn junction-btn-default" aria-label="接続のその他の操作" disabled={busy}>その他 •••</button></DropdownMenuTrigger>
        <DropdownMenuContent className="junction-exchange-menu" align="end" onKeyDown={(event) => event.stopPropagation()} onEscapeKeyDown={(event) => event.stopPropagation()} onCloseAutoFocus={(event) => {
          if (focusInput.current) { event.preventDefault(); input.current?.focus(); focusInput.current = false; }
        }}>
          {secondary.map((action) => <DropdownMenuItem key={action.id} disabled={busy} className={action.intent === 'danger' ? 'junction-menu-danger' : ''} onSelect={() => act(action.id)}>{action.label}</DropdownMenuItem>)}
        </DropdownMenuContent>
      </DropdownMenu>}
    </div>
    {textOpen && <div className="junction-inline-exchange">
      <label>{host ? 'このDJから届いた返答' : '管理DJから届いた新しい招待'}
        <textarea ref={input} value={text} onChange={(event) => setText(event.target.value)} rows={3} maxLength={131072} placeholder="PLUMDECK-JUNCTION-…" />
      </label>
      <div className="junction-card-actions">
        <button type="button" className="junction-btn junction-btn-default" disabled={busy} onClick={() => void readExchangeClipboard().then(setText).catch((cause) => setInputError(String(cause)))}>貼り付け</button>
        <button type="button" className="junction-btn junction-btn-primary" disabled={busy || !text.trim()} onClick={() => void submit()}>{host ? '返答を確認' : '招待を取り込む'}</button>
        <button type="button" className="junction-btn junction-btn-default" disabled={busy} onClick={() => setTextOpen(false)}>閉じる</button>
      </div>
    </div>}
    {(inputError || error || guidance.error) && <p className="junction-card-error" role="alert">{inputError || error || guidance.error}</p>}
  </div>;
}
