use crate::outbound::{
    b64_any, build_stream, normalize_ss_method, q, q_or, ss_method_supported, ss_settings,
    trojan_settings, vless_settings, vmess_settings,
};
use percent_encoding::percent_decode_str;
use regex::Regex;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;
use url::Url;

#[derive(Clone, Debug)]
pub struct Proxy {
    pub proto: &'static str,
    pub host: String,
    pub port: u16,
    pub name: String,
    pub base_url: String,
    pub orig_url: String,
    pub settings: Value,
    pub stream: Option<Value>,
    pub country: String,
    pub display: String,
    pub ping: Option<u64>,
    pub speed_kbs: f64,
}

impl Proxy {
    pub fn key(&self) -> String {
        format!(
            "{}|{}",
            self.settings,
            self.stream
                .as_ref()
                .map(|s| s.to_string())
                .unwrap_or_default()
        )
    }
}

#[derive(Debug)]
pub enum ParseErr {
    Unsupported(&'static str),
    Invalid(&'static str),
}

#[derive(Debug, Default)]
pub struct ParseStats {
    pub links: usize,
    pub parsed: usize,
    pub duplicates: usize,
    pub invalid: usize,
    pub invalid_reasons: HashMap<&'static str, usize>,
    pub unsupported: HashMap<&'static str, usize>,
}

fn link_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r#"(?i)\b(?:vless|vmess|trojan|ss|ssr|hysteria2|hy2|tuic|anytls|socks5?|mierus|wireguard|wg)://[^\s"'<>\\]+"#,
        )
        .unwrap()
    })
}

fn clean_link(raw: &str) -> String {
    let mut s: String = raw
        .trim()
        .replace("&amp;", "&")
        .replace(['\u{feff}', '\u{200b}'], "");
    while let Some(last) = s.chars().last() {
        if matches!(last, '"' | '\'' | ',' | ';' | '<' | '>' | '|')
            || (last == ')' && s.matches('(').count() < s.matches(')').count())
            || (last == ']' && s.matches('[').count() < s.matches(']').count())
        {
            s.pop();
        } else {
            break;
        }
    }
    s
}

pub fn extract_links(text: &str) -> Vec<String> {
    link_re()
        .find_iter(text)
        .map(|m| clean_link(m.as_str()))
        .filter(|s| s.contains("://") && s.len() > 8)
        .collect()
}

fn decode_component(s: &str) -> String {
    percent_decode_str(s).decode_utf8_lossy().into_owned()
}

fn params_of(u: &Url) -> HashMap<String, String> {
    u.query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect()
}

fn plausible_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() >= 16
        && id.len() <= 128
        && id
            .chars()
            .all(|c| c.is_ascii_hexdigit() || c == '-' || c == '_' || c == '.')
}

fn host_port(u: &Url, default_port: u16) -> Result<(String, u16), ParseErr> {
    let host = u
        .host_str()
        .ok_or(ParseErr::Invalid("no host"))?
        .to_string();
    let port = u.port().unwrap_or(default_port);
    if host.is_empty() || port == 0 {
        return Err(ParseErr::Invalid("bad host/port"));
    }
    Ok((host, port))
}

const FLOW_ALLOWED: &[&str] = &["", "xtls-rprx-vision"];

fn normalize_flow(raw: &str) -> Result<String, ParseErr> {
    let f = raw.to_ascii_lowercase();
    let f = if f == "xtls-rprx-visi" {
        "xtls-rprx-vision".to_string()
    } else {
        f
    };
    if FLOW_ALLOWED.contains(&f.as_str()) {
        Ok(f)
    } else {
        Err(ParseErr::Invalid("bad flow"))
    }
}

