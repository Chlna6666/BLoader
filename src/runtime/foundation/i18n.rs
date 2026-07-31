use std::collections::HashMap;
use std::sync::OnceLock;

use parking_lot::RwLock;

use crate::config::Config;
use crate::runtime::foundation::logging;

static I18N: OnceLock<I18nService> = OnceLock::new();
const DEFAULT_LOCALE: &str = "zh_CN";
type LangMap = HashMap<String, String>;
type LocaleTranslations = HashMap<String, LangMap>;
type ModTranslations = HashMap<String, LocaleTranslations>;

pub fn init(config: &Config) {
    let service = I18N.get_or_init(I18nService::new);
    service.configure(config);
}

pub fn tr(key: &str) -> String {
    I18N.get()
        .and_then(|service| service.translate(key))
        .unwrap_or_else(|| key.to_string())
}

pub fn current_locale() -> String {
    I18N.get()
        .map(I18nService::current_locale)
        .unwrap_or_else(|| DEFAULT_LOCALE.to_string())
}

pub fn tr_for(owner_name: &str, key: &str) -> String {
    I18N.get()
        .and_then(|service| service.translate_for(owner_name, key))
        .unwrap_or_else(|| key.to_string())
}

/// Mods register already-embedded language text directly through the SDK.
/// No language file is extracted to disk.
pub fn register_mod_lang(owner_name: &str, locale: &str, content: &str) -> bool {
    I18N.get()
        .map(|service| service.register_mod_lang(owner_name, locale, content))
        .unwrap_or(false)
}

struct I18nService {
    locale: RwLock<String>,
    translations: RwLock<LangMap>,
    mod_translations: RwLock<ModTranslations>,
}

impl I18nService {
    fn new() -> Self {
        Self {
            locale: RwLock::new(DEFAULT_LOCALE.to_string()),
            translations: RwLock::new(HashMap::new()),
            mod_translations: RwLock::new(HashMap::new()),
        }
    }

    fn configure(&self, config: &Config) {
        let locale = normalize_locale_name(&config.default_locale);
        let mut map = bundled_lang_map(DEFAULT_LOCALE);
        if locale != DEFAULT_LOCALE {
            map.extend(bundled_lang_map(&locale));
        }

        *self.locale.write() = locale.clone();
        *self.translations.write() = map;
        logging::info_message(&format!(
            "i18n ready: locale={} source=embedded-dll hot_reload=disabled",
            locale
        ));
    }

    fn translate(&self, key: &str) -> Option<String> {
        self.translations.read().get(key).cloned()
    }

    fn translate_for(&self, owner_name: &str, key: &str) -> Option<String> {
        let locale = self.current_locale();
        let normalized_owner = owner_name.trim();
        if !normalized_owner.is_empty() {
            let mod_translations = self.mod_translations.read();
            if let Some(locales) = mod_translations.get(normalized_owner) {
                if let Some(value) = locales
                    .get(&locale)
                    .and_then(|translations| translations.get(key))
                    .cloned()
                {
                    return Some(value);
                }
                if locale != DEFAULT_LOCALE {
                    if let Some(value) = locales
                        .get(DEFAULT_LOCALE)
                        .and_then(|translations| translations.get(key))
                        .cloned()
                    {
                        return Some(value);
                    }
                }
            }
        }
        self.translate(key)
    }

    fn current_locale(&self) -> String {
        self.locale.read().clone()
    }

    fn register_mod_lang(&self, owner_name: &str, locale: &str, content: &str) -> bool {
        let owner_name = owner_name.trim();
        if owner_name.is_empty() {
            return false;
        }

        let locale = normalize_locale_name(locale);
        let translations = parse_lang_map(content);
        if translations.is_empty() {
            logging::warn_message(&format!(
                "i18n mod locale ignored: owner={} locale={} entries=0",
                owner_name, locale
            ));
            return false;
        }

        let entry_count = translations.len();
        self.mod_translations
            .write()
            .entry(owner_name.to_string())
            .or_default()
            .insert(locale.clone(), translations);
        logging::info_message(&format!(
            "i18n mod locale registered: owner={} locale={} entries={} source=memory",
            owner_name, locale, entry_count
        ));
        true
    }
}

fn normalize_locale_name(locale: &str) -> String {
    let locale = locale.trim();
    if locale.is_empty() {
        return DEFAULT_LOCALE.to_string();
    }

    let normalized = locale.replace('-', "_");
    let mut parts = normalized.split('_');
    let language = parts.next().unwrap_or("zh").to_ascii_lowercase();
    if let Some(region) = parts.next() {
        format!("{}_{}", language, region.to_ascii_uppercase())
    } else {
        language
    }
}

fn bundled_lang_map(locale: &str) -> LangMap {
    match normalize_locale_name(locale).as_str() {
        "en_US" => parse_lang_map(include_str!("../../../resources/lang/en_US.lang")),
        _ => parse_lang_map(include_str!("../../../resources/lang/zh_CN.lang")),
    }
}

fn parse_lang_map(content: &str) -> LangMap {
    let mut map = HashMap::new();
    for raw_line in content.lines() {
        let line = raw_line.trim_start_matches('\u{feff}').trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if !key.is_empty() {
            map.insert(key.to_string(), value.trim().replace("\\n", "\n"));
        }
    }
    map
}
