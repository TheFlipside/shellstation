import { create } from "zustand";
import { persist } from "zustand/middleware";
import i18n from "../i18n";

interface SettingsState {
  language: string;
  setLanguage: (lang: string) => void;
}

export const useSettingsStore = create<SettingsState>()(
  persist(
    (set) => ({
      language: "",
      setLanguage: (lang: string) => {
        void i18n.changeLanguage(lang);
        set({ language: lang });
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
