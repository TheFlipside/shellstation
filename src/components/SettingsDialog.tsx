import React, { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { open, save } from "@tauri-apps/plugin-dialog";
import { useEscapeKey } from "../hooks/useEscapeKey";
import { useSessionStore } from "../stores/sessionStore";
import { useSettingsStore } from "../stores/settingsStore";
import { useToastStore } from "../stores/toastStore";

const AVAILABLE_LANGUAGES = [
  { code: "en", label: "English" },
  { code: "de", label: "Deutsch" },
];

const UI_SCALE_OPTIONS = [75, 80, 90, 100, 110, 120, 125, 150];

const FONT_OPTIONS = [
  "JetBrains Mono",
  "Fira Code",
  "Cascadia Code",
  "Source Code Pro",
  "IBM Plex Mono",
  "Ubuntu Mono",
  "Hack",
  "Inconsolata",
  "DejaVu Sans Mono",
  "Courier New",
  "monospace",
];

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
  const [dbDirty, setDbDirty] = useState(false);

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
    try {
      const data = await invoke<unknown>("db_export");
      const json = JSON.stringify(data, null, 2);
      const blob = new Blob([json], { type: "application/json" });
      const url = URL.createObjectURL(blob);
      const a = document.createElement("a");
      a.href = url;
      a.download = "shellstation-export.json";
      a.click();
      URL.revokeObjectURL(url);
    } catch (e) {
      setDbError(String(e));
    }
  }, []);

  const handleDbImport = useCallback(() => {
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
        setDbTestResult(result);
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
        try {
          const file = input.files?.[0];
          if (!file) return;
          const MAX_SIZE = 100 * 1024 * 1024;
          if (file.size > MAX_SIZE) {
            useToastStore.getState().addToast(t("settings.importTooLarge"));
            return;
          }
          const xml = await file.text();
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
    <div className="dialog-overlay" onClick={onClose} role="presentation">
      <div
        className="dialog dialog-wide"
        onClick={(e) => {
          e.stopPropagation();
        }}
        role="dialog"
        aria-modal="true"
        aria-labelledby="settings-title"
      >
        <h3 className="dialog-title" id="settings-title">
          {t("settings.title")}
        </h3>
        <h4 className="settings-section-title">{t("settings.general")}</h4>
        <div className="dialog-field">
          <label htmlFor="settings-language">{t("settings.languageLabel")}</label>
          <select
            id="settings-language"
            value={currentLang}
            onChange={(e) => {
              setLanguage(e.target.value);
            }}
          >
            {AVAILABLE_LANGUAGES.map((lang) => (
              <option key={lang.code} value={lang.code}>
                {lang.label}
              </option>
            ))}
          </select>
        </div>
        <div className="dialog-field">
          <label htmlFor="settings-ui-scale">{t("settings.uiScaleLabel")}</label>
          <select
            id="settings-ui-scale"
            value={uiScale}
            onChange={(e) => {
              setUiScale(Number(e.target.value));
            }}
          >
            {UI_SCALE_OPTIONS.map((scale) => (
              <option key={scale} value={scale}>
                {String(scale)}%
              </option>
            ))}
          </select>
        </div>
        <div className="dialog-field">
          <label htmlFor="settings-theme">{t("settings.themeLabel")}</label>
          <select
            id="settings-theme"
            value={themeMode}
            onChange={(e) => {
              setThemeMode(e.target.value as "dark" | "light" | "system");
            }}
          >
            <option value="dark">{t("settings.themeDark")}</option>
            <option value="light">{t("settings.themeLight")}</option>
            <option value="system">{t("settings.themeSystem")}</option>
          </select>
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
          <select
            id="settings-auto-refresh"
            value={String(autoRefreshInterval)}
            onChange={(e) => {
              setAutoRefreshInterval(Number(e.target.value));
            }}
          >
            <option value="0">{t("settings.autoRefreshOff")}</option>
            <option value="10">10s</option>
            <option value="30">30s</option>
            <option value="60">60s</option>
            <option value="120">120s</option>
            <option value="300">300s</option>
          </select>
          <span className="settings-help" title={t("settings.autoRefreshHint")}>
            ?
          </span>
        </div>

        <div className="dialog-field">
          <label htmlFor="settings-connect-timeout">{t("settings.connectTimeoutLabel")}</label>
          <select
            id="settings-connect-timeout"
            value={String(connectTimeout)}
            onChange={(e) => {
              setConnectTimeout(Number(e.target.value));
            }}
          >
            <option value="5">5s</option>
            <option value="10">10s</option>
            <option value="15">15s</option>
            <option value="30">30s</option>
            <option value="60">60s</option>
            <option value="120">120s</option>
          </select>
          <span className="settings-help" title={t("settings.connectTimeoutHint")}>
            ?
          </span>
        </div>

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
            <select
              id="settings-toast-dismiss-seconds"
              value={String(toastDismissSeconds)}
              onChange={(e) => {
                setToastDismissSeconds(Number(e.target.value));
              }}
            >
              <option value="3">3s</option>
              <option value="5">5s</option>
              <option value="10">10s</option>
              <option value="15">15s</option>
              <option value="30">30s</option>
            </select>
          </div>
        )}

        {/* ── Terminal Section ──────────────────────────────────────── */}
        <h4 className="settings-section-title">{t("settings.terminal")}</h4>
        <div className="dialog-field">
          <label htmlFor="settings-font-family">{t("settings.fontFamilyLabel")}</label>
          <select
            id="settings-font-family"
            value={terminalFontFamily}
            onChange={(e) => {
              setTerminalFontFamily(e.target.value);
            }}
          >
            {FONT_OPTIONS.map((font) => (
              <option key={font} value={font}>
                {font}
              </option>
            ))}
          </select>
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

        {/* ── Database Section ─────────────────────────────────────── */}
        <h4 className="settings-section-title">{t("settings.database")}</h4>
        <div className="dialog-field">
          <label htmlFor="settings-db-backend">{t("settings.dbBackendLabel")}</label>
          <select
            id="settings-db-backend"
            value={dbBackend}
            onChange={(e) => {
              handleDbBackendChange(e.target.value as "sqlite" | "postgres");
            }}
          >
            <option value="sqlite">{t("settings.dbSqlite")}</option>
            <option value="postgres">{t("settings.dbPostgres")}</option>
          </select>
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
              <select
                id="settings-pg-sslmode"
                value={pgSslMode}
                onChange={(e) => {
                  setPgSslMode(e.target.value);
                  handlePgFieldChange();
                }}
              >
                <option value="disable">{t("settings.dbSslModeDisable")}</option>
                <option value="prefer">{t("settings.dbSslModePrefer")}</option>
                <option value="require">{t("settings.dbSslModeRequire")}</option>
              </select>
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
          </>
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
        <h4 className="settings-section-title">{t("settings.database")} — Export / Import</h4>
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
                    void getCurrentWindow().destroy();
                  }
                : onClose
            }
          >
            {dbSaved ? t("settings.dbSaveRestart") : t("settings.close")}
          </button>
        </div>
      </div>
    </div>
  );
}
