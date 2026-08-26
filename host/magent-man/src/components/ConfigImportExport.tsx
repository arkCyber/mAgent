import { useState, useCallback, useRef } from 'react';
import { useTranslation } from 'react-i18next';
import { useToast } from './Toast';
import { bleExportConfig } from '../hooks/useBle';

interface ImportedConfig {
  wifi_ssid?: string;
  llm_model?: string;
  hostname?: string;
}

interface ConfigImportExportProps {
  isConnected: boolean;
  onImport: (config: ImportedConfig) => void;
}

type ImportExportState = 'idle' | 'exporting' | 'importing' | 'success' | 'error';

export function ConfigImportExport({ isConnected, onImport }: ConfigImportExportProps) {
  const { t } = useTranslation();
  const toast = useToast();
  const [state, setState] = useState<ImportExportState>('idle');
  const [exportedData, setExportedData] = useState<string | null>(null);
  const fileInputRef = useRef<HTMLInputElement>(null);

  const handleExport = useCallback(async () => {
    if (!isConnected) {
      toast.warning(t('configImport.notConnected'));
      return;
    }

    setState('exporting');
    try {
      const result = await bleExportConfig();
      const dataStr = JSON.stringify(result, null, 2);
      setExportedData(dataStr);

      // Create download
      const blob = new Blob([dataStr], { type: 'application/json' });
      const url = URL.createObjectURL(blob);
      const a = document.createElement('a');
      a.href = url;
      a.download = `magent-config-${new Date().toISOString().slice(0, 10)}.json`;
      document.body.appendChild(a);
      a.click();
      document.body.removeChild(a);
      URL.revokeObjectURL(url);

      setState('success');
      toast.success(t('configImport.exportSuccess'));
    } catch (error) {
      setState('error');
      toast.error(t('configImport.exportFailed'));
    } finally {
      setTimeout(() => setState('idle'), 3000);
    }
  }, [isConnected, toast, t]);

  const handleImportClick = useCallback(() => {
    fileInputRef.current?.click();
  }, []);

  const handleFileChange = useCallback(
    (event: React.ChangeEvent<HTMLInputElement>) => {
      const file = event.target.files?.[0];
      if (!file) return;

      setState('importing');
      const reader = new FileReader();

      reader.onload = (e) => {
        try {
          // HARDENING (audit-2026-08 frontend): `e.target?.result` can be
          // `undefined` when the FileReader's target is null (e.g. synthetic
          // events from certain browsers or iframe contexts). `JSON.parse(undefined)`
          // throws `SyntaxError` rather than a descriptive business error.
          // We check explicitly so the error path is clear and not silently
          // swallowed by the generic catch below.
          const content = e.target?.result;
          if (content === undefined) {
            throw new Error('file_reader_empty');
          }
          const parsed = JSON.parse(content as string) as ImportedConfig;

          // Validate structure
          if (typeof parsed !== 'object') {
            throw new Error('invalid_json_structure');
          }

          setExportedData(JSON.stringify(parsed, null, 2));
          onImport(parsed);
          setState('success');
          toast.success(t('configImport.importSuccess'));
        } catch (error) {
          setState('error');
          toast.error(t('configImport.importFailed'));
        } finally {
          setTimeout(() => setState('idle'), 3000);
          // Reset file input
          if (fileInputRef.current) {
            fileInputRef.current.value = '';
          }
        }
      };

      reader.onerror = () => {
        setState('error');
        toast.error(t('configImport.importFailed'));
      };

      reader.readAsText(file);
    },
    [onImport, toast, t]
  );

  return (
    <div className="rounded-xl p-5 border" style={{ backgroundColor: 'var(--color-surface)', borderColor: 'var(--color-border)' }}>
      <div className="flex items-center gap-2 mb-4 pb-3 border-b border-gray-200 dark:border-gray-700">
        <span className="text-xl">💾</span>
        <h3 className="font-semibold" style={{ color: 'var(--color-text)' }}>{t('configImport.title')}</h3>
      </div>

      <div className="space-y-3">
        {/* Export */}
        <button
          onClick={handleExport}
          disabled={!isConnected || state === 'exporting'}
          className="w-full flex items-center gap-3 p-4 bg-blue-50 dark:bg-blue-900/20 hover:bg-blue-100 dark:hover:bg-blue-900/30 rounded-lg transition-colors disabled:opacity-50"
        >
          <span className="text-2xl">📤</span>
          <div className="text-left">
            <p className="font-medium text-sm">{t('configImport.export')}</p>
            <p className="text-xs text-gray-500 dark:text-gray-400">
              {t('configImport.exportDesc')}
            </p>
          </div>
          {state === 'exporting' && (
            <span className="ml-auto w-5 h-5 border-2 border-blue-500/30 border-t-blue-500 rounded-full animate-spin" />
          )}
        </button>

        {/* Import */}
        <button
          onClick={handleImportClick}
          disabled={!isConnected || state === 'importing'}
          className="w-full flex items-center gap-3 p-4 bg-green-50 dark:bg-green-900/20 hover:bg-green-100 dark:hover:bg-green-900/30 rounded-lg transition-colors disabled:opacity-50"
        >
          <span className="text-2xl">📥</span>
          <div className="text-left">
            <p className="font-medium text-sm">{t('configImport.import')}</p>
            <p className="text-xs text-gray-500 dark:text-gray-400">
              {t('configImport.importDesc')}
            </p>
          </div>
          {state === 'importing' && (
            <span className="ml-auto w-5 h-5 border-2 border-green-500/30 border-t-green-500 rounded-full animate-spin" />
          )}
        </button>

        <input
          ref={fileInputRef}
          type="file"
          accept=".json"
          onChange={handleFileChange}
          className="hidden"
        />
      </div>

      {/* Preview */}
      {exportedData && (
        <div className="mt-4">
          <details className="group">
            <summary className="cursor-pointer text-xs text-gray-500 dark:text-gray-400 hover:text-gray-700 dark:hover:text-gray-300">
              {t('configImport.preview')}
            </summary>
            <pre className="mt-2 p-3 bg-gray-100 dark:bg-gray-900 rounded-lg text-xs overflow-x-auto max-h-40 overflow-y-auto">
              {exportedData}
            </pre>
          </details>
        </div>
      )}

      {/* Status Messages */}
      {state === 'success' && (
        <div className="mt-4 p-3 bg-green-50 dark:bg-green-900/20 text-green-600 dark:text-green-400 rounded-lg text-sm text-center">
          ✓ {t('configImport.success')}
        </div>
      )}

      {state === 'error' && (
        <div className="mt-4 p-3 bg-red-50 dark:bg-red-900/20 text-red-600 dark:text-red-400 rounded-lg text-sm text-center">
          ✕ {t('configImport.error')}
        </div>
      )}
    </div>
  );
}
