use serde_json::{json, Map, Value};
use std::collections::HashMap;

pub fn b64_any(s: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    let padded = if t.len() % 4 == 0 {
        t.to_string()
    } else {
        format!("{}{}", t, "=".repeat(4 - t.len() % 4))
    };
    base64::engine::general_purpose::STANDARD
        .decode(&padded)
        .ok()
        .or_else(|| base64::engine::general_purpose::URL_SAFE.decode(&padded).ok())
}

pub fn q<'a>(map: &'a HashMap<String, String>, key: &str) -> Option<&'a str> {
    map.get(key).map(|s| s.as_str())
}

pub fn q_or<'a>(map: &'a HashMap<String, String>, key: &str, default: &'a str) -> &'a str {
    q(map, key).unwrap_or(default)
}

fn truthy(v: Option<&str>) -> bool {
    matches!(v, Some("1") | Some("true") | Some("yes"))
}

/// Строит streamSettings. None = протокол/сетка не поддерживается ядром.
pub fn build_stream(
    net: &str,
    security: &str,
    params: &HashMap<String, String>,
    address: &str,
) -> Option<Value> {
    let network = match net {
        "tcp" => "raw",
        "splithttp" => "xhttp",
        "mkcp" => "kcp",
        "websocket" => "ws",
        "gun" => "grpc",
        other => other,
    };
    if matches!(network, "http" | "h2" | "h3" | "quic") {
        return None;
    }
    if !matches!(network, "raw" | "ws" | "grpc" | "kcp" | "xhttp" | "httpupgrade") {
        return None;
    }
    if security == "reality" && !matches!(network, "raw" | "xhttp" | "grpc") {
        return None;
    }

    let mut stream = Map::new();
    stream.insert(
        "network".into(),
        Value::String(network.to_string()),
    );
    stream.insert(
        "security".into(),
        Value::String(if security == "auto" {
            "none".into()
        } else {
            security.to_string()
        }),
    );

    let sni = q_or(params, "sni", "").to_string();
    let hostp = q_or(params, "host", "").to_string();
    let server_name = if !sni.is_empty() {
        sni
    } else if !hostp.is_empty() {
        hostp
    } else {
        address.to_string()
    };
    let fingerprint = {
        let fp = q_or(params, "fp", "");
        if fp.is_empty() {
            "chrome".to_string()
        } else {
            fp.to_string()
        }
    };

    if security == "tls" {
        let mut tls = Map::new();
        tls.insert("serverName".into(), Value::String(server_name));
        tls.insert("fingerprint".into(), Value::String(fingerprint));
        if let Some(alpn) = q(params, "alpn") {
            let list: Vec<Value> = alpn
                .split(',')
                .map(|s| Value::String(s.trim().to_string()))
                .filter(|v| !v.as_str().unwrap().is_empty())
                .collect();
            if !list.is_empty() {
                tls.insert("alpn".into(), Value::Array(list));
            }
        }
        if let Some(pcs) = q(params, "pcs") {
            if !pcs.is_empty() {
                tls.insert("pinnedPeerCertSha256".into(), Value::String(pcs.to_string()));
            }
        }
        if let Some(vcn) = q(params, "vcn") {
            if !vcn.is_empty() {
                tls.insert("verifyPeerCertByName".into(), Value::String(vcn.to_string()));
            }
        }
        if let Some(ech) = q(params, "ech") {
            if !ech.is_empty() {
                tls.insert("echConfigList".into(), Value::String(ech.to_string()));
            }
        }
        if truthy(q(params, "insecure")) || truthy(q(params, "allowInsecure")) {
            tls.insert("allowInsecure".into(), Value::Bool(true));
        }
        stream.insert("tlsSettings".into(), Value::Object(tls));
    } else if security == "reality" {
        let pbk = q(params, "pbk")?;
        if pbk.is_empty() {
            return None;
        }
        let mut reality = Map::new();
        reality.insert("password".into(), Value::String(pbk.to_string()));
        reality.insert(
            "shortId".into(),
            Value::String(q_or(params, "sid", "").to_string()),
        );
        reality.insert("serverName".into(), Value::String(server_name));
        reality.insert("fingerprint".into(), Value::String(fingerprint));
        reality.insert(
            "spiderX".into(),
            Value::String(q_or(params, "spx", "/").to_string()),
        );
        if let Some(pqv) = q(params, "pqv") {
            if !pqv.is_empty() {
                reality.insert("mldsa65Verify".into(), Value::String(pqv.to_string()));
            }
        }
        stream.insert("realitySettings".into(), Value::Object(reality));
    }

    match network {
        "xhttp" => {
            let mut xs = Map::new();
            xs.insert(
                "path".into(),
                Value::String(q_or(params, "path", "/").to_string()),
            );
            xs.insert(
                "host".into(),
                Value::String(q_or(params, "host", "").to_string()),
            );
            xs.insert(
                "mode".into(),
                Value::String(q_or(params, "mode", "auto").to_string()),
            );
            if let Some(extra) = q(params, "extra") {
                if let Ok(v) = serde_json::from_str::<Value>(extra) {
                    xs.insert("extra".into(), v);
                }
            }
            stream.insert("xhttpSettings".into(), Value::Object(xs));
        }
        "grpc" => {
            let service = q_or(params, "path", "/").trim_matches('/').to_string();
            let mut gs = Map::new();
            gs.insert("serviceName".into(), Value::String(service));
            stream.insert("grpcSettings".into(), Value::Object(gs));
        }
        "ws" | "httpupgrade" => {
            let mut ws = Map::new();
            ws.insert(
                "path".into(),
                Value::String(q_or(params, "path", "/").to_string()),
            );
            ws.insert(
                "host".into(),
                Value::String(q_or(params, "host", "").to_string()),
            );
            let key = if network == "httpupgrade" {
                "httpupgradeSettings"
            } else {
                "wsSettings"
            };
            stream.insert(key.into(), Value::Object(ws));
        }
        "kcp" => {
            let header = q_or(params, "headerType", "");
            if !header.is_empty() && header != "none" && q(params, "fm").is_none() {
                return None;
            }
        }
        _ => {}
    }

    if let Some(fm) = q(params, "fm") {
        if let Ok(v) = serde_json::from_str::<Value>(fm) {
            if v.is_object() {
                stream.insert("finalmask".into(), v);
            }
        }
    }

    Some(Value::Object(stream))
}

