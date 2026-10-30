use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

static NEXT_ID: AtomicUsize = AtomicUsize::new(0);

struct TestDir(std::path::PathBuf);

impl TestDir {
    fn new() -> Self {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "proxy-rs-cli-entrypoint-{}-{id}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("create temporary directory");
        Self(path)
    }

    fn path(&self, name: &str) -> std::path::PathBuf {
        self.0.join(name)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn serve_once(response: &'static [u8]) -> String {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind local fixture server");
    let address = listener.local_addr().unwrap();
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept local request");
        let mut request = [0; 2048];
        let _ = stream.read(&mut request);
        stream.write_all(response).expect("write local response");
    });
    format!("http://{address}/fixture")
}

fn run_with(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_proxy-rs"))
        .args(args)
        .output()
        .expect("run proxy-rs binary")
}

#[test]
fn binary_entrypoint_shows_validation_error_and_usage_for_invalid_options() {
    let output = run_with(&["--concurrency", "0"]);

    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("concurrency"), "{stderr}");
    assert!(stderr.contains("Usage:"), "{stderr}");
}

#[test]
fn binary_entrypoint_emits_structured_single_proxy_diagnostics() {
    let temp = TestDir::new();
    let input = temp.path("one-proxy.txt");
    fs::write(&input, "\n# one unsupported link\nssr://fixture\n").unwrap();

    let output = run_with(&["--check-one", input.to_str().unwrap()]);

    assert_eq!(output.status.code(), Some(1));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["parsed"]["status"], "unsupported");
    assert_eq!(report["parsed"]["reason"], "ssr");
    assert_eq!(report["failure"]["stage"], "parse");
    assert!(!String::from_utf8_lossy(&output.stdout).contains("fixture-secret"));
}

fn write_geoip_fixture(path: &std::path::Path) {
    fn varint(mut value: u64) -> Vec<u8> {
        let mut bytes = Vec::new();
        while value >= 0x80 {
            bytes.push((value as u8 & 0x7f) | 0x80);
            value >>= 7;
        }
        bytes.push(value as u8);
        bytes
    }

    fn bytes_field(field: u64, value: &[u8]) -> Vec<u8> {
        let mut bytes = varint((field << 3) | 2);
        bytes.extend(varint(value.len() as u64));
        bytes.extend(value);
        bytes
    }

    let mut cidr = bytes_field(1, &[8, 8, 8, 8]);
    cidr.extend(varint(16)); // field 2, wire type 0
    cidr.extend(varint(32));
    let mut country = bytes_field(1, b"US");
    country.extend(bytes_field(2, &cidr));
    country.extend(bytes_field(9, &vec![0; 1100])); // valid unknown field pads the fixture
    let mut database = bytes_field(1, &country);
    for _ in 1..10 {
        database.extend(bytes_field(1, &[]));
    }
    fs::write(path, database).expect("write local GeoIP database");
}

#[cfg(unix)]
fn write_fake_xray(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;

    let script = r##"#!/bin/sh
if [ "$1" = version ]; then
  echo 'Xray local fixture'
  exit 0
fi
exec python3 - "$3" <<'PY'
import json, select, socket, sys, threading
port = int(json.load(open(sys.argv[1]))["inbounds"][0]["port"])
server = socket.socket()
server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
server.bind(("127.0.0.1", port))
server.listen()
def exact(client, count):
    data = b""
    while len(data) < count:
        part = client.recv(count - len(data))
        if not part:
            raise EOFError()
        data += part
    return data
def handle(client):
    remote = None
    try:
        _, methods = exact(client, 2)
        exact(client, methods)
        client.sendall(b"\x05\x00")
        _, command, _, atyp = exact(client, 4)
        if command != 1:
            raise ValueError("expected CONNECT")
        if atyp == 1:
            host = socket.inet_ntoa(exact(client, 4))
        elif atyp == 3:
            length = exact(client, 1)[0]
            host = exact(client, length).decode()
        elif atyp == 4:
            host = socket.inet_ntop(socket.AF_INET6, exact(client, 16))
        else:
            raise ValueError("unsupported address type")
        destination_port = int.from_bytes(exact(client, 2), "big")
        remote = socket.create_connection((host, destination_port), timeout=3)
        client.sendall(b"\x05\x00\x00\x01\x7f\x00\x00\x01\x00\x00")
        while True:
            ready, _, _ = select.select([client, remote], [], [], 5)
            if not ready:
                break
            for source in ready:
                data = source.recv(65536)
                if not data:
                    return
                (remote if source is client else client).sendall(data)
    except Exception:
        pass
    finally:
        client.close()
        if remote:
            remote.close()
while True:
    client, _ = server.accept()
    threading.Thread(target=handle, args=(client,), daemon=True).start()
PY
"##;
    let mut contents = script.to_string();
    if contents.len() < 1_000_100 {
        contents.push_str(&"#".repeat(1_000_100 - contents.len()));
    }
    fs::write(path, contents).expect("write fake Xray executable");
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).unwrap();
}

