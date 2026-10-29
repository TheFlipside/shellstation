//! Wording of the dialog that asks before the private-address check is
//! switched off, in every language the UI ships.
//!
//! The text lives in the backend rather than in the locale files, because
//! the webview must not be able to choose what the dialog says. It only
//! names a language; anything unknown gets English.

/// One language's dialog text. `keep` is the default button and leaves the
/// check on, `allow` switches it off, and `cancel` is what closing the
/// dialog counts as. All three must differ, since the dialog result is told
/// apart by the button's label.
pub(super) struct Prompt {
    pub title: &'static str,
    pub message: &'static str,
    pub keep: &'static str,
    pub allow: &'static str,
    pub cancel: &'static str,
}

const ENGLISH: Prompt = Prompt {
    title: "Stop blocking private addresses?",
    message: "SSH and Telnet connections will be able to reach loopback (127.x), link-local \
              and private (10.x, 172.16-31.x, 192.168.x) addresses again.\n\n\
              If you did not just turn this setting off yourself, choose \u{201c}Keep blocking\u{201d}.",
    allow: "Stop blocking",
    keep: "Keep blocking",
    cancel: "Cancel",
};

/// Keyed by the codes in `SUPPORTED_LANGUAGES` (src/stores/settingsStore.ts).
static PROMPTS: [(&str, Prompt); 14] = [
    ("en", ENGLISH),
    (
        "de",
        Prompt {
            title: "Private Adressen nicht mehr blockieren?",
            message: "SSH- und Telnet-Verbindungen können dann wieder Loopback- (127.x), \
                      Link-Local- und private Adressen (10.x, 172.16-31.x, 192.168.x) erreichen.\n\n\
                      Wenn Sie diese Einstellung nicht gerade selbst ausgeschaltet haben, wählen Sie \
                      \u{201e}Weiter blockieren\u{201c}.",
            allow: "Nicht mehr blockieren",
            keep: "Weiter blockieren",
            cancel: "Abbrechen",
        },
    ),
    (
        "es",
        Prompt {
            title: "¿Dejar de bloquear las direcciones privadas?",
            message: "Las conexiones SSH y Telnet podrán volver a alcanzar direcciones loopback \
                      (127.x), link-local y privadas (10.x, 172.16-31.x, 192.168.x).\n\n\
                      Si no has desactivado tú este ajuste hace un momento, elige \
                      \u{ab}Seguir bloqueando\u{bb}.",
            allow: "Dejar de bloquear",
            keep: "Seguir bloqueando",
            cancel: "Cancelar",
        },
    ),
    (
        "fr",
        Prompt {
            title: "Ne plus bloquer les adresses privées ?",
            message: "Les connexions SSH et Telnet pourront de nouveau atteindre les adresses \
                      loopback (127.x), link-local et privées (10.x, 172.16-31.x, 192.168.x).\n\n\
                      Si vous ne venez pas de désactiver vous-même ce paramètre, choisissez \
                      \u{ab}\u{a0}Continuer à bloquer\u{a0}\u{bb}.",
            allow: "Ne plus bloquer",
            keep: "Continuer à bloquer",
            cancel: "Annuler",
        },
    ),
    (
        "it",
        Prompt {
            title: "Smettere di bloccare gli indirizzi privati?",
            message: "Le connessioni SSH e Telnet potranno di nuovo raggiungere indirizzi loopback \
                      (127.x), link-local e privati (10.x, 172.16-31.x, 192.168.x).\n\n\
                      Se non hai appena disattivato tu questa impostazione, scegli \
                      \u{ab}Continua a bloccare\u{bb}.",
            allow: "Smetti di bloccare",
            keep: "Continua a bloccare",
            cancel: "Annulla",
        },
    ),
    (
        "ja",
        Prompt {
            title: "プライベートアドレスのブロックを解除しますか？",
            message: "SSH および Telnet の接続で、再びループバック (127.x)、リンクローカル、\
                      プライベートアドレス (10.x, 172.16-31.x, 192.168.x) にアクセスできるようになります。\n\n\
                      この設定をご自身でオフにしたのでなければ、「ブロックを続ける」を選択してください。",
            allow: "ブロックを解除",
            keep: "ブロックを続ける",
            cancel: "キャンセル",
        },
    ),
    (
        "ko",
        Prompt {
            title: "사설 주소 차단을 해제하시겠습니까?",
            message: "SSH 및 Telnet 연결이 다시 루프백(127.x), 링크로컬, 사설 주소\
                      (10.x, 172.16-31.x, 192.168.x)에 연결할 수 있게 됩니다.\n\n\
                      이 설정을 방금 직접 끈 것이 아니라면 \u{2018}계속 차단\u{2019}을 선택하세요.",
            allow: "차단 해제",
            keep: "계속 차단",
            cancel: "취소",
        },
    ),
    (
        "nl",
        Prompt {
            title: "Privé-adressen niet meer blokkeren?",
            message: "SSH- en Telnet-verbindingen kunnen dan weer loopback- (127.x), link-local- en \
                      privé-adressen (10.x, 172.16-31.x, 192.168.x) bereiken.\n\n\
                      Als u deze instelling niet zojuist zelf hebt uitgeschakeld, kies dan \
                      \u{201c}Blijven blokkeren\u{201d}.",
            allow: "Niet meer blokkeren",
            keep: "Blijven blokkeren",
            cancel: "Annuleren",
        },
    ),
    (
        "pl",
        Prompt {
            title: "Przestać blokować adresy prywatne?",
            message: "Połączenia SSH i Telnet znów będą mogły docierać do adresów loopback (127.x), \
                      link-local i prywatnych (10.x, 172.16-31.x, 192.168.x).\n\n\
                      Jeśli to ustawienie nie zostało przed chwilą wyłączone przez ciebie, wybierz \
                      \u{201e}Nadal blokuj\u{201d}.",
            allow: "Przestań blokować",
            keep: "Nadal blokuj",
            cancel: "Anuluj",
        },
    ),
    (
        "pt",
        Prompt {
            title: "Deixar de bloquear endereços privados?",
            message: "As conexões SSH e Telnet poderão voltar a alcançar endereços loopback (127.x), \
                      link-local e privados (10.x, 172.16-31.x, 192.168.x).\n\n\
                      Se não foi você quem acabou de desativar esta configuração, escolha \
                      \u{201c}Continuar bloqueando\u{201d}.",
            allow: "Deixar de bloquear",
            keep: "Continuar bloqueando",
            cancel: "Cancelar",
        },
    ),
    (
        "ru",
        Prompt {
            title: "Перестать блокировать частные адреса?",
            message: "SSH- и Telnet-подключения снова смогут обращаться к loopback (127.x), \
                      link-local и частным адресам (10.x, 172.16-31.x, 192.168.x).\n\n\
                      Если вы не отключали этот параметр только что сами, выберите \
                      \u{ab}Продолжить блокировку\u{bb}.",
            allow: "Перестать блокировать",
            keep: "Продолжить блокировку",
            cancel: "Отмена",
        },
    ),
    (
        "sv",
        Prompt {
            title: "Sluta blockera privata adresser?",
            message: "SSH- och Telnet-anslutningar kommer återigen att kunna nå loopback- (127.x), \
                      link-local- och privata adresser (10.x, 172.16-31.x, 192.168.x).\n\n\
                      Om du inte precis själv stängde av den här inställningen väljer du \
                      \u{201d}Fortsätt blockera\u{201d}.",
            allow: "Sluta blockera",
            keep: "Fortsätt blockera",
            cancel: "Avbryt",
        },
    ),
    (
        "tr",
        Prompt {
            title: "Özel adreslerin engellenmesi durdurulsun mu?",
            message: "SSH ve Telnet bağlantıları yeniden loopback (127.x), link-local ve özel \
                      (10.x, 172.16-31.x, 192.168.x) adreslere ulaşabilecek.\n\n\
                      Bu ayarı az önce kendiniz kapatmadıysanız \u{201c}Engellemeye devam et\u{201d} \
                      seçeneğini seçin.",
            allow: "Engellemeyi durdur",
            keep: "Engellemeye devam et",
            cancel: "İptal",
        },
    ),
    (
        "zh",
        Prompt {
            title: "不再阻止私有地址？",
            message: "SSH 和 Telnet 连接将可以再次访问回环 (127.x)、链路本地和私有地址 \
                      (10.x, 172.16-31.x, 192.168.x)。\n\n\
                      如果这不是您刚刚亲自关闭的设置，请选择\u{201c}继续阻止\u{201d}。",
            allow: "不再阻止",
            keep: "继续阻止",
            cancel: "取消",
        },
    ),
];

