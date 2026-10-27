use crate::parse::Proxy;
use anyhow::{Context, Result};
use std::ffi::OsString;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_ID: AtomicU64 = AtomicU64::new(0);

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
    write_bytes_atomic(Path::new(path), text.as_bytes(), None)
}

/// Write a credential-free JSON view of proxy results for automation.
pub fn write_proxy_json(path: &str, proxies: &[Proxy]) -> Result<()> {
    let rows = proxies
        .iter()
        .map(|proxy| {
            serde_json::json!({
                "protocol": proxy.proto,
                "server": proxy.host,
                "port": proxy.port,
                "name": proxy.name,
                "country": proxy.country,
                "display": proxy.display,
                "ping_ms": proxy.ping,
                "speed_kbs": proxy.speed_kbs,
            })
        })
        .collect::<Vec<_>>();
    let json = serde_json::to_string_pretty(&rows)?;
    write_text_atomic(path, &json)
}

pub fn write_bytes_atomic(path: &Path, bytes: &[u8], unix_mode: Option<u32>) -> Result<()> {
    write_bytes_atomic_checked(path, bytes, unix_mode, |_| Ok(()))
}

pub fn write_bytes_atomic_checked<F>(
    path: &Path,
    bytes: &[u8],
    unix_mode: Option<u32>,
    validate: F,
) -> Result<()>
where
    F: FnOnce(&Path) -> Result<()>,
{
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let name = path
        .file_name()
        .context("путь назначения не содержит имени файла")?;
    let mut tmp = PathBuf::new();
    let mut file = None;
    for _ in 0..100 {
        let id = TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let mut temp_name = OsString::from(".");
        temp_name.push(name);
        temp_name.push(format!(".{}.{}.tmp", std::process::id(), id));
        if let Some(extension) = path.extension() {
            temp_name.push(".");
            temp_name.push(extension);
        }
        tmp = parent.join(temp_name);
        match OpenOptions::new().write(true).create_new(true).open(&tmp) {
            Ok(f) => {
                file = Some(f);
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => {
                return Err(e).with_context(|| format!("не удалось создать {}", tmp.display()));
            }
        }
    }
    let mut file = file.context("не удалось подобрать уникальное имя временного файла")?;
    let result = (|| -> Result<()> {
        file.write_all(bytes)
            .with_context(|| format!("не удалось записать {}", tmp.display()))?;
        file.sync_all()
            .with_context(|| format!("не удалось синхронизировать {}", tmp.display()))?;
        drop(file);
        #[cfg(unix)]
        if let Some(mode) = unix_mode {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode))
                .with_context(|| format!("не удалось установить права {}", tmp.display()))?;
        }
        validate(&tmp)?;
        replace_file(&tmp, path)
            .with_context(|| format!("не удалось заменить {}", path.display()))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

#[cfg(not(windows))]
fn replace_file(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::rename(from, to)
}

#[cfg(windows)]
fn replace_file(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x8;
    #[link(name = "kernel32")]
    extern "system" {
        fn MoveFileExW(existing: *const u16, new: *const u16, flags: u32) -> i32;
    }
    let from: Vec<u16> = from.as_os_str().encode_wide().chain(Some(0)).collect();
    let to: Vec<u16> = to.as_os_str().encode_wide().chain(Some(0)).collect();
    let ok = unsafe {
        MoveFileExW(
            from.as_ptr(),
            to.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if ok == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

pub fn print_top(items: &[Proxy], with_speed: bool) {
    for line in top_lines(items, with_speed) {
        println!("{line}");
    }
}

fn top_lines(items: &[Proxy], with_speed: bool) -> Vec<String> {
    if items.is_empty() {
        return vec!["   (пусто)".into()];
    }
    let mut lines = vec![format!(
        "   {:>6} {:>10}  {:<14} {:<12} адрес",
        "ping", "speed", "имя", "протокол"
    )];
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
        lines.push(format!(
            "   {:>6} {:>10}  {:<14} {:<12} {}:{}",
            ping, speed, name, p.proto, p.host, p.port
        ));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_ID: AtomicU64 = AtomicU64::new(0);

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "proxy-rs-output-{}-{}",
                std::process::id(),
                TEST_ID.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn path(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }

        fn temp_files(&self) -> Vec<PathBuf> {
            std::fs::read_dir(&self.0)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .filter(|path| {
                    path.file_name()
                        .unwrap()
                        .to_string_lossy()
                        .contains(".tmp.")
                        || path
                            .file_name()
                            .unwrap()
                            .to_string_lossy()
                            .ends_with(".tmp")
                })
                .collect()
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn proxy() -> Proxy {
        Proxy {
            proto: "vless",
            host: "proxy.example".into(),
            port: 443,
            name: "name".into(),
            base_url: "vless://uuid@proxy.example:443".into(),
            orig_url: "vless://uuid@proxy.example:443#original".into(),
            settings: serde_json::Value::Null,
            stream: None,
            country: "DE".into(),
            display: "[DE] - 001".into(),
            ping: None,
            speed_kbs: 0.0,
        }
    }

    #[test]
    fn line_uses_renamed_display_when_requested() {
        let p = proxy();
        assert_eq!(line(&p, true), "vless://uuid@proxy.example:443#[DE] - 001");
        assert_eq!(line(&p, false), p.orig_url);
    }

    #[test]
    fn line_falls_back_to_original_when_display_is_empty() {
        let mut p = proxy();
        p.display.clear();
        assert_eq!(line(&p, true), p.orig_url);
    }

    #[test]
    fn proxy_json_reports_results_without_uri_credentials() {
        let dir = TestDir::new();
        let path = dir.path("report.json");
        let mut item = proxy();
        item.orig_url = "vless://private-password@proxy.example:443?token=secret".into();
        item.settings = serde_json::json!({"password": "private-password", "id": "private-id"});
        item.ping = Some(24);
        item.speed_kbs = 1024.5;

        write_proxy_json(path.to_str().unwrap(), &[item]).unwrap();

        let json: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(json[0]["protocol"], "vless");
        assert_eq!(json[0]["ping_ms"], 24);
        assert_eq!(json[0]["speed_kbs"], 1024.5);
        let text = json.to_string();
        assert!(!text.contains("private-password"));
        assert!(!text.contains("private-id"));
        assert!(!text.contains("secret"));
    }

    #[test]
    fn proxy_json_handles_empty_results_and_missing_ping() {
        let dir = TestDir::new();
        let path = dir.path("empty.json");
        write_proxy_json(path.to_str().unwrap(), &[]).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "[]");

        let path = dir.path("without-ping.json");
        write_proxy_json(path.to_str().unwrap(), &[proxy()]).unwrap();
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert!(value[0]["ping_ms"].is_null());
        assert_eq!(value[0]["speed_kbs"], 0.0);
    }

    #[test]
    fn proxy_json_propagates_atomic_write_failure() {
        let dir = TestDir::new();
        let path = dir.path("missing-parent/report.json");

        let error = write_proxy_json(path.to_str().unwrap(), &[proxy()]).unwrap_err();

        assert!(error.to_string().contains("не удалось создать"));
        assert!(!path.exists());
        assert!(!dir.path("missing-parent").exists());
    }

    #[test]
    fn top_lines_formats_empty_input_and_both_speed_modes() {
        assert_eq!(top_lines(&[], false), ["   (пусто)"]);

        let mut named = proxy();
        named.ping = Some(12);
        named.speed_kbs = 1536.26;
        let without_speed = top_lines(&[named.clone()], false);
        assert_eq!(without_speed.len(), 2);
        assert!(without_speed[1].contains("12 ms"));
        assert!(without_speed[1].contains("         -"));
        assert!(without_speed[1].contains("[DE] - 001"));

        let with_speed = top_lines(&[named], true);
        assert!(with_speed[1].contains("1536.3 KB/s"));

        let mut unnamed = proxy();
        unnamed.display.clear();
        assert_eq!(
            top_lines(&[unnamed], false)[1]
                .split_whitespace()
                .collect::<Vec<_>>(),
            ["-", "-", "-", "vless", "proxy.example:443"]
        );
    }

    #[test]
    fn top_lines_formats_header_and_missing_ping_without_speed() {
        let item = proxy();
        let lines = top_lines(&[item], false);

        assert_eq!(
            lines[0],
            "     ping      speed  имя            протокол     адрес"
        );
        assert_eq!(
            lines[1],
            "        -          -  [DE] - 001     vless        proxy.example:443"
        );
    }

    #[test]
    fn top_lines_limits_the_table_to_fifteen_proxies() {
        let items = (0..20)
            .map(|index| {
                let mut item = proxy();
                item.port += index;
                item
            })
            .collect::<Vec<_>>();

        let lines = top_lines(&items, false);

        assert_eq!(lines.len(), 16); // header plus the first fifteen proxies
        assert!(lines.last().unwrap().ends_with("proxy.example:457"));
    }

    #[test]
    fn atomic_writer_replaces_existing_file_and_writes_all_bytes() {
        let dir = TestDir::new();
        let path = dir.path("result.bin");
        std::fs::write(&path, b"old contents").unwrap();

        write_bytes_atomic(&path, b"new\0contents", None).unwrap();

        assert_eq!(std::fs::read(&path).unwrap(), b"new\0contents");
        assert!(dir.temp_files().is_empty());
    }

    #[test]
    fn checked_writer_preserves_destination_and_removes_temp_on_validation_failure() {
        let dir = TestDir::new();
        let path = dir.path("result.txt");
        std::fs::write(&path, "keep me").unwrap();

        let err = write_bytes_atomic_checked(&path, b"invalid", None, |tmp| {
            assert_eq!(tmp.extension().and_then(|ext| ext.to_str()), Some("txt"));
            assert_eq!(std::fs::read(tmp).unwrap(), b"invalid");
            anyhow::bail!("validation rejected")
        });

        assert!(err.is_err());
        assert_eq!(std::fs::read_to_string(path).unwrap(), "keep me");
        assert!(dir.temp_files().is_empty());
    }

    #[test]
    fn atomic_writer_skips_existing_temporary_names() {
        let dir = TestDir::new();
        let path = dir.path("collision.txt");
        let first_id = TEMP_ID.load(Ordering::Relaxed);
        for id in first_id..first_id + 32 {
            let collision = dir.path(&format!(
                ".collision.txt.{}.{}.tmp.txt",
                std::process::id(),
                id
            ));
            std::fs::write(collision, b"reserved temporary name").unwrap();
        }

        write_bytes_atomic(&path, b"new contents", None).unwrap();

        assert_eq!(std::fs::read(&path).unwrap(), b"new contents");
        for id in first_id..first_id + 32 {
            let collision = dir.path(&format!(
                ".collision.txt.{}.{}.tmp.txt",
                std::process::id(),
                id
            ));
            assert_eq!(
                std::fs::read(collision).unwrap(),
                b"reserved temporary name"
            );
        }
    }

    #[test]
    fn writer_reports_creation_failure_without_leaving_a_temp_file() {
        let dir = TestDir::new();
        let path = dir.path("missing-parent").join("result.txt");

        assert!(write_bytes_atomic(&path, b"data", None).is_err());
        assert!(!path.exists());
        assert!(std::fs::read_dir(dir.path("missing-parent")).is_err());
    }

    #[test]
    fn writer_reports_temporary_name_exhaustion_without_overwriting_collisions() {
        let dir = TestDir::new();
        let path = dir.path("exhausted.txt");
        let first_id = TEMP_ID.load(Ordering::Relaxed);
        const RESERVED_NAMES: u64 = 1_000;
        for id in first_id..first_id + RESERVED_NAMES {
            let collision = dir.path(&format!(
                ".exhausted.txt.{}.{}.tmp.txt",
                std::process::id(),
                id
            ));
            std::fs::write(collision, b"reserved").unwrap();
        }

        let error = write_bytes_atomic(&path, b"data", None).unwrap_err();

        assert!(error
            .to_string()
            .contains("не удалось подобрать уникальное имя временного файла"));
        assert!(!path.exists());
        for id in first_id..first_id + RESERVED_NAMES {
            let collision = dir.path(&format!(
                ".exhausted.txt.{}.{}.tmp.txt",
                std::process::id(),
                id
            ));
            assert_eq!(std::fs::read(collision).unwrap(), b"reserved");
        }
    }

    #[test]
    fn writer_cleans_up_temp_file_when_destination_cannot_be_replaced() {
        let dir = TestDir::new();
        let destination = dir.path("destination");
        std::fs::create_dir(&destination).unwrap();

        assert!(write_bytes_atomic(&destination, b"data", None).is_err());
        assert!(destination.is_dir());
        assert!(dir.temp_files().is_empty());
    }

    #[test]
    fn write_lines_adds_one_newline_per_line_and_handles_empty_input() {
        let dir = TestDir::new();
        let path = dir.path("lines.txt");
        let lines = vec!["first".to_string(), "вторая".to_string()];

        write_lines_atomic(path.to_str().unwrap(), &lines).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "first\nвторая\n");

        write_lines_atomic(path.to_str().unwrap(), &[]).unwrap();
        assert_eq!(std::fs::read_to_string(path).unwrap(), "");
        assert!(dir.temp_files().is_empty());
    }

    #[test]
    fn writer_rejects_a_destination_without_a_file_name() {
        let err = write_bytes_atomic(Path::new(""), b"data", None).unwrap_err();

        assert!(err
            .to_string()
            .contains("путь назначения не содержит имени файла"));
    }

    #[test]
    fn writer_accepts_a_relative_destination_without_an_explicit_parent() {
        let id = TEST_ID.fetch_add(1, Ordering::Relaxed);
        let name = format!(".proxy-rs-relative-output-{}-{id}.txt", std::process::id());
        let path = Path::new(&name);

        write_bytes_atomic(path, b"relative output", None).unwrap();

        assert_eq!(std::fs::read(path).unwrap(), b"relative output");
        std::fs::remove_file(path).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn writer_applies_requested_unix_mode_and_preserves_destination_extension_for_validation() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TestDir::new();
        let path = dir.path("xray").with_extension("bin");

        write_bytes_atomic_checked(&path, b"binary", Some(0o755), |tmp| {
            assert_eq!(tmp.extension().and_then(|ext| ext.to_str()), Some("bin"));
            assert!(tmp.file_name().unwrap().to_string_lossy().contains(".tmp."));
            Ok(())
        })
        .unwrap();

        assert_eq!(std::fs::read(&path).unwrap(), b"binary");
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o755
        );
        assert!(dir.temp_files().is_empty());
    }
}