fn parse_vless(link: &str) -> Result<Proxy, ParseErr> {
    let u = Url::parse(link).map_err(|_| ParseErr::Invalid("bad url"))?;
    let (host, port) = host_port(&u, 443)?;
    let id = decode_component(u.username());
    if !plausible_id(&id) {
        return Err(ParseErr::Invalid("bad uuid"));
    }
    let mut params = params_of(&u);

    let encryption = q_or(&params, "encryption", "none").to_ascii_lowercase();
    if !matches!(encryption.as_str(), "none") {
        return Err(ParseErr::Invalid("bad encryption"));
    }
    let security = q_or(&params, "security", "none").to_ascii_lowercase();
    if !matches!(security.as_str(), "tls" | "reality" | "none" | "auto") {
        return Err(ParseErr::Invalid("bad security"));
    }
    let flow = normalize_flow(q_or(&params, "flow", ""))?;
    if !flow.is_empty() && !matches!(security.as_str(), "tls" | "reality") {
        return Err(ParseErr::Invalid("flow without tls"));
    }
    params.insert("flow".into(), flow);
    if security == "reality" {
        let pbk = q(&params, "pbk").unwrap_or("");
        if pbk.is_empty() || b64_any(pbk).map(|v| v.len()) != Some(32) {
            return Err(ParseErr::Invalid("bad reality pbk"));
        }
        if let Some(sid) = q(&params, "sid") {
            if !sid.is_empty()
                && !(sid.len() % 2 == 0
                    && sid.len() <= 16
                    && sid.chars().all(|c| c.is_ascii_hexdigit()))
            {
                return Err(ParseErr::Invalid("bad reality sid"));
            }
        }
        let spx = q_or(&params, "spx", "/");
        if !spx.starts_with('/') {
            return Err(ParseErr::Invalid("bad spiderX"));
        }
    }

    let raw_net = q_or(&params, "type", "tcp").to_ascii_lowercase();
    let net = match raw_net.as_str() {
        "tcp" => "raw",
        "splithttp" => "xhttp",
        "mkcp" => "kcp",
        other => other,
    };
    let stream = build_stream(net, &security, &params, &host)
        .ok_or(ParseErr::Unsupported("vless transport"))?;
    let settings = vless_settings(&host, port, &id, &params);

    Ok(Proxy {
        proto: "vless",
        host,
        port,
        name: decode_component(u.fragment().unwrap_or(""))
            .trim()
            .to_string(),
        base_url: link.split('#').next().unwrap_or(link).to_string(),
        orig_url: link.to_string(),
        settings,
        stream: Some(stream),
        country: String::new(),
        display: String::new(),
        ping: None,
        speed_kbs: 0.0,
    })
}

fn vmess_stream(
    net: &str,
    security: &str,
    params: &HashMap<String, String>,
    host: &str,
) -> Result<Value, ParseErr> {
    build_stream(net, security, params, host).ok_or(ParseErr::Unsupported("vmess transport"))
}

fn parse_vmess(link: &str) -> Result<Proxy, ParseErr> {
    let cleaned = link.replace(['\u{feff}', '\u{200b}', '\n', '\r'], "");

    if cleaned.contains('@') {
        let u = Url::parse(&cleaned).map_err(|_| ParseErr::Invalid("bad url"))?;
        let (host, port) = host_port(&u, 443)?;
        let id = decode_component(u.username());
        if !plausible_id(&id) {
            return Err(ParseErr::Invalid("bad uuid"));
        }
        let params = params_of(&u);
        let aid: i64 = q_or(&params, "aid", "0").parse().unwrap_or(-1);
        if aid < 0 {
            return Err(ParseErr::Invalid("bad aid"));
        }
        let cipher = q_or(&params, "encryption", "auto").to_ascii_lowercase();
        if !matches!(
            cipher.as_str(),
            "auto" | "aes-128-gcm" | "chacha20-poly1305" | "none" | "zero"
        ) {
            return Err(ParseErr::Invalid("bad cipher"));
        }
        let raw_net: String = q_or(&params, "type", "tcp")
            .to_ascii_lowercase()
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .collect();
        let raw_net = if raw_net.is_empty() {
            "tcp".to_string()
        } else {
            raw_net
        };
        let net = if matches!(raw_net.as_str(), "http" | "h2" | "httpupgrade") {
            "xhttp"
        } else {
            raw_net.as_str()
        };
        let security = q_or(&params, "security", "none").to_ascii_lowercase();
        let security = if security.is_empty() {
            "none".to_string()
        } else {
            security
        };
        let stream = vmess_stream(net, &security, &params, &host)?;
        let settings = vmess_settings(&host, port, &id, &cipher);
        return Ok(Proxy {
            proto: "vmess",
            host,
            port,
            name: decode_component(u.fragment().unwrap_or(""))
                .trim()
                .to_string(),
            base_url: link.split('#').next().unwrap_or(link).to_string(),
            orig_url: link.to_string(),
            settings,
            stream: Some(stream),
            country: String::new(),
            display: String::new(),
            ping: None,
            speed_kbs: 0.0,
        });
    }

    let content = cleaned
        .trim_start_matches("vmess://")
        .split('#')
        .next()
        .unwrap_or("")
        .split('?')
        .next()
        .unwrap_or("");
    let bytes = b64_any(content).ok_or(ParseErr::Invalid("bad vmess base64"))?;
    let text = String::from_utf8_lossy(&bytes);
    let data: serde_json::Value =
        serde_json::from_str(&text).map_err(|_| ParseErr::Invalid("bad vmess json"))?;
    let obj = data
        .as_object()
        .ok_or(ParseErr::Invalid("vmess not object"))?;

    let get = |k: &str| -> String {
        obj.get(k)
            .map(|v| match v {
                Value::String(s) => s.clone(),
                Value::Number(n) => n.to_string(),
                Value::Bool(b) => b.to_string(),
                _ => String::new(),
            })
            .unwrap_or_default()
    };

    let id = get("id");
    if !plausible_id(&id) {
        return Err(ParseErr::Invalid("bad uuid"));
    }
    let host = get("add");
    let port: u16 = get("port").parse().unwrap_or(0);
    if host.is_empty() || port == 0 {
        return Err(ParseErr::Invalid("bad address"));
    }
    let cipher = if get("scy").is_empty() {
        "auto".to_string()
    } else {
        get("scy").to_ascii_lowercase()
    };
    if !matches!(
        cipher.as_str(),
        "auto" | "aes-128-gcm" | "chacha20-poly1305" | "none" | "zero"
    ) {
        return Err(ParseErr::Invalid("bad cipher"));
    }
    let raw_net: String = get("net")
        .to_ascii_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();
    let raw_net = if raw_net.is_empty() {
        "tcp".to_string()
    } else {
        raw_net
    };
    let net = if matches!(raw_net.as_str(), "http" | "h2" | "httpupgrade") {
        "xhttp"
    } else {
        raw_net.as_str()
    };
    let security = if get("tls").is_empty() {
        "none".to_string()
    } else {
        get("tls").to_ascii_lowercase()
    };

    let mut params: HashMap<String, String> = HashMap::new();
    for (key, field) in [
        ("path", "path"),
        ("host", "host"),
        ("sni", "sni"),
        ("fp", "fp"),
        ("alpn", "alpn"),
        ("mode", "mode"),
    ] {
        let v = get(field);
        if !v.is_empty() {
            params.insert(key.to_string(), v);
        }
    }

    let stream = vmess_stream(net, &security, &params, &host)?;
    let settings = vmess_settings(&host, port, &id, &cipher);
    let name = obj
        .get("ps")
        .and_then(|v| v.as_str())
        .unwrap_or("vmess")
        .trim()
        .to_string();
    Ok(Proxy {
        proto: "vmess",
        host,
        port,
        name,
        base_url: link.split('#').next().unwrap_or(link).to_string(),
        orig_url: link.to_string(),
        settings,
        stream: Some(stream),
        country: String::new(),
        display: String::new(),
        ping: None,
        speed_kbs: 0.0,
    })
}

