use crate::cli::Cli;
use anyhow::{bail, Context, Result};
use std::collections::HashSet;
use std::path::Path;

pub struct Sources {
    pub fetch_urls: Vec<String>,
    pub direct_links: Vec<String>,
}

pub fn collect(cli: &Cli) -> Result<Sources> {
    let mut urls: Vec<String> = Vec::new();

    if Path::new(&cli.sources).exists() {
        let text = std::fs::read_to_string(&cli.sources)
            .with_context(|| format!("не удалось прочитать {}", cli.sources))?;
        let data: serde_json::Value =
            serde_json::from_str(&text).with_context(|| format!("битый JSON: {}", cli.sources))?;
        let obj = data
            .as_object()
            .context("sources.json должен быть объектом {\"1\": [...], \"2\": [...], \"3\": [...]}")?;

        let sets: Vec<String> = if cli.set.eq_ignore_ascii_case("all") {
            obj.keys().cloned().collect()
        } else {
            vec![cli.set.clone()]
        };
        for key in sets {
            match obj.get(&key) {
                Some(serde_json::Value::Array(list)) => {
                    for item in list {
                        if let Some(u) = item.as_str() {
                            urls.push(u.to_string());
                        }
                    }
                }
                _ => bail!("в sources.json нет набора \"{}\"", key),
            }
        }
    } else if cli.url.is_empty() && cli.file.is_none() {
        bail!(
            "не найден {} — укажите --sources, -u или -f",
            cli.sources
        );
    }

    urls.extend(cli.url.iter().cloned());

    let mut direct_links: Vec<String> = Vec::new();
    if let Some(path) = &cli.file {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("не удалось прочитать {}", path))?;
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let lower = line.to_ascii_lowercase();
            if lower.starts_with("http://") || lower.starts_with("https://") {
                urls.push(line.to_string());
            } else if line.contains("://") {
                direct_links.push(line.to_string());
            }
        }
    }

    let mut seen = HashSet::new();
    urls.retain(|u| seen.insert(u.clone()));
    Ok(Sources {
        fetch_urls: urls,
        direct_links,
    })
}
