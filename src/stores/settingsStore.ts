import { create } from "zustand";
import { persist } from "zustand/middleware";
import i18n from "../i18n";

export type ThemeMode = "dark" | "light" | "system";

interface SettingsState {
  language: string;
  uiScale: number;
  themeMode: ThemeMode;
  closeOnDisconnect: boolean;
  openLocalOnStartup: boolean;
  confirmOnQuit: boolean;
  confirmOnCloseTab: boolean;
  terminalFontFamily: string;
  terminalFontSize: number;
  copyOnSelect: boolean;
  pasteOnRightClick: boolean;
  restrictPrivateIps: boolean;
  setLanguage: (lang: string) => void;
  setUiScale: (scale: number) => void;
  setThemeMode: (mode: ThemeMode) => void;
  setCloseOnDisconnect: (value: boolean) => void;
  setOpenLocalOnStartup: (value: boolean) => void;
  setConfirmOnQuit: (value: boolean) => void;
  setConfirmOnCloseTab: (value: boolean) => void;
  setTerminalFontFamily: (family: string) => void;
  setTerminalFontSize: (size: number) => void;
  setCopyOnSelect: (value: boolean) => void;
  setPasteOnRightClick: (value: boolean) => void;
  setRestrictPrivateIps: (value: boolean) => void;
}

export const useSettingsStore = create<SettingsState>()(
  persist(
    (set) => ({
      language: "",
      uiScale: 100,
      themeMode: "dark" as ThemeMode,
      closeOnDisconnect: false,
      openLocalOnStartup: true,
      confirmOnQuit: true,
      confirmOnCloseTab: true,
      terminalFontFamily: "JetBrains Mono",
      terminalFontSize: 14,
      copyOnSelect: false,
      pasteOnRightClick: false,
      restrictPrivateIps: false,
      setLanguage: (lang: string) => {
        void i18n.changeLanguage(lang);
        set({ language: lang });
      },
      setUiScale: (scale: number) => {
        set({ uiScale: scale });
      },
      setThemeMode: (mode: ThemeMode) => {
        set({ themeMode: mode });
      },
      setCloseOnDisconnect: (value: boolean) => {
        set({ closeOnDisconnect: value });
      },
      setOpenLocalOnStartup: (value: boolean) => {
        set({ openLocalOnStartup: value });
      },
      setConfirmOnQuit: (value: boolean) => {
        set({ confirmOnQuit: value });
      },
      setConfirmOnCloseTab: (value: boolean) => {
        set({ confirmOnCloseTab: value });
      },
      setTerminalFontFamily: (family: string) => {
        set({ terminalFontFamily: family });
      },
      setTerminalFontSize: (size: number) => {
        set({ terminalFontSize: size });
      },
      setCopyOnSelect: (value: boolean) => {
        set({ copyOnSelect: value });
      },
      setPasteOnRightClick: (value: boolean) => {
        set({ pasteOnRightClick: value });
      },
      setRestrictPrivateIps: (value: boolean) => {
        set({ restrictPrivateIps: value });
      },
    }),
    {
      name: "shellstation-settings",
      onRehydrateStorage: () => {
        return (state?: SettingsState) => {
          if (state?.language) {
            void i18n.changeLanguage(state.language);
          }
        };
      },
    },
  ),
);