fn parse_trojan(link: &str) -> Result<Proxy, ParseErr> {
    let u = Url::parse(link).map_err(|_| ParseErr::Invalid("bad url"))?;
    let (host, port) = host_port(&u, 443)?;
    let password = decode_component(u.username());
    if password.is_empty() {
        return Err(ParseErr::Invalid("empty password"));
    }
    let mut params = params_of(&u);
    if !params.contains_key("sni") {
        if let Some(peer) = params.get("peer").cloned() {
            params.insert("sni".into(), peer);
        }
    }
    let security = q_or(&params, "security", "tls").to_ascii_lowercase();
    if !matches!(security.as_str(), "tls" | "reality") {
        return Err(ParseErr::Invalid("bad security"));
    }
    let raw_net = q_or(&params, "type", "tcp").to_ascii_lowercase();
    let net = match raw_net.as_str() {
        "tcp" => "raw",
        "splithttp" => "xhttp",
        "mkcp" => "kcp",
        other => other,
    };
    let stream = build_stream(net, &security, &params, &host)
        .ok_or(ParseErr::Unsupported("trojan transport"))?;
    let settings = trojan_settings(&host, port, &password, &params);

    Ok(Proxy {
        proto: "trojan",
        host,
        port,
        name: decode_component(u.fragment().unwrap_or(""))
            .trim()
            .to_string(),
        base_url: link.split('#').next().unwrap_or(link).to_string(),
        orig_url: link.to_string(),
        settings,
        stream: Some(stream),
        country: String::new(),
        display: String::new(),
        ping: None,
        speed_kbs: 0.0,
    })
}