/// The prompt for a UI language such as `"de"`. A region (`"pt-BR"`) is
/// ignored, and a language without its own text gets English.
pub(super) fn for_language(language: &str) -> &'static Prompt {
    let primary = language.split(['-', '_']).next().unwrap_or_default();
    PROMPTS
        .iter()
        .find(|(code, _)| code.eq_ignore_ascii_case(primary))
        .map_or(&ENGLISH, |(_, prompt)| prompt)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_ui_locale_has_a_prompt() {
        let locales = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../src/locales");
        let mut found = 0;
        for entry in std::fs::read_dir(&locales).expect("src/locales must exist") {
            let entry = entry.unwrap();
            if !entry.file_type().unwrap().is_dir() {
                continue;
            }
            let code = entry.file_name().into_string().unwrap();
            assert!(
                PROMPTS.iter().any(|(c, _)| *c == code),
                "no private-address prompt for locale {code}"
            );
            found += 1;
        }
        assert_eq!(found, PROMPTS.len(), "a prompt without a UI locale");
    }

    #[test]
    fn every_prompt_is_complete_and_its_buttons_differ() {
        for (code, prompt) in &PROMPTS {
            for text in [
                prompt.title,
                prompt.message,
                prompt.keep,
                prompt.allow,
                prompt.cancel,
            ] {
                assert!(!text.trim().is_empty(), "{code}: empty text");
            }
            assert_ne!(prompt.allow, prompt.keep, "{code}: identical buttons");
            assert_ne!(prompt.allow, prompt.cancel, "{code}: identical buttons");
            assert_ne!(prompt.keep, prompt.cancel, "{code}: identical buttons");
            // A line continuation that lost its `\` leaves runs of spaces or
            // extra line breaks in the text.
            assert!(!prompt.message.contains("  "), "{code}: double space");
            assert_eq!(
                prompt.message.matches('\n').count(),
                2,
                "{code}: the message is two paragraphs"
            );
            assert!(
                prompt.message.contains(prompt.keep),
                "{code}: the message must name the button that keeps the check on"
            );
        }
    }

    #[test]
    fn language_lookup_ignores_region_and_case() {
        assert_eq!(for_language("de").allow, "Nicht mehr blockieren");
        assert_eq!(for_language("pt-BR").allow, "Deixar de bloquear");
        assert_eq!(for_language("ZH_cn").allow, "不再阻止");
    }

    #[test]
    fn unknown_language_gets_english() {
        for language in ["", "xx", "-", "\u{202e}de"] {
            assert_eq!(for_language(language).allow, ENGLISH.allow);
        }
    }
}
