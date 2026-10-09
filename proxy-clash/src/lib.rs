//! Генерация конфига Clash / mihomo из нейтрального описания прокси.
//!
//! Крейт ничего не знает о типах основного проекта: на вход подаются
//! [`Source`] с xray-подобными `settings`/`stream` (как `serde_json::Value`),
//! на выходе — готовый YAML ([`render`]). Никакого I/O — только чистая логика.

mod convert;
mod groups;
mod rules;

use anyhow::Result;
use serde_json::Value;
use serde_yaml_ng::{Mapping, Value as Yaml};
use std::collections::HashSet;

/// Один прокси на входе генератора.
#[derive(Clone, Debug)]
pub struct Source {
    /// vless | vmess | trojan | shadowsocks
    pub proto: String,
    /// Отображаемое имя (например `[DE] - 001`)
    pub name: String,
    pub server: String,
    pub port: u16,
    /// ISO-код страны (`XX`, `CLOUDFLARE`, `RU-BLOCKED`, …)
    pub country: String,
    /// xray outbound settings
    pub settings: Value,
    /// xray streamSettings (может отсутствовать)
    pub stream: Option<Value>,
}

/// Параметры генерации.
#[derive(Clone, Debug)]
pub struct Options {
    /// Создавать группы по странам (`🇽🇽 XX`, `🌐 Прочие`)
    pub include_country: bool,
    /// Создавать группы-категории (Telegram/AI/YouTube/Игры)
    pub include_categories: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            include_country: true,
            include_categories: true,
        }
    }
}

/// Что получилось в итоге.
#[derive(Clone, Debug, Default)]
pub struct Report {
    /// Сколько прокси попало в конфиг
    pub proxies: usize,
    /// Сколько пропущено (неподдерживаемый протокол)
    pub skipped: usize,
    /// Сколько стран сгруппировано
    pub countries: usize,
    /// Сколько групп создано (включая 🚀/⚡/⚖️/🔓)
    pub groups: usize,
}

/// Собирает конфиг Clash/mihomo в строку YAML.
pub fn render(sources: &[Source], opt: &Options) -> Result<(String, Report)> {
    let mut entries: Vec<groups::Entry> = Vec::with_capacity(sources.len());
    let mut proxies: Vec<Yaml> = Vec::with_capacity(sources.len());
    let mut used: HashSet<String> = HashSet::new();
    let mut skipped = 0usize;

    for s in sources {
        let mut m = match convert::to_clash(s) {
            Some(m) => m,
            None => {
                skipped += 1;
                continue;
            }
        };

        let base = if s.name.trim().is_empty() {
            format!("{}:{}", s.server, s.port)
        } else {
            s.name.trim().to_string()
        };
        let mut name = base.clone();
        let mut i = 2;
        while !used.insert(name.clone()) {
            name = format!("{base} #{i}");
            i += 1;
        }
        put(&mut m, "name", str_val(&name));

        entries.push(groups::Entry {
            name,
            country: s.country.clone(),
        });
        proxies.push(Yaml::Mapping(m));
    }

    let (group_list, countries) = groups::build(&entries, opt);
    let groups_count = group_list.len();

    let mut root = Mapping::new();
    rules::insert_globals(&mut root);
    put(
        &mut root,
        "proxy-groups",
        Yaml::Sequence(group_list),
    );
    put(&mut root, "proxies", Yaml::Sequence(proxies));
    put(
        &mut root,
        "rules",
        rules::rules(opt.include_categories),
    );

    let body = serde_yaml_ng::to_string(&Yaml::Mapping(root))?;
    let header = format!(
        "# Сгенерировано proxy-rs (proxy-clash)\n# прокси: {} | групп: {} | стран: {}\n",
        entries.len(),
        groups_count,
        countries
    );

    Ok((
        format!("{header}{body}"),
        Report {
            proxies: entries.len(),
            skipped,
            countries,
            groups: groups_count,
        },
    ))
}

// ------------------------- вспомогательные API -------------------------

pub(crate) fn put(m: &mut Mapping, key: &str, value: Yaml) {
    m.insert(Yaml::String(key.to_string()), value);
}

