import React, { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open, save } from "@tauri-apps/plugin-dialog";
import { useEscapeKey } from "../hooks/useEscapeKey";
import { useAppStore } from "../stores/appStore";
import { useSessionStore } from "../stores/sessionStore";
import { useSettingsStore, ALLOWED_TERMINAL_FONTS } from "../stores/settingsStore";
import {
  useHighlightStore,
  type HighlightProfile,
  type HighlightRule,
} from "../stores/highlightStore";
import { useToastStore } from "../stores/toastStore";
import { HighlightProfileDialog } from "./HighlightProfileDialog";
import { CustomSelect } from "./CustomSelect";
import { ConfirmDialog } from "./ConfirmDialog";

// eslint-disable-next-line @typescript-eslint/no-empty-function
const noop = (): void => {};

const AVAILABLE_LANGUAGES = [
  { code: "en", label: "English" },
  { code: "de", label: "Deutsch" },
  { code: "es", label: "Espa\u00f1ol" },
  { code: "fr", label: "Fran\u00e7ais" },
  { code: "it", label: "Italiano" },
  { code: "ja", label: "\u65e5\u672c\u8a9e" },
  { code: "ko", label: "\ud55c\uad6d\uc5b4" },
  { code: "nl", label: "Nederlands" },
  { code: "pl", label: "Polski" },
  { code: "pt", label: "Portugu\u00eas" },
  { code: "ru", label: "\u0420\u0443\u0441\u0441\u043a\u0438\u0439" },
  { code: "sv", label: "Svenska" },
  { code: "tr", label: "T\u00fcrk\u00e7e" },
  { code: "zh", label: "\u4e2d\u6587" },
];

const UI_SCALE_OPTIONS = [75, 80, 90, 100, 110, 120, 125, 150];

const FONT_OPTIONS = ALLOWED_TERMINAL_FONTS;

interface AppConfig {
  db_backend: "sqlite" | "postgres";
  sqlite_path: string | null;
  postgres: {
    host: string;
    port: number;
    database: string;
    username: string;
    password: string;
    ssl_mode: string;
  };
}

interface LoggingConfig {
  enabled: boolean;
  log_directory: string | null;
  filename_format: string;
}

interface AppLoggingConfig {
  enabled: boolean;
  log_directory: string | null;
  level: string;
}

interface SettingsDialogProps {
  onClose: () => void;
}

