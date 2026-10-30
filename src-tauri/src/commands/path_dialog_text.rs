//! Titles of the file and folder dialogs that choose where the app writes:
//! the log directories and the `SQLite` database.
//!
//! The titles live in the backend rather than in the locale files so that
//! the webview cannot choose what these dialogs say, such as asking the
//! user to pick a folder it would like to write to. They come in the operating
//! system's language for the same reason (see [`system_language`]); a system
//! language without its own titles here gets English.

use super::system_language;

/// One language's dialog titles.
pub(super) struct Titles {
    pub session_logs: &'static str,
    pub app_log: &'static str,
    pub open_database: &'static str,
    pub create_database: &'static str,
}

const ENGLISH: Titles = Titles {
    session_logs: "Choose a folder for session logs",
    app_log: "Choose a folder for the application log",
    open_database: "Choose the session database",
    create_database: "Create a session database",
};

/// Keyed by the codes in `SUPPORTED_LANGUAGES` (src/stores/settingsStore.ts).
static TITLES: [(&str, Titles); 14] = [
    ("en", ENGLISH),
    (
        "de",
        Titles {
            session_logs: "Ordner für Sitzungsprotokolle wählen",
            app_log: "Ordner für das Anwendungsprotokoll wählen",
            open_database: "Sitzungsdatenbank wählen",
            create_database: "Sitzungsdatenbank erstellen",
        },
    ),
    (
        "es",
        Titles {
            session_logs: "Elige una carpeta para los registros de sesión",
            app_log: "Elige una carpeta para el registro de la aplicación",
            open_database: "Elige la base de datos de sesiones",
            create_database: "Crea una base de datos de sesiones",
        },
    ),
    (
        "fr",
        Titles {
            session_logs: "Choisir un dossier pour les journaux de session",
            app_log: "Choisir un dossier pour le journal de l'application",
            open_database: "Choisir la base de données des sessions",
            create_database: "Créer une base de données des sessions",
        },
    ),
    (
        "it",
        Titles {
            session_logs: "Scegli una cartella per i log delle sessioni",
            app_log: "Scegli una cartella per il log dell'applicazione",
            open_database: "Scegli il database delle sessioni",
            create_database: "Crea un database delle sessioni",
        },
    ),
    (
        "ja",
        Titles {
            session_logs: "セッションログの保存先フォルダーを選択",
            app_log: "アプリケーションログの保存先フォルダーを選択",
            open_database: "セッションデータベースを選択",
            create_database: "セッションデータベースを作成",
        },
    ),
    (
        "ko",
        Titles {
            session_logs: "세션 로그 폴더 선택",
            app_log: "애플리케이션 로그 폴더 선택",
            open_database: "세션 데이터베이스 선택",
            create_database: "세션 데이터베이스 만들기",
        },
    ),
    (
        "nl",
        Titles {
            session_logs: "Kies een map voor sessielogs",
            app_log: "Kies een map voor het applicatielog",
            open_database: "Kies de sessiedatabase",
            create_database: "Maak een sessiedatabase aan",
        },
    ),
    (
        "pl",
        Titles {
            session_logs: "Wybierz folder na logi sesji",
            app_log: "Wybierz folder na dziennik aplikacji",
            open_database: "Wybierz bazę danych sesji",
            create_database: "Utwórz bazę danych sesji",
        },
    ),
    (
        "pt",
        Titles {
            session_logs: "Escolha uma pasta para os logs de sessão",
            app_log: "Escolha uma pasta para o log da aplicação",
            open_database: "Escolha o banco de dados de sessões",
            create_database: "Crie um banco de dados de sessões",
        },
    ),
    (
        "ru",
        Titles {
            session_logs: "Выберите папку для журналов сессий",
            app_log: "Выберите папку для журнала приложения",
            open_database: "Выберите базу данных сессий",
            create_database: "Создайте базу данных сессий",
        },
    ),
    (
        "sv",
        Titles {
            session_logs: "Välj en mapp för sessionsloggar",
            app_log: "Välj en mapp för programloggen",
            open_database: "Välj sessionsdatabasen",
            create_database: "Skapa en sessionsdatabas",
        },
    ),
    (
        "tr",
        Titles {
            session_logs: "Oturum günlükleri için bir klasör seçin",
            app_log: "Uygulama günlüğü için bir klasör seçin",
            open_database: "Oturum veritabanını seçin",
            create_database: "Oturum veritabanı oluşturun",
        },
    ),
    (
        "zh",
        Titles {
            session_logs: "选择会话日志文件夹",
            app_log: "选择应用程序日志文件夹",
            open_database: "选择会话数据库",
            create_database: "创建会话数据库",
        },
    ),
];

/// The titles in the operating system's language, or English.
pub(super) fn for_system_language() -> &'static Titles {
    system_language::lookup(&TITLES).unwrap_or(&ENGLISH)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_ui_locale_has_titles() {
        let locales = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../src/locales");
        let mut found = 0;
        for entry in std::fs::read_dir(&locales).expect("src/locales must exist") {
            let entry = entry.unwrap();
            if !entry.file_type().unwrap().is_dir() {
                continue;
            }
            let code = entry.file_name().into_string().unwrap();
            assert!(
                TITLES.iter().any(|(c, _)| *c == code),
                "no dialog titles for locale {code}"
            );
            found += 1;
        }
        assert_eq!(found, TITLES.len(), "titles without a UI locale");
    }

    #[test]
    fn every_language_has_four_distinct_titles() {
        for (code, titles) in &TITLES {
            let all = [
                titles.session_logs,
                titles.app_log,
                titles.open_database,
                titles.create_database,
            ];
            for (i, title) in all.iter().enumerate() {
                assert!(!title.trim().is_empty(), "{code}: empty title");
                assert!(!title.chars().any(char::is_control), "{code}: {title:?}");
                assert!(
                    !all[..i].contains(title),
                    "{code}: repeated title {title:?}"
                );
            }
        }
    }
}
