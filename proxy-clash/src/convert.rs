//! xray settings/stream -> Clash/mihomo proxy (best-effort).

use crate::{num, put, str_val, Source};
use serde_json::Value;
use serde_yaml_ng::{Mapping, Value as Yaml};

/// Преобразует [`Source`] в Clash-proxy. `None` — неизвестный протокол.
pub fn to_clash(s: &Source) -> Option<Mapping> {
    let st = &s.settings;
    let mut m = Mapping::new();
    put(&mut m, "name", str_val(&s.name));

    match s.proto.as_str() {
        "vless" => {
            put(&mut m, "type", str_val("vless"));
            put(&mut m, "server", str_val(&s.server));
            put(&mut m, "port", num(s.port as u64));
            put(&mut m, "uuid", str_val(jstr(st, "id").unwrap_or("")));
            if let Some(f) = jstr(st, "flow").filter(|x| !x.is_empty()) {
                put(&mut m, "flow", str_val(f));
            }
        }
        "vmess" => {
            put(&mut m, "type", str_val("vmess"));
            put(&mut m, "server", str_val(&s.server));
            put(&mut m, "port", num(s.port as u64));
            put(&mut m, "uuid", str_val(jstr(st, "id").unwrap_or("")));
            put(&mut m, "alterId", num(0));
            let cipher = jstr(st, "security")
                .filter(|c| !c.is_empty() && *c != "none")
                .unwrap_or("auto");
            put(&mut m, "cipher", str_val(cipher));
        }
        "trojan" => {
            put(&mut m, "type", str_val("trojan"));
            put(&mut m, "server", str_val(&s.server));
            put(&mut m, "port", num(s.port as u64));
            put(&mut m, "password", str_val(jstr(st, "password").unwrap_or("")));
        }
        "shadowsocks" => {
            put(&mut m, "type", str_val("ss"));
            put(&mut m, "server", str_val(&s.server));
            put(&mut m, "port", num(s.port as u64));
            put(
                &mut m,
                "cipher",
                str_val(&normalize_ss_method(jstr(st, "method").unwrap_or("none"))),
            );
            put(&mut m, "password", str_val(jstr(st, "password").unwrap_or("")));
        }
        _ => return None,
    }

    apply_stream(&mut m, &s.proto, s.stream.as_ref());
    put(&mut m, "udp", Yaml::Bool(true));
    Some(m)
}