export function SettingsDialog({ onClose }: SettingsDialogProps): React.JSX.Element {
  const { t, i18n } = useTranslation();
  const {
    language,
    setLanguage,
    uiScale,
    setUiScale,
    themeMode,
    setThemeMode,
    closeOnDisconnect,
    setCloseOnDisconnect,
    openLocalOnStartup,
    setOpenLocalOnStartup,
    confirmOnQuit,
    setConfirmOnQuit,
    confirmOnCloseTab,
    setConfirmOnCloseTab,
    terminalFontFamily,
    setTerminalFontFamily,
    terminalFontSize,
    setTerminalFontSize,
    copyOnSelect,
    setCopyOnSelect,
    pasteOnRightClick,
    setPasteOnRightClick,
    restrictPrivateIps,
    setRestrictPrivateIps,
    autoRefreshInterval,
    setAutoRefreshInterval,
    connectTimeout,
    setConnectTimeout,
    keepaliveInterval,
    setKeepaliveInterval,
    keepaliveMax,
    setKeepaliveMax,
    toastAutoDismiss,
    setToastAutoDismiss,
    toastDismissSeconds,
    setToastDismissSeconds,
  } = useSettingsStore();

  const currentLang = language !== "" ? language : (i18n.resolvedLanguage ?? "en");

  // Database config — local state, loaded from backend
  const [dbBackend, setDbBackend] = useState<"sqlite" | "postgres">("sqlite");
  const [sqlitePath, setSqlitePath] = useState("");
  const [pgHost, setPgHost] = useState("");
  const [pgPort, setPgPort] = useState(5432);
  const [pgDatabase, setPgDatabase] = useState("");
  const [pgUsername, setPgUsername] = useState("");
  const [pgPassword, setPgPassword] = useState("");
  const [pgSslMode, setPgSslMode] = useState("prefer");
  const [dbTestResult, setDbTestResult] = useState<string | null>(null);
  const [dbTestLoading, setDbTestLoading] = useState(false);
  const [dbCreateLoading, setDbCreateLoading] = useState(false);
  const [dbCreateResult, setDbCreateResult] = useState<string | null>(null);
  const [dbSaved, setDbSaved] = useState(false);
  const [dbError, setDbError] = useState<string | null>(null);
  const [dbOpResult, setDbOpResult] = useState<string | null>(null);
  const [dbDirty, setDbDirty] = useState(false);

  // User identity (PG mode only)
  const appDbBackend = useAppStore((s) => s.dbBackend);
  const appUserIdent = useAppStore((s) => s.userIdent);
  const [userIdentInput, setUserIdentInput] = useState(appUserIdent ?? "");
  const [userIdentSaved, setUserIdentSaved] = useState(false);

  useEffect(() => {
    setUserIdentInput(appUserIdent ?? "");
  }, [appUserIdent]);

  const handleUserIdentSave = useCallback(async () => {
    const trimmed = userIdentInput.trim();
    if (!trimmed) return;
    try {
      await useAppStore.getState().setUserIdent(trimmed);
      setUserIdentSaved(true);
      setTimeout(() => {
        setUserIdentSaved(false);
      }, 3000);
    } catch (err: unknown) {
      useToastStore.getState().addToast(String(err));
    }
  }, [userIdentInput]);

  const [importProgress, setImportProgress] = useState<{
    phase: string;
    current: number;
    total: number;
  } | null>(null);

  // Logging config — loaded from backend
  const [loggingEnabled, setLoggingEnabled] = useState(false);
  const [logDirectory, setLogDirectory] = useState("");
  const [logFilenameFormat, setLogFilenameFormat] = useState("{name}_{mm}-{hh}_{dd}{MM}{yy}.log");
  const [loggingDirty, setLoggingDirty] = useState(false);
  const [loggingSaved, setLoggingSaved] = useState(false);
  const [loggingError, setLoggingError] = useState<string | null>(null);

  // Application logging — separate from session logging.
  const [appLoggingEnabled, setAppLoggingEnabled] = useState(false);
  const [appLogDirectory, setAppLogDirectory] = useState("");
  const [appLogLevel, setAppLogLevel] = useState("info");
  const [appLoggingDirty, setAppLoggingDirty] = useState(false);
  const [appLoggingSaved, setAppLoggingSaved] = useState(false);
  const [appLoggingError, setAppLoggingError] = useState<string | null>(null);

  // Highlight profiles
  const highlightProfiles = useHighlightStore((s) => s.profiles);
  const loadHighlightProfiles = useHighlightStore((s) => s.loadProfiles);
  const createHighlightProfile = useHighlightStore((s) => s.createProfile);
  const updateHighlightProfile = useHighlightStore((s) => s.updateProfile);
  const deleteHighlightProfile = useHighlightStore((s) => s.deleteProfile);
  const [highlightDialog, setHighlightDialog] = useState<{
    mode: "create" | "edit";
    profile?: HighlightProfile;
  } | null>(null);
  const [highlightDeleteConfirm, setHighlightDeleteConfirm] = useState<HighlightProfile | null>(
    null,
  );

  useEffect(() => {
    loadHighlightProfiles().catch(noop);
  }, [loadHighlightProfiles]);

  const handleHighlightSubmit = (name: string, rules: HighlightRule[]): void => {
    if (highlightDialog?.mode === "create") {
      createHighlightProfile(name, rules).catch(noop);
    } else if (highlightDialog?.mode === "edit" && highlightDialog.profile) {
      updateHighlightProfile(highlightDialog.profile.id, name, rules).catch(noop);
    }
    setHighlightDialog(null);
  };

  const handleImportHighlights = (): void => {
    const input = document.createElement("input");
    input.type = "file";
    input.accept = ".ini";
    input.style.display = "none";
    document.body.appendChild(input);
    input.onchange = async (): Promise<void> => {
      const file = input.files?.[0];
      if (!file) return;
      try {
        const text = await file.text();
        const result = await invoke<{ profiles_created: number; total_rules: number }>(
          "import_securecrt_highlights",
          { content: text },
        );
        await loadHighlightProfiles();
        useToastStore.getState().addToast(
          t("highlighting.importSuccess", {
            count: String(result.profiles_created),
            rules: String(result.total_rules),
          }),
          "success",
        );
      } catch (e) {
        useToastStore.getState().addToast(String(e), "error");
      } finally {
        document.body.removeChild(input);
      }
    };
    input.click();
  };

  useEffect(() => {
    invoke<AppConfig>("db_get_config")
      .then((config) => {
        setDbBackend(config.db_backend);
        setSqlitePath(config.sqlite_path ?? "");
        setPgHost(config.postgres.host);
        setPgPort(config.postgres.port);
        setPgDatabase(config.postgres.database);
        setPgUsername(config.postgres.username);
        setPgPassword(config.postgres.password);
        setPgSslMode(config.postgres.ssl_mode || "prefer");
      })
      .catch(() => {
        // Config load failed — keep defaults
      });
  }, []);

  // Load logging config from backend on mount
  useEffect(() => {
    invoke<LoggingConfig>("logging_get_config")
      .then((cfg) => {
        setLoggingEnabled(cfg.enabled);
        setLogDirectory(cfg.log_directory ?? "");
        setLogFilenameFormat(cfg.filename_format);
      })
      .catch(() => {
        // Config load failed — keep defaults
      });
  }, []);

  useEffect(() => {
    invoke<AppLoggingConfig>("app_logging_get_config")
      .then((cfg) => {
        setAppLoggingEnabled(cfg.enabled);
        setAppLogDirectory(cfg.log_directory ?? "");
        setAppLogLevel(cfg.level);
      })
      .catch(() => {
        // Config load failed — keep defaults
      });
  }, []);

  const handleLoggingSave = useCallback(async () => {
    setLoggingError(null);
    try {
      await invoke("logging_save_config", {
        enabled: loggingEnabled,
        logDirectory: logDirectory || null,
        filenameFormat: logFilenameFormat || null,
      });
      setLoggingDirty(false);
      setLoggingSaved(true);
    } catch (e) {
      setLoggingError(String(e));
    }
  }, [loggingEnabled, logDirectory, logFilenameFormat]);

  const handleAppLoggingSave = useCallback(async () => {
    setAppLoggingError(null);
    try {
      await invoke("app_logging_save_config", {
        enabled: appLoggingEnabled,
        logDirectory: appLogDirectory || null,
        level: appLogLevel,
      });
      setAppLoggingDirty(false);
      setAppLoggingSaved(true);
    } catch (e) {
      setAppLoggingError(String(e));
    }
  }, [appLoggingEnabled, appLogDirectory, appLogLevel]);

  const handleDbBackendChange = useCallback((backend: "sqlite" | "postgres") => {
    setDbBackend(backend);
    setDbDirty(true);
    setDbSaved(false);
    setDbTestResult(null);
  }, []);

  const handlePgFieldChange = useCallback(() => {
    setDbDirty(true);
    setDbSaved(false);
    setDbTestResult(null);
  }, []);

  const handleTestConnection = useCallback(async () => {
    setDbTestLoading(true);
    setDbTestResult(null);
    try {
      const result = await invoke<string>("db_test_connection", {
        host: pgHost,
        port: pgPort,
        database: pgDatabase,
        sslMode: pgSslMode,
        username: pgUsername,
        password: pgPassword,
      });
      setDbTestResult(result === "db_not_found" ? "db_not_found" : "success");
    } catch (e) {
      setDbTestResult(String(e));
    } finally {
      setDbTestLoading(false);
    }
  }, [pgHost, pgPort, pgDatabase, pgUsername, pgPassword, pgSslMode]);

  const handleCreateDatabase = useCallback(async () => {
    setDbCreateLoading(true);
    setDbCreateResult(null);
    try {
      await invoke<string>("db_create_database", {
        host: pgHost,
        port: pgPort,
        database: pgDatabase,
        sslMode: pgSslMode,
        username: pgUsername,
        password: pgPassword,
      });
      setDbCreateResult("success");
    } catch (e) {
      setDbCreateResult(String(e));
    } finally {
      setDbCreateLoading(false);
    }
  }, [pgHost, pgPort, pgDatabase, pgUsername, pgPassword, pgSslMode]);

  const handleDbSave = useCallback(async () => {
    setDbError(null);
    try {
      await invoke("db_save_config", {
        backend: dbBackend,
        sqlitePath: sqlitePath || null,
        host: pgHost,
        port: pgPort,
        database: pgDatabase,
        username: pgUsername,
        password: pgPassword,
        sslMode: pgSslMode,
      });
      setDbSaved(true);
      setDbDirty(false);
    } catch (e) {
      setDbError(String(e));
    }
  }, [dbBackend, sqlitePath, pgHost, pgPort, pgDatabase, pgUsername, pgPassword, pgSslMode]);

  const handleDbExport = useCallback(async () => {
    setDbOpResult(null);
    setDbError(null);
    try {
      const path = await save({
        title: t("settings.dbExport"),
        defaultPath: "shellstation-export.json",
        filters: [{ name: "JSON", extensions: ["json"] }],
      });
      if (!path) return;
      const result = await invoke<string>("db_export_file", { path });
      setDbOpResult(result);
    } catch (e) {
      setDbError(String(e));
    }
  }, [t]);

  const handleDbImport = useCallback(() => {
    setDbOpResult(null);
    setDbError(null);
    const input = document.createElement("input");
    input.type = "file";
    input.accept = ".json";
    input.style.display = "none";
    document.body.appendChild(input);
    input.onchange = async () => {
      try {
        const file = input.files?.[0];
        if (!file) return;
        const MAX_IMPORT_SIZE = 100 * 1024 * 1024; // 100 MB
        if (file.size > MAX_IMPORT_SIZE) {
          setDbError("Import file is too large (max 100 MB).");
          return;
        }
        const text = await file.text();
        const data: unknown = JSON.parse(text);
        if (
          typeof data !== "object" ||
          data === null ||
          Array.isArray(data) ||
          !("folders" in data) ||
          !("sessions" in data) ||
          !Array.isArray((data as Record<string, unknown>).folders) ||
          !Array.isArray((data as Record<string, unknown>).sessions)
        ) {
          setDbError("Invalid export format: expected an object with folders and sessions arrays.");
          return;
        }
        const result = await invoke<string>("db_import", { data });
        await useSessionStore.getState().loadAll();
        setDbOpResult(result);
      } catch (e) {
        setDbError(String(e));
      } finally {
        input.remove();
      }
    };
    input.click();
  }, []);

  const handleImportExternal = useCallback(
    (command: string, accept: string) => {
      const input = document.createElement("input");
      input.type = "file";
      input.accept = accept;
      input.style.display = "none";
      document.body.appendChild(input);
      input.onchange = async () => {
        let unlisten: (() => void) | null = null;
        try {
          const file = input.files?.[0];
          if (!file) return;
          const MAX_SIZE = 100 * 1024 * 1024;
          if (file.size > MAX_SIZE) {
            useToastStore.getState().addToast(t("settings.importTooLarge"));
            return;
          }
          const xml = await file.text();
          setImportProgress({ phase: "reading", current: 0, total: 0 });
          unlisten = await listen<{ phase: string; current: number; total: number }>(
            "import:progress",
            (event) => {
              setImportProgress(event.payload);
            },
          );
          const result = await invoke<{
            folders_created: number;
            sessions_created: number;
            skipped: number;
            warnings: string[];
          }>(command, { xml });
          const parts = [
            `${String(result.folders_created)} folders`,
            `${String(result.sessions_created)} sessions`,
          ];
          if (result.skipped > 0) {
            parts.push(`${String(result.skipped)} skipped`);
          }
          await useSessionStore.getState().loadAll();
          useToastStore
            .getState()
            .addToast(t("settings.importSuccess", { summary: parts.join(", ") }), "success");
          if (result.warnings.length > 0) {
            for (const w of result.warnings) {
              useToastStore.getState().addToast(w, "warning");
            }
          }
        } catch (e) {
          useToastStore.getState().addToast(String(e));
        } finally {
          if (unlisten) unlisten();
          setImportProgress(null);
          input.remove();
        }
      };
      input.click();
    },
    [t],
  );

  // Clear sensitive fields from state when the dialog unmounts.
  useEffect(() => {
    return () => {
      setPgPassword("");
    };
  }, []);

  useEscapeKey(onClose);

  return (
    <div className="dialog-overlay" role="presentation">
      {importProgress && (
        <div className="dialog-overlay" style={{ zIndex: 10000 }} role="presentation">
          <div className="dialog" style={{ minWidth: 320, textAlign: "center" }}>
            <div className="dialog-title">
              {t("settings.importInProgress", { defaultValue: "Importing…" })}
            </div>
            <div style={{ marginTop: 12 }}>
              {t(`settings.importPhase.${importProgress.phase}`, {
                defaultValue: importProgress.phase,
              })}
              {importProgress.total > 0 && (
                <>
                  {" "}
                  — {importProgress.current} / {importProgress.total}
                </>
              )}
            </div>
          </div>
        </div>
      )}
      <div
        className="dialog dialog-wide"
        role="dialog"
        aria-modal="true"
        aria-labelledby="settings-title"
      >
        <h3 className="dialog-title" id="settings-title">
          {t("settings.title")}
        </h3>
        <h4 className="settings-section-title">{t("settings.general")}</h4>
        <div className="dialog-field">
          <label htmlFor="settings-language">🌐 {t("settings.languageLabel")}</label>
          <CustomSelect
            id="settings-language"
            value={currentLang}
            onChange={setLanguage}
            options={AVAILABLE_LANGUAGES.map((lang) => ({
              value: lang.code,
              label: lang.label,
            }))}
          />
        </div>
        <div className="dialog-field">
          <label htmlFor="settings-ui-scale">{t("settings.uiScaleLabel")}</label>
          <CustomSelect
            id="settings-ui-scale"
            value={String(uiScale)}
            onChange={(v) => {
              setUiScale(Number(v));
            }}
            options={UI_SCALE_OPTIONS.map((scale) => ({
              value: String(scale),
              label: `${String(scale)}%`,
            }))}
          />
        </div>
        <div className="dialog-field">
          <label htmlFor="settings-theme">{t("settings.themeLabel")}</label>
          <CustomSelect
            id="settings-theme"
            value={themeMode}
            onChange={(v) => {
              setThemeMode(v as "dark" | "light" | "system");
            }}
            options={[
              { value: "dark", label: t("settings.themeDark") },
              { value: "light", label: t("settings.themeLight") },
              { value: "system", label: t("settings.themeSystem") },
            ]}
          />
        </div>
        <h4 className="settings-section-title">{t("settings.sessions")}</h4>
        <div className="dialog-field dialog-field-row">
          <input
            type="checkbox"
            id="settings-close-on-disconnect"
            checked={closeOnDisconnect}
            onChange={(e) => {
              setCloseOnDisconnect(e.target.checked);
            }}
          />
          <label htmlFor="settings-close-on-disconnect">
            {t("settings.closeOnDisconnectLabel")}
          </label>
          <span className="settings-help" title={t("settings.closeOnDisconnectHint")}>
            ?
          </span>
        </div>
        <div className="dialog-field dialog-field-row">
          <input
            type="checkbox"
            id="settings-open-local-on-startup"
            checked={openLocalOnStartup}
            onChange={(e) => {
              setOpenLocalOnStartup(e.target.checked);
            }}
          />
          <label htmlFor="settings-open-local-on-startup">
            {t("settings.openLocalOnStartupLabel")}
          </label>
          <span className="settings-help" title={t("settings.openLocalOnStartupHint")}>
            ?
          </span>
        </div>
        <div className="dialog-field dialog-field-row">
          <input
            type="checkbox"
            id="settings-confirm-on-close-tab"
            checked={confirmOnCloseTab}
            onChange={(e) => {
              setConfirmOnCloseTab(e.target.checked);
            }}
          />
          <label htmlFor="settings-confirm-on-close-tab">
            {t("settings.confirmOnCloseTabLabel")}
          </label>
          <span className="settings-help" title={t("settings.confirmOnCloseTabHint")}>
            ?
          </span>
        </div>
        <div className="dialog-field dialog-field-row">
          <input
            type="checkbox"
            id="settings-confirm-on-quit"
            checked={confirmOnQuit}
            onChange={(e) => {
              setConfirmOnQuit(e.target.checked);
            }}
          />
          <label htmlFor="settings-confirm-on-quit">{t("settings.confirmOnQuitLabel")}</label>
          <span className="settings-help" title={t("settings.confirmOnQuitHint")}>
            ?
          </span>
        </div>

        <div className="dialog-field">
          <label htmlFor="settings-auto-refresh">{t("settings.autoRefreshLabel")}</label>
          <CustomSelect
            id="settings-auto-refresh"
            value={String(autoRefreshInterval)}
            onChange={(v) => {
              setAutoRefreshInterval(Number(v));
            }}
            options={[
              { value: "0", label: t("settings.autoRefreshOff") },
              { value: "10", label: "10s" },
              { value: "30", label: "30s" },
              { value: "60", label: "60s" },
              { value: "120", label: "120s" },
              { value: "300", label: "300s" },
            ]}
          />
          <span className="settings-help" title={t("settings.autoRefreshHint")}>
            ?
          </span>
        </div>

        <div className="dialog-field">
          <label htmlFor="settings-connect-timeout">{t("settings.connectTimeoutLabel")}</label>
          <CustomSelect
            id="settings-connect-timeout"
            value={String(connectTimeout)}
            onChange={(v) => {
              setConnectTimeout(Number(v));
            }}
            options={[
              { value: "5", label: "5s" },
              { value: "10", label: "10s" },
              { value: "15", label: "15s" },
              { value: "30", label: "30s" },
              { value: "60", label: "60s" },
              { value: "120", label: "120s" },
            ]}
          />
          <span className="settings-help" title={t("settings.connectTimeoutHint")}>
            ?
          </span>
        </div>

        <div className="dialog-field">
          <label htmlFor="settings-keepalive-interval">
            {t("settings.keepaliveIntervalLabel")}
          </label>
          <CustomSelect
            id="settings-keepalive-interval"
            value={String(keepaliveInterval)}
            onChange={(v) => {
              setKeepaliveInterval(Number(v));
            }}
            options={[
              { value: "0", label: t("settings.keepaliveOff") },
              { value: "15", label: "15s" },
              { value: "30", label: "30s" },
              { value: "60", label: "60s" },
              { value: "120", label: "120s" },
              { value: "300", label: "300s" },
            ]}
          />
          <span className="settings-help" title={t("settings.keepaliveIntervalHint")}>
            ?
          </span>
        </div>

        {keepaliveInterval > 0 && (
          <div className="dialog-field">
            <label htmlFor="settings-keepalive-max">{t("settings.keepaliveMaxLabel")}</label>
            <CustomSelect
              id="settings-keepalive-max"
              value={String(keepaliveMax)}
              onChange={(v) => {
                setKeepaliveMax(Number(v));
              }}
              options={[
                { value: "1", label: "1" },
                { value: "3", label: "3" },
                { value: "5", label: "5" },
                { value: "10", label: "10" },
              ]}
            />
            <span className="settings-help" title={t("settings.keepaliveMaxHint")}>
              ?
            </span>
          </div>
        )}

        <div className="dialog-field dialog-field-row">
          <input
            type="checkbox"
            id="settings-toast-auto-dismiss"
            checked={toastAutoDismiss}
            onChange={(e) => {
              setToastAutoDismiss(e.target.checked);
            }}
          />
          <label htmlFor="settings-toast-auto-dismiss">{t("settings.toastAutoDismissLabel")}</label>
          <span className="settings-help" title={t("settings.toastAutoDismissHint")}>
            ?
          </span>
        </div>
        {toastAutoDismiss && (
          <div className="dialog-field">
            <label htmlFor="settings-toast-dismiss-seconds">
              {t("settings.toastDismissSecondsLabel")}
            </label>
            <CustomSelect
              id="settings-toast-dismiss-seconds"
              value={String(toastDismissSeconds)}
              onChange={(v) => {
                setToastDismissSeconds(Number(v));
              }}
              options={[
                { value: "3", label: "3s" },
                { value: "5", label: "5s" },
                { value: "10", label: "10s" },
                { value: "15", label: "15s" },
                { value: "30", label: "30s" },
              ]}
            />
          </div>
        )}

        {/* ── Terminal Section ──────────────────────────────────────── */}
        <h4 className="settings-section-title">{t("settings.terminal")}</h4>
        <div className="dialog-field">
          <label htmlFor="settings-font-family">{t("settings.fontFamilyLabel")}</label>
          <CustomSelect
            id="settings-font-family"
            value={terminalFontFamily}
            onChange={setTerminalFontFamily}
            options={FONT_OPTIONS.map((font) => ({ value: font, label: font }))}
          />
        </div>
        <div className="dialog-field">
          <label htmlFor="settings-font-size">{t("settings.fontSizeLabel")}</label>
          <input
            id="settings-font-size"
            type="number"
            value={terminalFontSize}
            min={6}
            max={72}
            onChange={(e) => {
              const val = Math.max(6, Math.min(72, Number(e.target.value) || 6));
              setTerminalFontSize(val);
            }}
          />
          <span className="settings-help" title={t("settings.fontSizeHint")}>
            ?
          </span>
        </div>
        <div
          className="settings-font-preview"
          style={{
            fontFamily: FONT_OPTIONS.includes(terminalFontFamily)
              ? `"${terminalFontFamily}", monospace`
              : "monospace",
            fontSize: `${String(terminalFontSize)}px`,
          }}
        >
          {t("settings.fontPreview")}
        </div>
        <div className="dialog-field dialog-field-row">
          <input
            type="checkbox"
            id="settings-copy-on-select"
            checked={copyOnSelect}
            onChange={(e) => {
              setCopyOnSelect(e.target.checked);
            }}
          />
          <label htmlFor="settings-copy-on-select">{t("settings.copyOnSelectLabel")}</label>
          <span className="settings-help" title={t("settings.copyOnSelectHint")}>
            ?
          </span>
        </div>
        <div className="dialog-field dialog-field-row">
          <input
            type="checkbox"
            id="settings-paste-on-right-click"
            checked={pasteOnRightClick}
            onChange={(e) => {
              setPasteOnRightClick(e.target.checked);
            }}
          />
          <label htmlFor="settings-paste-on-right-click">
            {t("settings.pasteOnRightClickLabel")}
          </label>
          <span className="settings-help" title={t("settings.pasteOnRightClickHint")}>
            ?
          </span>
        </div>

        {/* ── Security Section ────────────────────────────────────── */}
        <h4 className="settings-section-title">{t("settings.security")}</h4>
        <div className="dialog-field dialog-field-row">
          <input
            type="checkbox"
            id="settings-restrict-private-ips"
            checked={restrictPrivateIps}
            onChange={(e) => {
              setRestrictPrivateIps(e.target.checked);
            }}
          />
          <label htmlFor="settings-restrict-private-ips">
            {t("settings.restrictPrivateIpsLabel")}
          </label>
          <span className="settings-help" title={t("settings.restrictPrivateIpsHint")}>
            ?
          </span>
        </div>

        {/* ── Session Logging Section ──────────────────────────────── */}
        <h4 className="settings-section-title">{t("settings.logging")}</h4>
        <div className="dialog-field dialog-field-row">
          <input
            type="checkbox"
            id="settings-logging-enabled"
            checked={loggingEnabled}
            onChange={(e) => {
              setLoggingEnabled(e.target.checked);
              setLoggingDirty(true);
              setLoggingSaved(false);
            }}
          />
          <label htmlFor="settings-logging-enabled">{t("settings.loggingEnabledLabel")}</label>
          <span className="settings-help" title={t("settings.loggingEnabledHint")}>
            ?
          </span>
        </div>
        {loggingEnabled && (
          <>
            <div className="dialog-field">
              <div className="settings-label-row">
                <label htmlFor="settings-log-directory">
                  {t("settings.loggingDirectoryLabel")}
                </label>
                <span className="settings-help" title={t("settings.loggingDirectoryHint")}>
                  ?
                </span>
              </div>
              <div className="dialog-row">
                <div className="dialog-field-grow">
                  <input
                    id="settings-log-directory"
                    type="text"
                    value={logDirectory}
                    placeholder={t("settings.loggingDirectoryPlaceholder")}
                    onChange={(e) => {
                      setLogDirectory(e.target.value);
                      setLoggingDirty(true);
                      setLoggingSaved(false);
                    }}
                  />
                </div>
                <button
                  type="button"
                  className="dialog-btn"
                  onClick={() => {
                    void (async () => {
                      const path = await open({
                        title: t("settings.loggingDirectoryLabel"),
                        defaultPath: logDirectory || undefined,
                        multiple: false,
                        directory: true,
                      });
                      if (path) {
                        setLogDirectory(path);
                        setLoggingDirty(true);
                        setLoggingSaved(false);
                      }
                    })();
                  }}
                >
                  {t("settings.dbBrowse")}
                </button>
              </div>
            </div>
            <div className="dialog-field">
              <div className="settings-label-row">
                <label htmlFor="settings-log-filename">{t("settings.loggingFilenameLabel")}</label>
                <span className="settings-help" title={t("settings.loggingFilenameHint")}>
                  ?
                </span>
              </div>
              <input
                id="settings-log-filename"
                type="text"
                value={logFilenameFormat}
                onChange={(e) => {
                  setLogFilenameFormat(e.target.value);
                  setLoggingDirty(true);
                  setLoggingSaved(false);
                }}
              />
            </div>
          </>
        )}
        {loggingDirty && (
          <div className="dialog-field">
            <button
              type="button"
              className="dialog-btn dialog-btn-primary"
              onClick={() => {
                void handleLoggingSave();
              }}
            >
              {t("settings.loggingSave")}
            </button>
          </div>
        )}
        {loggingSaved && <span className="settings-db-success">{t("settings.loggingSaved")}</span>}
        {loggingError !== null && <p className="settings-db-error">{loggingError}</p>}

        {/* ── Application Logging Section ──────────────────────────── */}
        <h4 className="settings-section-title">{t("settings.appLogging")}</h4>
        <div className="dialog-field dialog-field-row">
          <input
            type="checkbox"
            id="settings-app-logging-enabled"
            checked={appLoggingEnabled}
            onChange={(e) => {
              setAppLoggingEnabled(e.target.checked);
              setAppLoggingDirty(true);
              setAppLoggingSaved(false);
            }}
          />
          <label htmlFor="settings-app-logging-enabled">
            {t("settings.appLoggingEnabledLabel")}
          </label>
          <span className="settings-help" title={t("settings.appLoggingEnabledHint")}>
            ?
          </span>
        </div>
        {appLoggingEnabled && (
          <>
            <div className="dialog-field">
              <div className="settings-label-row">
                <label htmlFor="settings-app-log-directory">
                  {t("settings.appLoggingDirectoryLabel")}
                </label>
                <span className="settings-help" title={t("settings.appLoggingDirectoryHint")}>
                  ?
                </span>
              </div>
              <div className="dialog-row">
                <div className="dialog-field-grow">
                  <input
                    id="settings-app-log-directory"
                    type="text"
                    value={appLogDirectory}
                    placeholder={t("settings.appLoggingDirectoryPlaceholder")}
                    onChange={(e) => {
                      setAppLogDirectory(e.target.value);
                      setAppLoggingDirty(true);
                      setAppLoggingSaved(false);
                    }}
                  />
                </div>
                <button
                  type="button"
                  className="dialog-btn"
                  onClick={() => {
                    void (async () => {
                      const path = await open({
                        title: t("settings.appLoggingDirectoryLabel"),
                        defaultPath: appLogDirectory || undefined,
                        multiple: false,
                        directory: true,
                      });
                      if (path) {
                        setAppLogDirectory(path);
                        setAppLoggingDirty(true);
                        setAppLoggingSaved(false);
                      }
                    })();
                  }}
                >
                  {t("settings.dbBrowse")}
                </button>
              </div>
            </div>
            <div className="dialog-field">
              <div className="settings-label-row">
                <label htmlFor="settings-app-log-level">{t("settings.appLoggingLevelLabel")}</label>
                <span className="settings-help" title={t("settings.appLoggingLevelHint")}>
                  ?
                </span>
              </div>
              <CustomSelect
                id="settings-app-log-level"
                value={appLogLevel}
                onChange={(v) => {
                  setAppLogLevel(v);
                  setAppLoggingDirty(true);
                  setAppLoggingSaved(false);
                }}
                options={[
                  { value: "error", label: "ERROR" },
                  { value: "warn", label: "WARN" },
                  { value: "info", label: "INFO" },
                  { value: "debug", label: "DEBUG" },
                  { value: "trace", label: "TRACE" },
                ]}
              />
            </div>
          </>
        )}
        {appLoggingDirty && (
          <div className="dialog-field">
            <button
              type="button"
              className="dialog-btn dialog-btn-primary"
              onClick={() => {
                void handleAppLoggingSave();
              }}
            >
              {t("settings.appLoggingSave")}
            </button>
          </div>
        )}
        {appLoggingSaved && (
          <span className="settings-db-success">{t("settings.appLoggingSaved")}</span>
        )}
        {appLoggingError !== null && <p className="settings-db-error">{appLoggingError}</p>}

        {/* ── Database Section ─────────────────────────────────────── */}
        <h4 className="settings-section-title">{t("settings.database")}</h4>
        <div className="dialog-field">
          <label htmlFor="settings-db-backend">{t("settings.dbBackendLabel")}</label>
          <CustomSelect
            id="settings-db-backend"
            value={dbBackend}
            onChange={(v) => {
              handleDbBackendChange(v as "sqlite" | "postgres");
            }}
            options={[
              { value: "sqlite", label: t("settings.dbSqlite") },
              { value: "postgres", label: t("settings.dbPostgres") },
            ]}
          />
        </div>
        {dbBackend === "sqlite" && (
          <div className="dialog-field">
            <label htmlFor="settings-sqlite-path">{t("settings.dbSqlitePathLabel")}</label>
            <div className="dialog-row">
              <div className="dialog-field-grow">
                <input
                  id="settings-sqlite-path"
                  type="text"
                  value={sqlitePath}
                  placeholder={t("settings.dbSqlitePathPlaceholder")}
                  onChange={(e) => {
                    setSqlitePath(e.target.value);
                    setDbDirty(true);
                    setDbSaved(false);
                  }}
                />
              </div>
              <button
                type="button"
                className="dialog-btn"
                onClick={() => {
                  void (async () => {
                    const path = await open({
                      title: t("settings.dbSqlitePathLabel"),
                      defaultPath: sqlitePath || undefined,
                      multiple: false,
                      directory: false,
                      filters: [{ name: "SQLite", extensions: ["db", "sqlite", "sqlite3"] }],
                    });
                    if (path) {
                      setSqlitePath(path);
                      setDbDirty(true);
                      setDbSaved(false);
                    }
                  })();
                }}
              >
                {t("settings.dbBrowse")}
              </button>
              <button
                type="button"
                className="dialog-btn"
                onClick={() => {
                  void (async () => {
                    const path = await save({
                      title: t("settings.dbCreateNew"),
                      defaultPath: "sessions.db",
                      filters: [{ name: "SQLite", extensions: ["db", "sqlite", "sqlite3"] }],
                    });
                    if (path) {
                      setSqlitePath(path);
                      setDbDirty(true);
                      setDbSaved(false);
                    }
                  })();
                }}
              >
                {t("settings.dbCreateNew")}
              </button>
              <span className="settings-help" title={t("settings.dbSqlitePathHint")}>
                ?
              </span>
            </div>
          </div>
        )}
        {dbBackend === "postgres" && (
          <>
            <div className="dialog-row">
              <div className="dialog-field dialog-field-grow">
                <label htmlFor="settings-pg-host">{t("settings.dbHostLabel")}</label>
                <input
                  id="settings-pg-host"
                  type="text"
                  value={pgHost}
                  placeholder={t("settings.dbHostPlaceholder")}
                  onChange={(e) => {
                    setPgHost(e.target.value);
                    handlePgFieldChange();
                  }}
                />
              </div>
              <div className="dialog-field dialog-field-small">
                <label htmlFor="settings-pg-port">{t("settings.dbPortLabel")}</label>
                <input
                  id="settings-pg-port"
                  type="number"
                  value={pgPort}
                  min={1}
                  max={65535}
                  onChange={(e) => {
                    setPgPort(Number(e.target.value));
                    handlePgFieldChange();
                  }}
                />
              </div>
            </div>
            <div className="dialog-field">
              <label htmlFor="settings-pg-database">{t("settings.dbDatabaseLabel")}</label>
              <div className="dialog-row">
                <div className="dialog-field-grow">
                  <input
                    id="settings-pg-database"
                    type="text"
                    value={pgDatabase}
                    placeholder={t("settings.dbDatabasePlaceholder")}
                    onChange={(e) => {
                      setPgDatabase(e.target.value);
                      setDbCreateResult(null);
                      handlePgFieldChange();
                    }}
                  />
                </div>
                <button
                  type="button"
                  className="dialog-btn"
                  disabled={dbCreateLoading || !pgHost || !pgDatabase || !pgUsername}
                  onClick={() => {
                    void handleCreateDatabase();
                  }}
                >
                  {dbCreateLoading ? "..." : t("settings.dbCreateDatabase")}
                </button>
              </div>
              {dbCreateResult === "success" && (
                <span className="settings-db-success">{t("settings.dbCreateDatabaseSuccess")}</span>
              )}
              {dbCreateResult !== null && dbCreateResult !== "success" && (
                <span className="settings-db-error">
                  {t("settings.dbCreateDatabaseFailed", { message: dbCreateResult })}
                </span>
              )}
            </div>
            <div className="dialog-field">
              <label htmlFor="settings-pg-username">{t("settings.dbUsernameLabel")}</label>
              <input
                id="settings-pg-username"
                type="text"
                value={pgUsername}
                placeholder={t("settings.dbUsernamePlaceholder")}
                onChange={(e) => {
                  setPgUsername(e.target.value);
                  handlePgFieldChange();
                }}
              />
            </div>
            <div className="dialog-field">
              <label htmlFor="settings-pg-password">{t("settings.dbPasswordLabel")}</label>
              <input
                id="settings-pg-password"
                type="password"
                value={pgPassword}
                placeholder={t("settings.dbPasswordPlaceholder")}
                onChange={(e) => {
                  setPgPassword(e.target.value);
                  handlePgFieldChange();
                }}
              />
            </div>
            <div className="dialog-field">
              <label htmlFor="settings-pg-sslmode">{t("settings.dbSslModeLabel")}</label>
              <CustomSelect
                id="settings-pg-sslmode"
                value={pgSslMode}
                onChange={(v) => {
                  setPgSslMode(v);
                  handlePgFieldChange();
                }}
                options={[
                  { value: "disable", label: t("settings.dbSslModeDisable") },
                  { value: "prefer", label: t("settings.dbSslModePrefer") },
                  { value: "require", label: t("settings.dbSslModeRequire") },
                ]}
              />
            </div>
            <div className="dialog-field">
              <button
                type="button"
                className="dialog-btn"
                disabled={dbTestLoading || !pgHost || !pgDatabase || !pgUsername}
                onClick={() => {
                  void handleTestConnection();
                }}
              >
                {dbTestLoading ? "..." : t("settings.dbTestConnection")}
              </button>
              {dbTestResult === "success" && (
                <span className="settings-db-success">{t("settings.dbTestSuccess")}</span>
              )}
              {dbTestResult === "db_not_found" && (
                <span className="settings-db-warning">{t("settings.dbTestDbNotFound")}</span>
              )}
              {dbTestResult !== null &&
                dbTestResult !== "success" &&
                dbTestResult !== "db_not_found" && (
                  <span className="settings-db-error">
                    {t("settings.dbTestFailed", { message: dbTestResult })}
                  </span>
                )}
            </div>
            <p className="settings-db-note">{t("settings.dbCredentialNote")}</p>
            <p className="settings-db-note">{t("settings.pgIsolationNote")}</p>
          </>
        )}
        {appDbBackend === "postgres" && (
          <div className="dialog-field">
            <label htmlFor="settings-user-ident">{t("settings.userIdentLabel")}</label>
            <div className="dialog-row">
              <div className="dialog-field-grow">
                <input
                  id="settings-user-ident"
                  type="text"
                  value={userIdentInput}
                  onChange={(e) => {
                    setUserIdentInput(e.target.value);
                  }}
                  maxLength={128}
                />
              </div>
              <button
                type="button"
                className="dialog-btn"
                disabled={!userIdentInput.trim() || userIdentInput.trim() === appUserIdent}
                onClick={() => {
                  void handleUserIdentSave();
                }}
              >
                {t("dialog.save")}
              </button>
            </div>
            <span className="dialog-hint">{t("settings.userIdentHint")}</span>
            {userIdentSaved && (
              <span className="settings-db-success">{t("settings.userIdentSaved")}</span>
            )}
          </div>
        )}
        {dbDirty && (
          <div className="dialog-field">
            <button
              type="button"
              className="dialog-btn dialog-btn-primary"
              onClick={() => {
                void handleDbSave();
              }}
            >
              {t("settings.dbSaveRestart")}
            </button>
          </div>
        )}
        {dbSaved && <p className="settings-db-restart">{t("settings.dbRestartRequired")}</p>}
        {dbError !== null && <p className="settings-db-error">{dbError}</p>}

        {/* ── Data Export / Import ─────────────────────────────────── */}
        <div className="settings-label-row">
          <h4 className="settings-section-title">{t("settings.database")} — Export / Import</h4>
          <span className="settings-help" title={t("settings.dbExportImportHint")}>
            ?
          </span>
        </div>
        <div className="dialog-field dialog-field-row">
          <button
            type="button"
            className="dialog-btn"
            onClick={() => {
              void handleDbExport();
            }}
          >
            {t("settings.dbExport")}
          </button>
          <button type="button" className="dialog-btn" onClick={handleDbImport}>
            {t("settings.dbImport")}
          </button>
        </div>
        {dbOpResult && <span className="settings-db-success">{dbOpResult}</span>}

        {/* ── Keyword Highlighting ──────────────────────────── */}
        <div className="settings-label-row">
          <h4 className="settings-section-title">{t("highlighting.sectionTitle")}</h4>
          <span className="settings-help" title={t("highlighting.sectionHint")}>
            ?
          </span>
        </div>
        {highlightProfiles.length === 0 ? (
          <p className="settings-hint">{t("highlighting.noProfiles")}</p>
        ) : (
          <div className="highlight-profiles-list">
            {highlightProfiles.map((p) => (
              <div key={p.id} className="highlight-profile-item">
                <span className="highlight-profile-name">{p.name}</span>
                <span className="highlight-profile-count">
                  {p.rules.length} {p.rules.length === 1 ? "rule" : "rules"}
                </span>
                <button
                  type="button"
                  className="btn-icon"
                  title={t("common.edit")}
                  onClick={() => {
                    setHighlightDialog({ mode: "edit", profile: p });
                  }}
                >
                  {"\u270E"}
                </button>
                <button
                  type="button"
                  className="btn-icon btn-icon-danger"
                  title={t("common.delete")}
                  onClick={() => {
                    setHighlightDeleteConfirm(p);
                  }}
                >
                  {"\u2715"}
                </button>
              </div>
            ))}
          </div>
        )}
        <div className="dialog-field dialog-field-row">
          <button
            type="button"
            className="dialog-btn"
            onClick={() => {
              setHighlightDialog({ mode: "create" });
            }}
          >
            {t("highlighting.addProfile")}
          </button>
          <button type="button" className="dialog-btn" onClick={handleImportHighlights}>
            {t("highlighting.importProfile")}
          </button>
        </div>

        {/* ── Import from External Tools ──────────────────────────── */}
        <h4 className="settings-section-title">{t("settings.importExternal")}</h4>
        <div className="dialog-field dialog-field-row">
          <button
            type="button"
            className="dialog-btn"
            onClick={() => {
              handleImportExternal("import_mremoteng", ".xml");
            }}
          >
            {t("settings.importMremoteng")}
          </button>
          <button
            type="button"
            className="dialog-btn"
            onClick={() => {
              handleImportExternal("import_securecrt", ".xml");
            }}
          >
            {t("settings.importSecurecrt")}
          </button>
        </div>

        <div className="dialog-actions">
          <button
            type="button"
            className="dialog-btn dialog-btn-primary"
            onClick={
              dbSaved
                ? () => {
                    void invoke("app_restart");
                  }
                : onClose
            }
          >
            {dbSaved ? t("settings.dbSaveRestart") : t("settings.close")}
          </button>
        </div>
      </div>
      {highlightDialog && (
        <HighlightProfileDialog
          title={
            highlightDialog.mode === "create"
              ? t("highlighting.addProfile")
              : t("highlighting.editProfile")
          }
          initialName={highlightDialog.profile?.name ?? ""}
          initialRules={highlightDialog.profile?.rules ?? []}
          onSubmit={handleHighlightSubmit}
          onCancel={() => {
            setHighlightDialog(null);
          }}
        />
      )}
      {highlightDeleteConfirm && (
        <ConfirmDialog
          message={t("highlighting.deleteConfirm", { name: highlightDeleteConfirm.name })}
          onConfirm={() => {
            deleteHighlightProfile(highlightDeleteConfirm.id).catch(noop);
            setHighlightDeleteConfirm(null);
          }}
          onCancel={() => {
            setHighlightDeleteConfirm(null);
          }}
        />
      )}
    </div>
  );
}
