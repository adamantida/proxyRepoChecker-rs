use crate::parse::Proxy;
use anyhow::{bail, Context, Result};
use futures::stream::{self, StreamExt};
use std::collections::HashMap;
use std::net::IpAddr;
use std::path::Path;
use std::time::Duration;

const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/121.0.0.0 Safari/537.36";

fn read_varint(buf: &[u8], pos: &mut usize) -> Option<u64> {
    let mut result: u64 = 0;
    let mut shift = 0u32;
    loop {
        if *pos >= buf.len() || shift > 63 {
            return None;
        }
        let b = buf[*pos];
        *pos += 1;
        result |= ((b & 0x7f) as u64) << shift;
        if b & 0x80 == 0 {
            return Some(result);
        }
        shift += 7;
    }
}

struct RangeV4 {
    start: u32,
    end: u32,
    country: usize,
}

struct RangeV6 {
    start: u128,
    end: u128,
    country: usize,
}

pub struct GeoDb {
    v4: Vec<RangeV4>,
    v6: Vec<RangeV6>,
    names: Vec<String>,
}

fn mask_u32(prefix: u32) -> (u32, u32) {
    if prefix == 0 {
        (0, u32::MAX)
    } else if prefix >= 32 {
        (u32::MAX, u32::MAX)
    } else {
        let m = u32::MAX << (32 - prefix);
        (m, !m)
    }
}

fn mask_u128(prefix: u32) -> (u128, u128) {
    if prefix == 0 {
        (0, u128::MAX)
    } else if prefix >= 128 {
        (u128::MAX, u128::MAX)
    } else {
        let m = u128::MAX << (128 - prefix as u32);
        (m, !m)
    }
}

fn parse_cidr(cidr: &[u8]) -> Option<(&[u8], u32)> {
    let mut pos = 0usize;
    let mut ip: Option<&[u8]> = None;
    let mut prefix: u32 = 0;
    while pos < cidr.len() {
        let key = read_varint(cidr, &mut pos)?;
        let field = key >> 3;
        match (field, key & 7) {
            (1, 2) => {
                let len = read_varint(cidr, &mut pos)? as usize;
                if pos + len > cidr.len() {
                    return None;
                }
                ip = Some(&cidr[pos..pos + len]);
                pos += len;
            }
            (2, 0) => {
                prefix = read_varint(cidr, &mut pos)? as u32;
            }
            (_, 0) => {
                read_varint(cidr, &mut pos)?;
            }
            (_, 2) => {
                let len = read_varint(cidr, &mut pos)? as usize;
                if pos + len > cidr.len() {
                    return None;
                }
                pos += len;
            }
            _ => return None,
        }
    }
    ip.map(|ip| (ip, prefix))
}

impl GeoDb {
    /// Разбирает geoip.dat (protobuf: Geoip{country_code, cidr[]}).
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let mut names: Vec<String> = Vec::new();
        let mut index: HashMap<String, usize> = HashMap::new();
        let mut v4: Vec<RangeV4> = Vec::new();
        let mut v6: Vec<RangeV6> = Vec::new();
        let mut entries = 0usize;

