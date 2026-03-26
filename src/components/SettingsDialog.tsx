import React, { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { open, save } from "@tauri-apps/plugin-dialog";
import { useEscapeKey } from "../hooks/useEscapeKey";
import { useSettingsStore } from "../stores/settingsStore";

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
  const [dbTestResult, setDbTestResult] = useState<string | null>(null);
  const [dbTestLoading, setDbTestLoading] = useState(false);
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
      await invoke<string>("db_test_connection", {
        host: pgHost,
        port: pgPort,
        database: pgDatabase,
        username: pgUsername,
        password: pgPassword,
      });
      setDbTestResult("success");
    } catch (e) {
      setDbTestResult(String(e));
    } finally {
      setDbTestLoading(false);
    }
  }, [pgHost, pgPort, pgDatabase, pgUsername, pgPassword]);

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
      });
      setDbSaved(true);
      setDbDirty(false);
    } catch (e) {
      setDbError(String(e));
    }
  }, [dbBackend, sqlitePath, pgHost, pgPort, pgDatabase, pgUsername, pgPassword]);

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
    input.onchange = async () => {
      const file = input.files?.[0];
      if (!file) return;
      const MAX_IMPORT_SIZE = 10 * 1024 * 1024; // 10 MB
      if (file.size > MAX_IMPORT_SIZE) {
        setDbError("Import file is too large (max 10 MB).");
        return;
      }
      try {
        const text = await file.text();
        const data: unknown = JSON.parse(text);
        const result = await invoke<string>("db_import", { data });
        setDbTestResult(result);
      } catch (e) {
        setDbError(String(e));
      }
    };
    input.click();
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
              <input
                id="settings-pg-database"
                type="text"
                value={pgDatabase}
                placeholder={t("settings.dbDatabasePlaceholder")}
                onChange={(e) => {
                  setPgDatabase(e.target.value);
                  handlePgFieldChange();
                }}
              />
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
              {dbTestResult !== null && dbTestResult !== "success" && (
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
