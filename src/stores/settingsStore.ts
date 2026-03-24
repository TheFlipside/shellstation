import { create } from "zustand";
import { persist } from "zustand/middleware";
import i18n from "../i18n";

interface SettingsState {
  language: string;
  closeOnDisconnect: boolean;
  openLocalOnStartup: boolean;
  confirmOnQuit: boolean;
  setLanguage: (lang: string) => void;
  setCloseOnDisconnect: (value: boolean) => void;
  setOpenLocalOnStartup: (value: boolean) => void;
  setConfirmOnQuit: (value: boolean) => void;
}

export const useSettingsStore = create<SettingsState>()(
  persist(
    (set) => ({
      language: "",
      closeOnDisconnect: false,
      openLocalOnStartup: true,
      confirmOnQuit: true,
      setLanguage: (lang: string) => {
        void i18n.changeLanguage(lang);
        set({ language: lang });
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
