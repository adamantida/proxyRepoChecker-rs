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
        .replace('\u{feff}', "")
        .replace('\u{200b}', "");
    loop {
        let Some(last) = s.chars().last() else { break };
        if matches!(last, '"' | '\'' | ',' | ';' | '<' | '>' | '|') {
            s.pop();
        } else if last == ')' && s.matches('(').count() < s.matches(')').count() {
            s.pop();
        } else if last == ']' && s.matches('[').count() < s.matches(']').count() {
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
    percent_decode_str(s)
        .decode_utf8_lossy()
        .into_owned()
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
    let params = params_of(&u);

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
        name: decode_component(u.fragment().unwrap_or("")).trim().to_string(),
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
    let cleaned = link
        .replace('\u{feff}', "")
        .replace('\u{200b}', "")
        .replace('\n', "")
        .replace('\r', "");

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
            name: decode_component(u.fragment().unwrap_or("")).trim().to_string(),
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
        name: decode_component(u.fragment().unwrap_or("")).trim().to_string(),
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
        let (m, p) = decoded.split_once(':').ok_or(ParseErr::Invalid("ss format"))?;
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
        let (mp, ap) = decoded.rsplit_once('@').ok_or(ParseErr::Invalid("ss format"))?;
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