pub(crate) fn str_val(s: &str) -> Yaml {
    Yaml::String(s.to_string())
}

pub(crate) fn num(n: u64) -> Yaml {
    Yaml::Number(serde_yaml_ng::Number::from(n))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn vless(name: &str, country: &str, net: &str, sec: &str, stream_extra: Value) -> Source {
        let mut stream = json!({ "network": net, "security": sec });
        if sec == "reality" {
            stream["realitySettings"] = json!({
                "password": "PBK",
                "shortId": "abcd",
                "serverName": "example.com",
                "fingerprint": "chrome",
            });
        } else if sec == "tls" {
            stream["tlsSettings"] = json!({
                "serverName": "sni.example",
                "fingerprint": "firefox",
                "allowInsecure": true,
            });
        }
        if let Value::Object(extra) = stream_extra {
            for (k, v) in extra {
                stream[k] = v;
            }
        }
        Source {
            proto: "vless".into(),
            name: name.into(),
            server: "1.2.3.4".into(),
            port: 443,
            country: country.into(),
            settings: json!({ "address": "1.2.3.4", "port": 443, "id": "uuid-1", "flow": "xtls-rprx-vision" }),
            stream: Some(stream),
        }
    }

    #[test]
    fn renders_valid_yaml_with_groups() {
        let src = vec![
            vless("[DE] - 001", "DE", "raw", "reality", json!({})),
            vless("[DE] - 002", "DE", "ws", "tls", json!({ "wsSettings": { "path": "/x", "host": "h.example" } })),
            vless("[RU] - 001", "RU", "grpc", "tls", json!({ "grpcSettings": { "serviceName": "svc" } })),
        ];
        let (yaml, rep) = render(&src, &Options::default()).unwrap();
        assert_eq!(rep.proxies, 3);
        assert_eq!(rep.skipped, 0);
        assert!(rep.groups >= 6, "групп: {}", rep.groups);

        let parsed: serde_yaml_ng::Value = serde_yaml_ng::from_str(&yaml).unwrap();
        assert!(parsed.get("proxies").unwrap().as_sequence().unwrap().len() == 3);
        assert!(parsed.get("proxy-groups").is_some());
        assert!(parsed.get("rules").is_some());
        assert!(yaml.contains("reality-opts"));
        assert!(yaml.contains("public-key: PBK"));
        assert!(yaml.contains("ws-opts"));
        assert!(yaml.contains("grpc-service-name: svc"));
    }

    #[test]
    fn trojan_uses_sni() {
        let mut s = vless("[XX] - 001", "XX", "ws", "tls", json!({}));
        s.proto = "trojan".into();
        s.settings = json!({ "address": "1.2.3.4", "port": 443, "password": "pw" });
        let (yaml, _) = render(&[s], &Options::default()).unwrap();
        assert!(yaml.contains("sni: sni.example"), "{yaml}");
        assert!(!yaml.contains("servername: sni.example"));
    }

    #[test]
    fn skips_unknown_proto() {
        let mut s = vless("[XX] - 001", "XX", "raw", "none", json!({}));
        s.proto = "hysteria2".into();
        let (_, rep) = render(&[s], &Options::default()).unwrap();
        assert_eq!(rep.proxies, 0);
        assert_eq!(rep.skipped, 1);
    }

    #[test]
    fn dedupes_names() {
        let a = vless("dup", "DE", "raw", "none", json!({}));
        let b = vless("dup", "DE", "raw", "none", json!({}));
        let (yaml, rep) = render(&[a, b], &Options::default()).unwrap();
        assert_eq!(rep.proxies, 2);
        assert!(yaml.contains("dup #2"));
    }

    #[test]
    fn toggles_remove_groups() {
        let src = vec![vless("[DE] - 001", "DE", "raw", "none", json!({}))];
        let (yaml, rep) = render(
            &src,
            &Options {
                include_country: false,
                include_categories: false,
            },
        )
        .unwrap();
        assert_eq!(rep.countries, 0);
        assert_eq!(rep.groups, 4);
        assert!(!yaml.contains("Telegram"));
    }
}