fn parse_ss(link: &str) -> Result<Proxy, ParseErr> {
    let without_frag = link.split('#').next().unwrap_or(link);
    let (main, name) = match link.split_once('#') {
        Some((m, n)) => (m, decode_component(n).trim().to_string()),
        None => (without_frag, "ss".to_string()),
    };

    let (method, password, host, port);
    if main.contains('@') {
        let u = Url::parse(main).map_err(|_| ParseErr::Invalid("bad ss url"))?;
        let userinfo = decode_component(u.username());
        let decoded = if userinfo.contains(':') {
            userinfo
        } else {
            let bytes = b64_any(&userinfo).ok_or(ParseErr::Invalid("bad ss userinfo"))?;
            String::from_utf8_lossy(&bytes).into_owned()
        };
        let (m, p) = decoded
            .split_once(':')
            .ok_or(ParseErr::Invalid("ss format"))?;
        method = m.to_string();
        password = p.to_string();
        host = u
            .host_str()
            .ok_or(ParseErr::Invalid("no host"))?
            .to_string();
        port = u.port().unwrap_or(0);
    } else {
        let b64 = main
            .trim_start_matches("ss://")
            .split('?')
            .next()
            .unwrap_or("");
        let bytes = b64_any(b64).ok_or(ParseErr::Invalid("bad ss base64"))?;
        let decoded = String::from_utf8_lossy(&bytes).into_owned();
        let (mp, ap) = decoded
            .rsplit_once('@')
            .ok_or(ParseErr::Invalid("ss format"))?;
        let (m, p) = mp.split_once(':').ok_or(ParseErr::Invalid("ss format"))?;
        method = m.to_string();
        password = p.to_string();
        let (h, pt) = ap.rsplit_once(':').ok_or(ParseErr::Invalid("ss format"))?;
        host = h.trim_start_matches('[').trim_end_matches(']').to_string();
        port = pt.parse().unwrap_or(0);
    }

    if host.is_empty() || port == 0 {
        return Err(ParseErr::Invalid("bad ss address"));
    }
    let method = normalize_ss_method(&method);
    if !ss_method_supported(&method) {
        return Err(ParseErr::Unsupported("ss method"));
    }
    let settings = ss_settings(&host, port, &method, &password);

    Ok(Proxy {
        proto: "shadowsocks",
        host,
        port,
        name,
        base_url: link.split('#').next().unwrap_or(link).to_string(),
        orig_url: link.to_string(),
        settings,
        stream: None,
        country: String::new(),
        display: String::new(),
        ping: None,
        speed_kbs: 0.0,
    })
}

pub fn parse_link(link: &str) -> Result<Proxy, ParseErr> {
    let lower = link.to_ascii_lowercase();
    if lower.starts_with("vless://") {
        parse_vless(link)
    } else if lower.starts_with("vmess://") {
        parse_vmess(link)
    } else if lower.starts_with("trojan://") {
        parse_trojan(link)
    } else if lower.starts_with("ss://") {
        parse_ss(link)
    } else if lower.starts_with("ssr://") {
        Err(ParseErr::Unsupported("ssr"))
    } else if lower.starts_with("hysteria2://")
        || lower.starts_with("hy2://")
        || lower.starts_with("hysteria://")
    {
        Err(ParseErr::Unsupported("hysteria2"))
    } else if lower.starts_with("tuic://") {
        Err(ParseErr::Unsupported("tuic"))
    } else if lower.starts_with("anytls://") {
        Err(ParseErr::Unsupported("anytls"))
    } else {
        Err(ParseErr::Unsupported("other"))
    }
}