/// Переносит tls/reality и транспорт из xray streamSettings в Clash-поля.
fn apply_stream(m: &mut Mapping, proto: &str, stream: Option<&Value>) {
    let net = stream.and_then(|v| jstr(v, "network")).unwrap_or("raw");
    let sec = stream.and_then(|v| jstr(v, "security")).unwrap_or("none");
    // trojan в mihomo использует `sni`, остальные — `servername`.
    let sni_key = if proto == "trojan" { "sni" } else { "servername" };

    match sec {
        "tls" => {
            put(m, "tls", Yaml::Bool(true));
            if let Some(t) = stream.and_then(|v| v.get("tlsSettings")) {
                if let Some(sni) = jstr(t, "serverName").filter(|x| !x.is_empty()) {
                    put(m, sni_key, str_val(sni));
                }
                if let Some(fp) = jstr(t, "fingerprint").filter(|x| !x.is_empty()) {
                    put(m, "client-fingerprint", str_val(fp));
                }
                if jbool(t, "allowInsecure") {
                    put(m, "skip-cert-verify", Yaml::Bool(true));
                }
                if let Some(alpn) = t.get("alpn").and_then(|a| a.as_array()) {
                    let list: Vec<Yaml> = alpn
                        .iter()
                        .filter_map(|x| x.as_str())
                        .filter(|x| !x.is_empty())
                        .map(str_val)
                        .collect();
                    if !list.is_empty() {
                        put(m, "alpn", Yaml::Sequence(list));
                    }
                }
            }
        }
        "reality" => {
            put(m, "tls", Yaml::Bool(true));
            if let Some(r) = stream.and_then(|v| v.get("realitySettings")) {
                if let Some(sni) = jstr(r, "serverName").filter(|x| !x.is_empty()) {
                    put(m, sni_key, str_val(sni));
                }
                let fp = jstr(r, "fingerprint")
                    .filter(|x| !x.is_empty())
                    .unwrap_or("chrome");
                put(m, "client-fingerprint", str_val(fp));
                let mut ro = Mapping::new();
                // В xray публичный ключ reality лежит в поле `password`.
                if let Some(pk) = jstr(r, "password").filter(|x| !x.is_empty()) {
                    put(&mut ro, "public-key", str_val(pk));
                }
                if let Some(sid) = jstr(r, "shortId") {
                    put(&mut ro, "short-id", str_val(sid));
                }
                if !ro.is_empty() {
                    put(m, "reality-opts", Yaml::Mapping(ro));
                }
            }
        }
        _ => {}
    }

    match net {
        "raw" | "tcp" => {}
        "ws" => {
            put(m, "network", str_val("ws"));
            if let Some(ws) = stream.and_then(|v| v.get("wsSettings")) {
                ws_opts(m, ws, false);
            }
        }
        // mihomo изображает v2ray http-upgrade как ws с флагом.
        "httpupgrade" => {
            put(m, "network", str_val("ws"));
            if let Some(ws) = stream.and_then(|v| v.get("httpupgradeSettings")) {
                ws_opts(m, ws, true);
            }
        }
        "grpc" => {
            put(m, "network", str_val("grpc"));
            if let Some(gs) = stream.and_then(|v| v.get("grpcSettings")) {
                if let Some(svc) = jstr(gs, "serviceName").filter(|x| !x.is_empty()) {
                    let mut go = Mapping::new();
                    put(&mut go, "grpc-service-name", str_val(svc));
                    put(m, "grpc-opts", Yaml::Mapping(go));
                }
            }
        }
        "xhttp" => {
            put(m, "network", str_val("xhttp"));
            if let Some(xs) = stream.and_then(|v| v.get("xhttpSettings")) {
                let mut xo = Mapping::new();
                if let Some(path) = jstr(xs, "path").filter(|x| !x.is_empty()) {
                    put(&mut xo, "path", str_val(path));
                }
                if let Some(host) = jstr(xs, "host").filter(|x| !x.is_empty()) {
                    put(&mut xo, "host", str_val(host));
                }
                if let Some(mode) = jstr(xs, "mode").filter(|x| !x.is_empty()) {
                    put(&mut xo, "mode", str_val(mode));
                }
                if !xo.is_empty() {
                    put(m, "xhttp-opts", Yaml::Mapping(xo));
                }
            }
        }
        other => {
            put(m, "network", str_val(other));
        }
    }
}

fn ws_opts(m: &mut Mapping, ws: &Value, v2ray_http_upgrade: bool) {
    let mut o = Mapping::new();
    if let Some(path) = jstr(ws, "path").filter(|x| !x.is_empty()) {
        put(&mut o, "path", str_val(path));
    }
    if let Some(host) = jstr(ws, "host").filter(|x| !x.is_empty()) {
        let mut h = Mapping::new();
        put(&mut h, "Host", str_val(host));
        put(&mut o, "headers", Yaml::Mapping(h));
    }
    if v2ray_http_upgrade {
        put(&mut o, "v2ray-http-upgrade", Yaml::Bool(true));
    }
    if !o.is_empty() {
        put(m, "ws-opts", Yaml::Mapping(o));
    }
}

fn normalize_ss_method(method: &str) -> String {
    match method.trim().to_ascii_lowercase().as_str() {
        "chacha20-poly1305" => "chacha20-ietf-poly1305".to_string(),
        "xchacha20-poly1305" => "xchacha20-ietf-poly1305".to_string(),
        "plain" => "none".to_string(),
        other => other.to_string(),
    }
}

fn jstr<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(|x| x.as_str())
}

fn jbool(v: &Value, key: &str) -> bool {
    v.get(key).and_then(|x| x.as_bool()).unwrap_or(false)
}
