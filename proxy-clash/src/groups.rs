//! proxy-groups: 🚀 Прокси / ⚡ Авто / ⚖️ Баланс / 🌍 Страны / 🔓 Заблокированные,
//! группы по странам (включая псевдо-страны вроде Cloudflare/RU-BLOCKED) и категории.

use crate::{num, put, str_val, Options};
use serde_yaml_ng::{Mapping, Value as Yaml};
use std::collections::BTreeMap;

/// Прокси для группировки.
pub struct Entry {
    pub name: String,
    pub country: String,
}

const AUTO: &str = "⚡ Авто";
const BALANCE: &str = "⚖️ Баланс";
const MAIN: &str = "🚀 Прокси";
const COUNTRIES: &str = "🌍 Страны";
const BLOCKED: &str = "🔓 Заблокированные";
const OTHER: &str = "🌐 Прочие";
const CATEGORIES: [&str; 4] = ["💬 Telegram", "🤖 AI", "📺 YouTube", "🎮 Игры"];
const TEST_URL: &str = "http://cp.cloudflare.com/generate_204";

/// Возвращает список групп и число стран.
pub fn build(entries: &[Entry], opt: &Options) -> (Vec<Yaml>, usize) {
    let all: Vec<Yaml> = entries.iter().map(|e| str_val(&e.name)).collect();

    let (country_groups, country_names, countries) = if opt.include_country {
        build_countries(entries)
    } else {
        (Vec::new(), Vec::new(), 0)
    };
    let has_countries = !country_names.is_empty();

    let mut groups: Vec<Yaml> = Vec::new();

    // 🚀 Прокси: ручной выбор верхнего уровня.
    let mut main_members = vec![str_val("DIRECT"), str_val(AUTO), str_val(BALANCE)];
    if has_countries {
        main_members.push(str_val(COUNTRIES));
    }
    main_members.push(str_val(BLOCKED));
    groups.push(select(MAIN, main_members));

    groups.push(url_test(AUTO, all.clone()));
    groups.push(load_balance(BALANCE, all));

    // 🌍 Страны: ручная (Manual) группа со всеми странами/псевдо-странами.
    if has_countries {
        groups.push(select(COUNTRIES, country_names.clone()));
    }

    let mut blocked_members = vec![str_val(AUTO), str_val(BALANCE)];
    if has_countries {
        blocked_members.push(str_val(COUNTRIES));
    }
    groups.push(select(BLOCKED, blocked_members));

    groups.extend(country_groups);

    if opt.include_categories {
        let mut base = vec![
            str_val(MAIN),
            str_val(AUTO),
            str_val(BALANCE),
        ];
        if has_countries {
            base.push(str_val(COUNTRIES));
        }
        base.push(str_val(BLOCKED));
        for name in CATEGORIES {
            groups.push(select(name, base.clone()));
        }
    }

    (groups, countries)
}

fn build_countries(entries: &[Entry]) -> (Vec<Yaml>, Vec<Yaml>, usize) {
    let mut buckets: BTreeMap<String, Vec<Yaml>> = BTreeMap::new();
    for e in entries {
        buckets
            .entry(country_group(&e.country))
            .or_default()
            .push(str_val(&e.name));
    }

    let mut groups = Vec::with_capacity(buckets.len());
    let mut names = Vec::with_capacity(buckets.len());
    for (name, members) in buckets {
        names.push(str_val(&name));
        groups.push(url_test(&name, members));
    }
    let count = groups.len();
    (groups, names, count)
}

fn country_group(code: &str) -> String {
    let up = code.trim().to_ascii_uppercase();
    // `XX` = страна не определена — в общую группу, без фейкового флага.
    if up.is_empty() || up == "XX" {
        return OTHER.to_string();
    }
    if let Some(f) = flag(&up) {
        return format!("{f} {up}");
    }
    // Псевдо-страны из geoip.dat — отдельная группа, как у настоящих стран.
    let icon = match up.as_str() {
        "CLOUDFLARE" => "☁️",
        "RU-BLOCKED" => "🔒",
        _ => "🏳️",
    };
    format!("{icon} {up}")
}