pub fn parse_all(links: Vec<String>) -> (Vec<Proxy>, ParseStats) {
    let mut stats = ParseStats {
        links: links.len(),
        ..Default::default()
    };
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<Proxy> = Vec::new();
    for link in links {
        match parse_link(&link) {
            Ok(p) => {
                let key = p.key();
                if seen.insert(key) {
                    out.push(p);
                } else {
                    stats.duplicates += 1;
                }
            }
            Err(ParseErr::Unsupported(proto)) => {
                *stats.unsupported.entry(proto).or_insert(0) += 1;
            }
            Err(ParseErr::Invalid(reason)) => {
                stats.invalid += 1;
                *stats.invalid_reasons.entry(reason).or_insert(0) += 1;
            }
        }
    }
    stats.parsed = out.len();
    (out, stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;
    use serde_json::json;

    const UUID: &str = "01234567-89ab-cdef-0123-456789abcdef";

    fn invalid_reason(result: Result<Proxy, ParseErr>) -> &'static str {
        match result {
            Err(ParseErr::Invalid(reason)) => reason,
            Err(ParseErr::Unsupported(proto)) => {
                panic!("expected invalid input, got unsupported {proto}")
            }
            Ok(proxy) => panic!("expected invalid input, parsed {}", proxy.proto),
        }
    }

    fn unsupported_proto(result: Result<Proxy, ParseErr>) -> &'static str {
        match result {
            Err(ParseErr::Unsupported(proto)) => proto,
            Err(ParseErr::Invalid(reason)) => {
                panic!("expected unsupported protocol, got invalid {reason}")
            }
            Ok(proxy) => panic!("expected unsupported protocol, parsed {}", proxy.proto),
        }
    }

    fn vmess_json_link(value: Value) -> String {
        let encoded = STANDARD.encode(value.to_string());
        format!("vmess://{encoded}")
    }

    #[test]
    fn extract_links_cleans_wrappers_and_preserves_balanced_punctuation() {
        let text = "Links: (VLESS://id@example.com:443?type=tcp#name), ss://abc==; and (trojan://pw@example.net:443/path(a)))";
        assert_eq!(
            extract_links(text),
            vec![
                "VLESS://id@example.com:443?type=tcp#name".to_string(),
                "ss://abc==".to_string(),
                "trojan://pw@example.net:443/path(a)".to_string(),
            ]
        );
    }

    #[test]
    fn extract_links_decodes_html_ampersands_and_removes_invisible_chars() {
        let text = "vless://ab\u{200b}cd@example.com:443?x=1&amp;y=2\u{feff}";
        assert_eq!(
            extract_links(text),
            vec!["vless://abcd@example.com:443?x=1&y=2".to_string()]
        );
    }

    #[test]
    fn extract_links_ignores_non_links_and_trailing_delimiters() {
        assert!(extract_links("https://example.com vless:// ss://\"'").is_empty());
        assert_eq!(
            extract_links("<ss://valid-token|>"),
            vec!["ss://valid-token"]
        );
        assert_eq!(
            extract_links("wg://example.com/path]"),
            ["wg://example.com/path"]
        );
    }

    #[test]
    fn parse_vless_decodes_name_and_applies_defaults() {
        let link = format!("vless://{UUID}@example.com#%20Node%20");
        let proxy = parse_link(&link).unwrap();
        assert_eq!(
            (proxy.proto, proxy.host.as_str(), proxy.port),
            ("vless", "example.com", 443)
        );
        assert_eq!(proxy.name, "Node");
        assert_eq!(proxy.settings["id"], UUID);
        assert_eq!(proxy.stream.as_ref().unwrap()["network"], "raw");
        assert_eq!(proxy.stream.as_ref().unwrap()["security"], "none");
        assert_eq!(proxy.base_url, format!("vless://{UUID}@example.com"));
    }

    #[test]
    fn parse_vless_accepts_reality_and_normalizes_flow_alias() {
        let pbk = STANDARD.encode([7u8; 32]);
        let link = format!(
            "vless://{UUID}@example.com:8443?security=REALITY&pbk={pbk}&sid=a1b2&flow=xtls-rprx-visi&type=tcp#edge"
        );
        let proxy = parse_link(&link).unwrap();
        assert_eq!(proxy.port, 8443);
        assert_eq!(proxy.settings["flow"], "xtls-rprx-vision");
        let stream = proxy.stream.unwrap();
        assert_eq!(stream["network"], "raw");
        assert_eq!(stream["security"], "reality");
        assert_eq!(stream["realitySettings"]["password"], pbk);
    }

    #[test]
    fn parse_vless_checks_uuid_length_and_reality_parameters() {
        for id in ["a".repeat(16), "b".repeat(128)] {
            assert!(parse_link(&format!("vless://{id}@example.com")).is_ok());
        }
        assert_eq!(
            invalid_reason(parse_link(&format!(
                "vless://{}@example.com",
                "a".repeat(129)
            ))),
            "bad uuid"
        );
        assert_eq!(
            invalid_reason(parse_link(&format!(
                "vless://{UUID}@example.com?security=reality&pbk=bad"
            ))),
            "bad reality pbk"
        );
        let pbk = STANDARD.encode([1u8; 32]);
        assert_eq!(
            invalid_reason(parse_link(&format!(
                "vless://{UUID}@example.com?security=reality&pbk={pbk}&sid=xyz"
            ))),
            "bad reality sid"
        );
        assert_eq!(
            invalid_reason(parse_link(&format!(
                "vless://{UUID}@example.com?security=reality&pbk={pbk}&spx=not-absolute"
            ))),
            "bad spiderX"
        );
        assert_eq!(
            invalid_reason(parse_link(&format!(
                "vless://{UUID}@example.com?security=unknown"
            ))),
            "bad security"
        );
        assert_eq!(
            invalid_reason(parse_link(&format!(
                "vless://{UUID}@example.com?flow=not-a-flow&security=tls"
            ))),
            "bad flow"
        );

        for (alias, expected) in [("splithttp", "xhttp"), ("mkcp", "kcp")] {
            let proxy = parse_link(&format!("vless://{UUID}@example.com?type={alias}")).unwrap();
            assert_eq!(proxy.stream.unwrap()["network"], expected);
        }
    }

    #[test]
    fn parse_vmess_json_base64_and_uri_forms() {
        let json_link = vmess_json_link(
            json!({"id": UUID, "add": "vmess.example", "port": 8443, "ps": " json ", "net": "ws", "tls": "tls", "path": "/v"}),
        );
        let json_proxy = parse_link(&json_link).unwrap();
        assert_eq!(
            (json_proxy.proto, json_proxy.host.as_str(), json_proxy.port),
            ("vmess", "vmess.example", 8443)
        );
        assert_eq!(json_proxy.name, "json");
        assert_eq!(json_proxy.stream.as_ref().unwrap()["network"], "ws");

        let uri = format!("vmess://{UUID}@uri.example:443?type=ws&security=tls&path=%2Fws#uri");
        let uri_proxy = parse_link(&uri).unwrap();
        assert_eq!(uri_proxy.name, "uri");
        assert_eq!(uri_proxy.settings["id"], UUID);
        assert_eq!(uri_proxy.stream.unwrap()["network"], "ws");
    }

    #[test]
    fn parse_vmess_json_handles_non_string_fields_and_empty_defaults() {
        let link = vmess_json_link(json!({
            "id": UUID,
            "add": "vmess.example",
            "port": 8443,
            "net": "---",
            "tls": "",
            "path": true,
            "host": null,
            "sni": false,
            "fp": [],
            "alpn": 5,
            "mode": {},
            "ps": "boolean fields"
        }));

        let proxy = parse_link(&link).unwrap();
        assert_eq!(proxy.name, "boolean fields");
        assert_eq!(proxy.stream.as_ref().unwrap()["network"], "raw");
        assert_eq!(proxy.stream.as_ref().unwrap()["security"], "none");

        let alias = vmess_json_link(json!({
            "id": UUID,
            "add": "vmess.example",
            "port": 8443,
            "net": "h2",
            "tls": "tls"
        }));
        let proxy = parse_link(&alias).unwrap();
        assert_eq!(proxy.stream.unwrap()["network"], "xhttp");
    }

    #[test]
    fn parse_vmess_uri_validates_uuid_aid_cipher_and_normalizes_aliases() {
        assert_eq!(
            invalid_reason(parse_link("vmess://short@example.com")),
            "bad uuid"
        );
        assert_eq!(
            invalid_reason(parse_link(&format!("vmess://{UUID}@example.com?aid=-1"))),
            "bad aid"
        );
        assert_eq!(
            invalid_reason(parse_link(&format!(
                "vmess://{UUID}@example.com?encryption=invalid"
            ))),
            "bad cipher"
        );

        let proxy = parse_link(&format!(
            "vmess://{UUID}@example.com?type=httpupgrade&security=#alias"
        ))
        .unwrap();
        assert_eq!(proxy.name, "alias");
        assert_eq!(proxy.stream.unwrap()["network"], "xhttp");

        let empty_type =
            parse_link(&format!("vmess://{UUID}@example.com?type=---&security=tls")).unwrap();
        assert_eq!(empty_type.stream.unwrap()["network"], "raw");
    }

    #[test]
    fn parse_trojan_supports_peer_alias_and_defaults() {
        let proxy =
            parse_link("trojan://secret@example.org?peer=sni.example#%20secure%20").unwrap();
        assert_eq!(
            (proxy.proto, proxy.port, proxy.name.as_str()),
            ("trojan", 443, "secure")
        );
        assert_eq!(proxy.settings["password"], "secret");
        let stream = proxy.stream.unwrap();
        assert_eq!(stream["security"], "tls");
        assert_eq!(stream["tlsSettings"]["serverName"], "sni.example");

        let explicit_sni =
            parse_link("trojan://secret@example.org?sni=explicit.example&peer=ignored.example")
                .unwrap();
        assert_eq!(
            explicit_sni.stream.unwrap()["tlsSettings"]["serverName"],
            "explicit.example"
        );
    }

    #[test]
    fn parse_trojan_handles_transport_aliases_and_unknown_networks() {
        for (alias, expected) in [("splithttp", "xhttp"), ("mkcp", "kcp")] {
            let proxy = parse_link(&format!("trojan://secret@example.org?type={alias}")).unwrap();
            assert_eq!(proxy.stream.unwrap()["network"], expected);
        }
        assert_eq!(
            unsupported_proto(parse_link("trojan://secret@example.org?type=unknown")),
            "trojan transport"
        );
    }

    #[test]
    fn host_port_rejects_missing_hosts_and_port_zero() {
        let no_host = Url::parse("file:///tmp/proxy").unwrap();
        assert!(matches!(
            host_port(&no_host, 443),
            Err(ParseErr::Invalid("no host"))
        ));

        let zero_port = Url::parse("vless://example.com:0").unwrap();
        assert!(matches!(
            host_port(&zero_port, 443),
            Err(ParseErr::Invalid("bad host/port"))
        ));
    }

    #[test]
    fn parse_ss_accepts_plain_userinfo_credentials() {
        let proxy = parse_link("ss://aes-128-gcm%3Aplain@example.org:8388").unwrap();
        assert_eq!(proxy.settings["method"], "aes-128-gcm");
        assert_eq!(proxy.settings["password"], "plain");
    }

    #[test]
    fn parse_ss_supports_userinfo_and_base64_variants() {
        let auth = STANDARD.encode("aes-128-gcm:pass");
        let uri = parse_link(&format!("ss://{auth}@example.org:8388#%20plain%20")).unwrap();
        assert_eq!(
            (uri.proto, uri.host.as_str(), uri.port, uri.name.as_str()),
            ("shadowsocks", "example.org", 8388, "plain")
        );
        assert_eq!(uri.settings["password"], "pass");

        let legacy = format!(
            "ss://{}",
            STANDARD.encode("aes-128-gcm:legacy@[2001:db8::1]:443")
        );
        let proxy = parse_link(&legacy).unwrap();
        assert_eq!(proxy.host, "2001:db8::1");
        assert_eq!(proxy.port, 443);
        assert_eq!(proxy.name, "ss");

        let userinfo = STANDARD.encode("aes-128-gcm:encoded");
        let encoded = parse_link(&format!("ss://{userinfo}@encoded.example:8388")).unwrap();
        assert_eq!(encoded.host, "encoded.example");
        assert_eq!(encoded.settings["password"], "encoded");
    }

    #[test]
    fn parse_link_reports_invalid_boundaries_and_transport_errors() {
        assert_eq!(
            invalid_reason(parse_link("vless://short@example.com")),
            "bad uuid"
        );
        assert_eq!(
            invalid_reason(parse_link(&format!(
                "vless://{UUID}@example.com?encryption=aes"
            ))),
            "bad encryption"
        );
        assert_eq!(
            invalid_reason(parse_link(&format!(
                "vless://{UUID}@example.com?flow=xtls-rprx-vision"
            ))),
            "flow without tls"
        );
        assert_eq!(
            invalid_reason(parse_link("trojan://@example.com")),
            "empty password"
        );
        assert_eq!(
            invalid_reason(parse_link("trojan://pw@example.com?security=none")),
            "bad security"
        );
        assert_eq!(
            unsupported_proto(parse_link(&format!(
                "vless://{UUID}@example.com?type=unknown"
            ))),
            "vless transport"
        );
        assert_eq!(
            invalid_reason(parse_link("vmess://not-base64!")),
            "bad vmess base64"
        );
        assert_eq!(
            invalid_reason(parse_link("ss://not-base64!")),
            "bad ss base64"
        );
        assert_eq!(
            invalid_reason(parse_link(&format!(
                "ss://{}@example.com:0",
                STANDARD.encode("aes-128-gcm:pw")
            ))),
            "bad ss address"
        );
        assert_eq!(
            unsupported_proto(parse_link(&format!(
                "ss://{}@example.com:8388",
                STANDARD.encode("unsupported:pw")
            ))),
            "ss method"
        );
        assert_eq!(invalid_reason(parse_link("vmess://e30=")), "bad uuid");
        assert_eq!(
            invalid_reason(parse_link(&vmess_json_link(json!({"id": UUID})))),
            "bad address"
        );
        assert_eq!(
            invalid_reason(parse_link(&vmess_json_link(
                json!({"id": UUID, "add": "host", "port": 443, "scy": "invalid"})
            ))),
            "bad cipher"
        );
        assert_eq!(invalid_reason(parse_link("ss://YWVz")), "ss format");
    }

    #[test]
    fn parse_vmess_and_legacy_ss_report_malformed_payload_shapes() {
        assert_eq!(
            invalid_reason(parse_link(&format!("vmess://{}", STANDARD.encode("{{")))),
            "bad vmess json"
        );
        assert_eq!(
            invalid_reason(parse_link(&format!("vmess://{}", STANDARD.encode("[]")))),
            "vmess not object"
        );

        for (payload, expected) in [
            ("cipher-only@host:443", "ss format"),
            ("cipher:password@host", "ss format"),
            ("cipher:password@", "ss format"),
        ] {
            let link = format!("ss://{}", STANDARD.encode(payload));
            assert_eq!(invalid_reason(parse_link(&link)), expected, "{payload}");
        }
    }

    #[test]
    fn parse_vmess_reports_unsupported_transport_in_uri_and_json_forms() {
        assert_eq!(
            unsupported_proto(parse_link(&format!(
                "vmess://{UUID}@node.example?type=quic"
            ))),
            "vmess transport"
        );

        let link = vmess_json_link(json!({
            "id": UUID,
            "add": "node.example",
            "port": 443,
            "net": "quic"
        }));
        assert_eq!(unsupported_proto(parse_link(&link)), "vmess transport");
    }

    #[test]
    fn parse_link_classifies_protocols_case_insensitively() {
        for (link, expected) in [
            ("ssr://anything", "ssr"),
            ("hysteria2://anything", "hysteria2"),
            ("hy2://anything", "hysteria2"),
            ("hysteria://anything", "hysteria2"),
            ("tuic://anything", "tuic"),
            ("anytls://anything", "anytls"),
            ("custom://anything", "other"),
        ] {
            assert_eq!(unsupported_proto(parse_link(link)), expected, "{link}");
        }
        assert_eq!(
            parse_link(&format!("VLESS://{UUID}@example.com"))
                .unwrap()
                .proto,
            "vless"
        );
    }

    #[test]
    fn parse_all_deduplicates_by_outbound_and_counts_errors() {
        let vless = format!("vless://{UUID}@example.com#first");
        let same_outbound = format!("vless://{UUID}@example.com#second");
        let distinct = format!("vless://{UUID}@other.example");
        let (proxies, stats) = parse_all(vec![
            vless,
            same_outbound,
            distinct,
            "vless://bad@example.com".into(),
            "tuic://unsupported".into(),
            "hysteria2://unsupported".into(),
        ]);

        assert_eq!(proxies.len(), 2);
        assert_eq!(proxies[0].name, "first");
        assert_eq!(stats.links, 6);
        assert_eq!(stats.parsed, 2);
        assert_eq!(stats.duplicates, 1);
        assert_eq!(stats.invalid, 1);
        assert_eq!(stats.invalid_reasons.get("bad uuid"), Some(&1));
        assert_eq!(stats.unsupported.get("tuic"), Some(&1));
        assert_eq!(stats.unsupported.get("hysteria2"), Some(&1));
    }

    #[test]
    fn parse_all_empty_input_has_zeroed_stats() {
        let (proxies, stats) = parse_all(Vec::new());
        assert!(proxies.is_empty());
        assert_eq!(stats.links, 0);
        assert_eq!(stats.parsed, 0);
        assert_eq!(stats.duplicates, 0);
        assert_eq!(stats.invalid, 0);
        assert!(stats.invalid_reasons.is_empty());
        assert!(stats.unsupported.is_empty());
    }
    #[test]
    fn parse_assertion_helpers_report_unexpected_result_variants() {
        let valid = parse_link(&format!("vless://{UUID}@example.com")).unwrap();
        assert!(std::panic::catch_unwind(|| invalid_reason(Ok(valid.clone()))).is_err());
        assert!(std::panic::catch_unwind(|| {
            invalid_reason(Err(ParseErr::Unsupported("unexpected")))
        })
        .is_err());
        assert!(std::panic::catch_unwind(|| unsupported_proto(Ok(valid))).is_err());
        assert!(std::panic::catch_unwind(|| {
            unsupported_proto(Err(ParseErr::Invalid("unexpected")))
        })
        .is_err());
    }

    #[test]
    fn deterministic_malformed_uri_corpus_never_panics() {
        let mut state = 0x6a09_e667_f3bc_c909u64;
        for case in 0..2_000 {
            let len = (next(&mut state) % 192) as usize;
            let mut input = String::with_capacity(len + 10);
            input.push_str(match case % 4 {
                0 => "vless://",
                1 => "vmess://",
                2 => "trojan://",
                _ => "ss://",
            });
            for _ in 0..len {
                input.push(char::from(next(&mut state) as u8));
            }
            assert!(
                std::panic::catch_unwind(|| parse_link(&input)).is_ok(),
                "case {case}"
            );
        }
    }

    #[test]
    fn deterministic_uri_mutations_keep_parse_and_dedup_accounting_consistent() {
        let base = format!("vless://{UUID}@example.org:443?type=tcp#seed");
        let mut state = 0xbb67_ae85_84ca_a73bu64;
        let mut links = Vec::with_capacity(514);
        for case in 0..512 {
            let mut candidate = base.clone();
            let position = (next(&mut state) as usize) % candidate.len();
            candidate.replace_range(
                position..position + 1,
                &char::from(next(&mut state) as u8).to_string(),
            );
            if case % 7 == 0 {
                candidate.push_str("%GG");
            }
            links.push(candidate);
        }
        links.extend([base.clone(), base]);

        let total = links.len();
        let (proxies, stats) = parse_all(links);
        assert_eq!(stats.links, total);
        assert_eq!(
            stats.parsed
                + stats.duplicates
                + stats.invalid
                + stats.unsupported.values().sum::<usize>(),
            total
        );
        assert_eq!(proxies.len(), stats.parsed);
    }

    fn next(state: &mut u64) -> u64 {
        *state ^= *state << 13;
        *state ^= *state >> 7;
        *state ^= *state << 17;
        *state
    }
}