        let mut pos = 0usize;
        while pos < bytes.len() {
            let key = match read_varint(bytes, &mut pos) {
                Some(k) => k,
                None => break,
            };
            if key >> 3 != 1 || key & 7 != 2 {
                bail!("geoip.dat: неожиданный protobuf-ключ {key}");
            }
            let len = read_varint(bytes, &mut pos).context("geoip.dat: битый varint")? as usize;
            if pos + len > bytes.len() {
                bail!("geoip.dat: запись выходит за границу файла");
            }
            let payload = &bytes[pos..pos + len];
            pos += len;
            entries += 1;

            let mut cpos = 0usize;
            let mut country = String::new();
            while cpos < payload.len() {
                let ikey = read_varint(payload, &mut cpos).context("geoip.dat: битый inner key")?;
                let ifield = ikey >> 3;
                match (ifield, ikey & 7) {
                    (1, 2) => {
                        let ilen = read_varint(payload, &mut cpos).context("geoip.dat: country_code")? as usize;
                        if cpos + ilen > payload.len() {
                            bail!("geoip.dat: country_code за границей");
                        }
                        country = String::from_utf8_lossy(&payload[cpos..cpos + ilen]).into_owned();
                        cpos += ilen;
                    }
                    (2, 2) => {
                        let ilen = read_varint(payload, &mut cpos).context("geoip.dat: cidr")? as usize;
                        if cpos + ilen > payload.len() {
                            bail!("geoip.dat: cidr за границей");
                        }
                        let cidr = &payload[cpos..cpos + ilen];
                        cpos += ilen;
                        if let Some((ip, prefix)) = parse_cidr(cidr) {
                            let cidx = match index.get(&country) {
                                Some(i) => *i,
                                None => {
                                    let i = names.len();
                                    names.push(country.clone());
                                    index.insert(country.clone(), i);
                                    i
                                }
                            };
                            if ip.len() == 4 && prefix <= 32 {
                                let raw = u32::from_be_bytes([ip[0], ip[1], ip[2], ip[3]]);
                                let (sm, em) = mask_u32(prefix);
                                v4.push(RangeV4 {
                                    start: raw & sm,
                                    end: raw | em,
                                    country: cidx,
                                });
                            } else if ip.len() == 16 && prefix <= 128 {
                                let mut arr = [0u8; 16];
                                arr.copy_from_slice(ip);
                                let raw = u128::from_be_bytes(arr);
                                let (sm, em) = mask_u128(prefix);
                                v6.push(RangeV6 {
                                    start: raw & sm,
                                    end: raw | em,
                                    country: cidx,
                                });
                            }
                        }
                    }
                    (_, 2) => {
                        let ilen = read_varint(payload, &mut cpos).context("geoip.dat: skip")? as usize;
                        if cpos + ilen > payload.len() {
                            bail!("geoip.dat: skip за границей");
                        }
                        cpos += ilen;
                    }
                    (_, 0) => {
                        read_varint(payload, &mut cpos).context("geoip.dat: varint")?;
                    }
                    _ => bail!("geoip.dat: неожиданный wire type"),
                }
            }
        }

        if entries < 10 || names.is_empty() || (v4.is_empty() && v6.is_empty()) {
            bail!("geoip.dat: разобрано слишком мало (записей: {entries})");
        }
        v4.sort_by_key(|r| r.start);
        v6.sort_by_key(|r| r.start);
        Ok(Self { v4, v6, names })
    }

    pub fn open(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path)
            .with_context(|| format!("не удалось прочитать {}", path.display()))?;
        Self::parse(&bytes).with_context(|| format!("не удалось разобрать {}", path.display()))
    }

    pub fn stats(&self) -> (usize, usize, usize) {
        (self.names.len(), self.v4.len(), self.v6.len())
    }

    fn iso_for(&self, ip: IpAddr) -> Option<&str> {
        let idx = match ip {
            IpAddr::V4(a) => {
                let x = u32::from(a);
                let i = self.v4.partition_point(|r| r.start <= x);
                if i == 0 || x > self.v4[i - 1].end {
                    return None;
                }
                self.v4[i - 1].country
            }
            IpAddr::V6(a) => {
                let x = u128::from(a);
                let i = self.v6.partition_point(|r| r.start <= x);
                if i == 0 || x > self.v6[i - 1].end {
                    return None;
                }
                self.v6[i - 1].country
            }
        };
        self.names.get(idx).map(|s| s.as_str())
    }
}