#[test]
fn binary_entrypoint_runs_a_local_file_pipeline_without_network_access() {
    let temp = TestDir::new();
    let input = temp.path("links.txt");
    fs::write(
        &input,
        "vless://01234567-89ab-cdef-0123-456789abcdef@proxy.example:443#test-node\n",
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_proxy-rs"))
        .args([
            "--file",
            input.to_str().unwrap(),
            "--sources",
            temp.path("missing-sources.json").to_str().unwrap(),
            "--output",
            temp.path("alive.txt").to_str().unwrap(),
            "--raw-output",
            temp.path("raw.txt").to_str().unwrap(),
            "--parsed-output",
            temp.path("parsed.txt").to_str().unwrap(),
            "--good-output",
            temp.path("good.txt").to_str().unwrap(),
            "--no-geoip",
            "--no-tcp-ping",
            "--no-speed",
        ])
        .output()
        .expect("run proxy-rs binary");

    assert!(
        output.status.success(),
        "proxy-rs failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(fs::read_to_string(temp.path("alive.txt"))
        .unwrap()
        .contains("proxy.example"));
    assert!(String::from_utf8_lossy(&output.stdout).contains("ИТОГИ"));
}

#[test]
fn binary_entrypoint_fetches_a_local_subscription_and_saves_raw_and_parsed_outputs() {
    let temp = TestDir::new();
    let feed = serve_once(
        b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\nvless://01234567-89ab-cdef-0123-456789abcdef@feed.example:443#feed-node\n",
    );
    let sources = temp.path("sources.json");
    fs::write(&sources, format!(r#"{{"3":["{feed}"]}}"#)).unwrap();

    let output = run_with(&[
        "--sources",
        sources.to_str().unwrap(),
        "--set",
        "3",
        "--output",
        temp.path("alive.txt").to_str().unwrap(),
        "--raw-output",
        temp.path("raw.txt").to_str().unwrap(),
        "--parsed-output",
        temp.path("parsed.txt").to_str().unwrap(),
        "--good-output",
        temp.path("good.txt").to_str().unwrap(),
        "--no-geoip",
        "--no-tcp-ping",
        "--no-speed",
    ]);

    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let raw = fs::read_to_string(temp.path("raw.txt")).unwrap();
    assert!(raw.contains("feed.example"), "{raw}");
    let parsed = fs::read_to_string(temp.path("parsed.txt")).unwrap();
    assert_eq!(parsed.lines().count(), 1);
    assert!(parsed.contains("feed-node"), "{parsed}");
    assert!(String::from_utf8_lossy(&output.stdout).contains("Загружено подписок: 1, ошибок: 0"));
}

#[test]
fn binary_entrypoint_uses_a_local_geoip_database_and_renames_the_proxy() {
    let temp = TestDir::new();
    let input = temp.path("links.txt");
    fs::write(
        &input,
        "vless://01234567-89ab-cdef-0123-456789abcdef@8.8.8.8:443#geo-node\n",
    )
    .unwrap();
    let geoip = temp.path("geoip.dat");
    write_geoip_fixture(&geoip);

    let output = run_with(&[
        "--file",
        input.to_str().unwrap(),
        "--sources",
        temp.path("missing-sources.json").to_str().unwrap(),
        "--output",
        temp.path("alive.txt").to_str().unwrap(),
        "--raw-output",
        temp.path("raw.txt").to_str().unwrap(),
        "--parsed-output",
        temp.path("parsed.txt").to_str().unwrap(),
        "--good-output",
        temp.path("good.txt").to_str().unwrap(),
        "--geoip-db",
        geoip.to_str().unwrap(),
        "--no-tcp-ping",
        "--no-speed",
    ]);

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let alive = fs::read_to_string(temp.path("alive.txt")).unwrap();
    assert!(alive.contains("[US]"), "{alive}");
    assert!(String::from_utf8_lossy(&output.stdout)
        .contains("Стран в результатах: 1, распределение: US: 1"));
}

#[cfg(unix)]
#[test]
fn binary_entrypoint_runs_speed_good_statistics_and_clash_with_local_fixtures() {
    let temp = TestDir::new();
    let http_response = {
        let body = vec![b'x'; 16 * 1024];
        let mut response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        response.extend_from_slice(&body);
        response
    };
    let feed = serve_once(Box::leak(http_response.into_boxed_slice()));
    let input = temp.path("links.txt");
    fs::write(
        &input,
        "vless://01234567-89ab-cdef-0123-456789abcdef@127.0.0.1:443#local-speed-node\n",
    )
    .unwrap();
    let core = temp.path("fake-xray");
    write_fake_xray(&core);

    let output = run_with(&[
        "--file",
        input.to_str().unwrap(),
        "--sources",
        temp.path("missing-sources.json").to_str().unwrap(),
        "--output",
        temp.path("alive.txt").to_str().unwrap(),
        "--raw-output",
        temp.path("raw.txt").to_str().unwrap(),
        "--parsed-output",
        temp.path("parsed.txt").to_str().unwrap(),
        "--good-output",
        temp.path("good.txt").to_str().unwrap(),
        "--clash",
        temp.path("clash.yaml").to_str().unwrap(),
        "--core",
        core.to_str().unwrap(),
        "--workdir",
        temp.path("work").to_str().unwrap(),
        "--speed-url",
        &feed,
        "--min-kb",
        "0",
        "--speed-timeout",
        "3",
        "--speed-connect-timeout",
        "2",
        "--no-geoip",
        "--no-tcp-ping",
    ]);

    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("прошли порог: 1/1"), "{stdout}");
    assert!(
        stdout.contains("средняя:") && stdout.contains("медиана:"),
        "{stdout}"
    );
    assert!(fs::read_to_string(temp.path("good.txt"))
        .unwrap()
        .contains("local-speed-node"));
    assert!(fs::read_to_string(temp.path("alive.txt"))
        .unwrap()
        .contains("local-speed-node"));
    let clash = fs::read_to_string(temp.path("clash.yaml")).unwrap();
    assert!(clash.contains("local-speed-node"), "{clash}");
    assert!(clash.contains("proxy-groups:"), "{clash}");
}
