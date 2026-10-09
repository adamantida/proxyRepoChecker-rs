use crate::parse::Proxy;
use anyhow::{Context, Result};

pub fn line(p: &Proxy, rename: bool) -> String {
    if rename && !p.display.is_empty() {
        format!("{}#{}", p.base_url, p.display)
    } else {
        p.orig_url.clone()
    }
}

pub fn write_lines_atomic(path: &str, lines: &[String]) -> Result<()> {
    let mut out = String::with_capacity(lines.len() * 160);
    for l in lines {
        out.push_str(l);
        out.push('\n');
    }
    write_text_atomic(path, &out)
}

pub fn write_text_atomic(path: &str, text: &str) -> Result<()> {
    let tmp = format!("{path}.tmp");
    std::fs::write(&tmp, text).with_context(|| format!("не удалось записать {tmp}"))?;
    std::fs::rename(&tmp, path).with_context(|| format!("не удалось заменить {path}"))?;
    Ok(())
}

pub fn print_top(items: &[Proxy], with_speed: bool) {
    if items.is_empty() {
        println!("   (пусто)");
        return;
    }
    println!(
        "   {:>6} {:>10}  {:<14} {:<12} {}",
        "ping", "speed", "имя", "протокол", "адрес"
    );
    for p in items.iter().take(15) {
        let ping = p
            .ping
            .map(|m| format!("{m} ms"))
            .unwrap_or_else(|| "-".into());
        let speed = if with_speed {
            format!("{:.1} KB/s", p.speed_kbs)
        } else {
            "-".into()
        };
        let name = if p.display.is_empty() {
            "-"
        } else {
            &p.display
        };
        println!(
            "   {:>6} {:>10}  {:<14} {:<12} {}:{}",
            ping,
            speed,
            name,
            p.proto,
            p.host,
            p.port
        );
    }
}