/// Скачивает geoip.dat, если файла нет или он битый.
pub async fn ensure_db(path: &Path, url: &str) -> Result<()> {
    if path.exists() {
        if let Ok(bytes) = std::fs::read(path) {
            if GeoDb::parse(&bytes).is_ok() {
                return Ok(());
            }
        }
    }
    println!(">> GeoIP: качаю geoip.dat -> {}", path.display());
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).ok();
        }
    }
    let client = reqwest::Client::builder()
        .user_agent(UA)
        .danger_accept_invalid_certs(true)
        .build()
        .context("reqwest client")?;
    let resp = client
        .get(url)
        .timeout(Duration::from_secs(60))
        .send()
        .await
        .with_context(|| format!("не удалось скачать {url}"))?
        .error_for_status()
        .context("ошибка ответа при скачивании geoip.dat")?;
    let bytes = resp.bytes().await.context("тело ответа geoip.dat")?;
    if bytes.len() < 1024 {
        bail!(
            "скачан подозрительно маленький geoip.dat ({} байт)",
            bytes.len()
        );
    }
    GeoDb::parse(&bytes).context("скачанный файл не является валидным geoip.dat")?;
    let tmp = path.with_extension("dat.tmp");
    std::fs::write(&tmp, &bytes)
        .with_context(|| format!("не удалось записать {}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .with_context(|| format!("не удалось заменить {}", path.display()))?;
    Ok(())
}

fn local_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(a) => {
            a.is_loopback() || a.is_unspecified() || a.is_private() || a.is_link_local()
        }
        IpAddr::V6(a) => {
            a.is_loopback()
                || a.is_unspecified()
                || (a.segments()[0] & 0xfe00) == 0xfc00
                || (a.segments()[0] & 0xffc0) == 0xfe80
        }
    }
}

async fn resolve_host(host: String) -> (String, Option<IpAddr>) {
    let ip = if let Ok(addr) = host.parse::<IpAddr>() {
        if local_ip(addr) {
            None
        } else {
            Some(addr)
        }
    } else {
        tokio::net::lookup_host((host.clone(), 0))
            .await
            .ok()
            .and_then(|mut addrs| addrs.next().map(|a| a.ip()))
            .filter(|ip| !local_ip(*ip))
    };
    (host, ip)
}

/// Определяет страну для каждого прокси и переименовывает в [XX] - NNN.
pub async fn assign_countries(
    proxies: &mut [Proxy],
    db: &GeoDb,
) -> Result<HashMap<String, usize>> {
    let hosts: Vec<String> = {
        let mut set: std::collections::HashSet<String> = std::collections::HashSet::new();
        for p in proxies.iter() {
            if p.host.parse::<IpAddr>().is_err() {
                set.insert(p.host.clone());
            }
        }
        set.into_iter().collect()
    };

    let pb = indicatif::ProgressBar::new(hosts.len() as u64).with_style(
        indicatif::ProgressStyle::with_template(
            "{spinner:.green} GeoIP резолв {pos}/{len} хостов [{elapsed_precise}<{eta_precise}]",
        )
        .unwrap_or_else(|_| indicatif::ProgressStyle::default_bar()),
    );
    let resolved: HashMap<String, Option<IpAddr>> = stream::iter(hosts)
        .map(|host| {
            let pb = pb.clone();
            async move {
                let r = resolve_host(host).await;
                pb.inc(1);
                r
            }
        })
        .buffer_unordered(512)
        .collect()
        .await;
    pb.finish_and_clear();

    let mut counters: HashMap<String, u32> = HashMap::new();
    let mut counts: HashMap<String, usize> = HashMap::new();

    for p in proxies.iter_mut() {
        let ip = match p.host.parse::<IpAddr>() {
            Ok(addr) if local_ip(addr) => None,
            Ok(addr) => Some(addr),
            Err(_) => resolved.get(&p.host).and_then(|x| *x),
        };
        let iso = ip
            .and_then(|ip| db.iso_for(ip))
            .unwrap_or("XX")
            .to_ascii_uppercase();
        let n = counters.entry(iso.clone()).or_insert(0);
        *n += 1;
        p.country = iso.clone();
        p.display = format!("[{}] - {:03}", iso, *n);
        *counts.entry(iso).or_insert(0) += 1;
    }

    Ok(counts)
}

pub fn print_country_summary(counts: &HashMap<String, usize>) {
    if counts.is_empty() {
        return;
    }
    let mut list: Vec<(&String, &usize)> = counts.iter().collect();
    list.sort_by(|a, b| b.1.cmp(a.1));
    let shown: Vec<String> = list
        .iter()
        .take(12)
        .map(|(k, v)| format!("{k}: {v}"))
        .collect();
    println!(">> GeoIP: стран: {}, топ: {}", counts.len(), shown.join(", "));
}