pub fn vless_settings(
    address: &str,
    port: u16,
    id: &str,
    params: &HashMap<String, String>,
) -> Value {
    json!({
        "address": address,
        "port": port,
        "level": 0,
        "id": id,
        "encryption": q_or(params, "encryption", "none"),
        "flow": q_or(params, "flow", ""),
    })
}

pub fn vmess_settings(address: &str, port: u16, id: &str, scy: &str) -> Value {
    json!({
        "address": address,
        "port": port,
        "level": 0,
        "id": id,
        "security": scy,
        "experiments": "",
    })
}

pub fn trojan_settings(
    address: &str,
    port: u16,
    password: &str,
    params: &HashMap<String, String>,
) -> Value {
    json!({
        "address": address,
        "port": port,
        "level": 0,
        "password": password,
        "flow": q_or(params, "flow", ""),
    })
}

pub fn ss_settings(address: &str, port: u16, method: &str, password: &str) -> Value {
    json!({
        "address": address,
        "port": port,
        "method": method,
        "password": password,
        "uot": false,
        "uotVersion": 0,
    })
}

pub fn normalize_ss_method(method: &str) -> String {
    match method.trim().to_ascii_lowercase().as_str() {
        "chacha20-poly1305" => "chacha20-ietf-poly1305".to_string(),
        "xchacha20-poly1305" => "xchacha20-ietf-poly1305".to_string(),
        other => other.to_string(),
    }
}

pub const SS_ALLOWED_METHODS: &[&str] = &[
    "2022-blake3-aes-128-gcm",
    "2022-blake3-aes-256-gcm",
    "2022-blake3-chacha20-poly1305",
    "aes-128-gcm",
    "aes-256-gcm",
    "chacha20-poly1305",
    "chacha20-ietf-poly1305",
    "xchacha20-poly1305",
    "xchacha20-ietf-poly1305",
    "none",
    "plain",
];

pub fn ss_method_supported(method: &str) -> bool {
    SS_ALLOWED_METHODS.contains(&method)
}
