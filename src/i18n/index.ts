import i18n from "i18next";
import { initReactI18next } from "react-i18next";
import LanguageDetector from "i18next-browser-languagedetector";
import en from "../locales/en/translation.json";
import de from "../locales/de/translation.json";
import es from "../locales/es/translation.json";
import fr from "../locales/fr/translation.json";
import it from "../locales/it/translation.json";
import ja from "../locales/ja/translation.json";
import ko from "../locales/ko/translation.json";
import nl from "../locales/nl/translation.json";
import pl from "../locales/pl/translation.json";
import pt from "../locales/pt/translation.json";
import ru from "../locales/ru/translation.json";
import sv from "../locales/sv/translation.json";
import tr from "../locales/tr/translation.json";
import zh from "../locales/zh/translation.json";

void i18n
  .use(LanguageDetector)
  .use(initReactI18next)
  .init({
    resources: {
      en: { translation: en },
      de: { translation: de },
      es: { translation: es },
      fr: { translation: fr },
      it: { translation: it },
      ja: { translation: ja },
      ko: { translation: ko },
      nl: { translation: nl },
      pl: { translation: pl },
      pt: { translation: pt },
      ru: { translation: ru },
      sv: { translation: sv },
      tr: { translation: tr },
      zh: { translation: zh },
    },
    fallbackLng: "en",
    interpolation: {
      escapeValue: false, // React already escapes
    },
  });

export default i18n;
