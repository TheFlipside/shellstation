import { create } from "zustand";
import { persist } from "zustand/middleware";
import i18n from "../i18n";

interface SettingsState {
  language: string;
  closeOnDisconnect: boolean;
  setLanguage: (lang: string) => void;
  setCloseOnDisconnect: (value: boolean) => void;
}

export const useSettingsStore = create<SettingsState>()(
  persist(
    (set) => ({
      language: "",
      closeOnDisconnect: false,
      setLanguage: (lang: string) => {
        void i18n.changeLanguage(lang);
        set({ language: lang });
      },
      setCloseOnDisconnect: (value: boolean) => {
        set({ closeOnDisconnect: value });
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