/// ISO alpha-2 -> эмодзи-флаг (региональные индикаторы).
fn flag(code: &str) -> Option<String> {
    let b = code.as_bytes();
    if b.len() != 2 || !b.iter().all(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    let mut s = String::with_capacity(8);
    for c in b {
        s.push(char::from_u32(0x1F1E6 + (c.to_ascii_uppercase() - b'A') as u32)?);
    }
    Some(s)
}

fn select(name: &str, proxies: Vec<Yaml>) -> Yaml {
    let mut m = Mapping::new();
    put(&mut m, "name", str_val(name));
    put(&mut m, "type", str_val("select"));
    put(&mut m, "proxies", Yaml::Sequence(proxies));
    Yaml::Mapping(m)
}

fn url_test(name: &str, proxies: Vec<Yaml>) -> Yaml {
    let mut m = Mapping::new();
    put(&mut m, "name", str_val(name));
    put(&mut m, "type", str_val("url-test"));
    put(&mut m, "url", str_val(TEST_URL));
    put(&mut m, "interval", num(300));
    put(&mut m, "tolerance", num(50));
    put(&mut m, "lazy", Yaml::Bool(false));
    put(&mut m, "proxies", Yaml::Sequence(proxies));
    Yaml::Mapping(m)
}

fn load_balance(name: &str, proxies: Vec<Yaml>) -> Yaml {
    let mut m = Mapping::new();
    put(&mut m, "name", str_val(name));
    put(&mut m, "type", str_val("load-balance"));
    put(&mut m, "strategy", str_val("consistent-hashing"));
    put(&mut m, "url", str_val(TEST_URL));
    put(&mut m, "interval", num(300));
    put(&mut m, "proxies", Yaml::Sequence(proxies));
    Yaml::Mapping(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries() -> Vec<Entry> {
        vec![
            Entry {
                name: "[DE] - 001".into(),
                country: "DE".into(),
            },
            Entry {
                name: "[DE] - 002".into(),
                country: "DE".into(),
            },
            Entry {
                name: "[XX] - 001".into(),
                country: "XX".into(),
            },
            Entry {
                name: "[CF] - 001".into(),
                country: "CLOUDFLARE".into(),
            },
        ]
    }

    #[test]
    fn creates_country_and_category_groups() {
        let (groups, countries) = build(&entries(), &Options::default());
        // 🇩🇪 DE + ☁️ CLOUDFLARE + 🌐 Прочие (XX)
        assert_eq!(countries, 3);
        // 🚀 + ⚡ + ⚖️ + 🌍 Страны + 🔓 + 3 страны + 4 категории
        assert_eq!(groups.len(), 5 + 3 + 4);
    }

    #[test]
    fn has_countries_manual_group() {
        let (groups, _) = build(&entries(), &Options::default());
        let countries_group = groups
            .iter()
            .find(|g| g.get("name").and_then(|n| n.as_str()) == Some("🌍 Страны"))
            .expect("нет группы 🌍 Страны");
        let members = countries_group
            .get("proxies")
            .and_then(|p| p.as_sequence())
            .unwrap();
        let names: Vec<&str> = members.iter().filter_map(|v| v.as_str()).collect();
        assert!(names.contains(&"🇩🇪 DE"));
        assert!(names.contains(&"☁️ CLOUDFLARE"));
        assert!(names.contains(&"🌐 Прочие"));
        assert_eq!(countries_group.get("type").and_then(|t| t.as_str()), Some("select"));
    }

    #[test]
    fn cloudflare_and_ru_blocked_get_own_groups() {
        let src = vec![
            Entry {
                name: "[CF] - 001".into(),
                country: "CLOUDFLARE".into(),
            },
            Entry {
                name: "[RB] - 001".into(),
                country: "RU-BLOCKED".into(),
            },
            Entry {
                name: "[ZZ] - 001".into(),
                country: "RE-FILTER".into(),
            },
        ];
        let (groups, countries) = build(&src, &Options::default());
        assert_eq!(countries, 3);
        let names: Vec<String> = groups
            .iter()
            .filter_map(|g| g.get("name").and_then(|n| n.as_str()))
            .map(str::to_string)
            .collect();
        assert!(names.contains(&"☁️ CLOUDFLARE".to_string()));
        assert!(names.contains(&"🔒 RU-BLOCKED".to_string()));
        assert!(names.contains(&"🏳️ RE-FILTER".to_string()));
    }

    #[test]
    fn flag_for_iso_code() {
        assert_eq!(flag("de").unwrap(), "🇩🇪");
        assert!(flag("XX").is_some());
        assert!(flag("CLOUDFLARE").is_none());
        assert!(flag("").is_none());
    }

    #[test]
    fn only_unknown_go_to_other() {
        let (_, countries) = build(&entries(), &Options::default());
        assert_eq!(countries, 3);
    }

    #[test]
    fn no_country_no_categories() {
        let (groups, countries) = build(
            &entries(),
            &Options {
                include_country: false,
                include_categories: false,
            },
        );
        assert_eq!(countries, 0);
        assert_eq!(groups.len(), 4);
        assert!(groups
            .iter()
            .all(|g| g.get("name").and_then(|n| n.as_str()) != Some("🌍 Страны")));
    }
}
