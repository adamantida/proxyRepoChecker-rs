//! Stable benchmark harness (no third-party benchmark dependency).
//! Run with `cargo bench --bench pipeline`; all inputs are generated locally.
#![allow(dead_code, unused_imports)]

#[path = "../src/geoip.rs"]
mod geoip;
#[path = "../src/outbound.rs"]
mod outbound;
#[path = "../src/output.rs"]
mod output;
#[path = "../src/parse.rs"]
mod parse;

use proxy_clash::{render, Options, Source};
use serde_json::json;
use std::hint::black_box;
use std::time::{Duration, Instant};

fn main() {
    let parse_inputs = parse_inputs();
    let geoip_input = geoip_database(500);
    let clash_inputs = clash_inputs(250);

    bench_parse(&parse_inputs, 200);
    bench("GeoIP protobuf parse (500 entries)", 200, || {
        black_box(geoip::GeoDb::parse(black_box(&geoip_input)).unwrap());
    });
    bench("Clash render (250 sources)", 200, || {
        black_box(render(black_box(&clash_inputs), black_box(&Options::default())).unwrap());
    });
}

fn bench_parse(template: &[String], iterations: u32) {
    let mut elapsed = Duration::ZERO;
    for _ in 0..iterations {
        // parse_all consumes its input; prepare the owned batch outside the timer
        // so the measurement reflects parsing and deduplication only.
        let input = black_box(template.to_vec());
        let start = Instant::now();
        let (parsed, stats) = parse::parse_all(input);
        elapsed += start.elapsed();
        black_box((parsed, stats));
    }
    let each = elapsed / iterations;
    println!(
        "parse_all + dedup (288 links): {iterations} iterations, {each:?}/iteration ({})",
        per_second(elapsed, iterations)
    );
}

fn bench(name: &str, iterations: u32, mut run: impl FnMut()) {
    let start = Instant::now();
    for _ in 0..iterations {
        run();
    }
    let elapsed = start.elapsed();
    let each = elapsed / iterations;
    println!(
        "{name}: {iterations} iterations, {each:?}/iteration ({})",
        per_second(elapsed, iterations)
    );
}

fn per_second(elapsed: Duration, iterations: u32) -> String {
    let rate = iterations as f64 / elapsed.as_secs_f64();
    format!("{rate:.1} ops/s")
}

fn parse_inputs() -> Vec<String> {
    let id = "01234567-89ab-cdef-0123-456789abcdef";
    let mut links = Vec::with_capacity(256);
    for i in 0..128 {
        links.push(format!(
            "vless://{id}@node-{i}.example:443?type=tcp#node-{i}"
        ));
        links.push(format!(
            "vless://{id}@node-{i}.example:443?type=tcp#duplicate-{i}"
        ));
    }
    links.extend((0..16).map(|i| format!("tuic://unsupported-{i}")));
    links.extend((0..16).map(|i| format!("vless://bad-{i}@example.org")));
    links
}

fn geoip_database(entries: usize) -> Vec<u8> {
    let mut database = Vec::new();
    for index in 0..entries {
        let mut cidr = field_bytes(
            1,
            &[10, (index >> 16) as u8, (index >> 8) as u8, index as u8],
        );
        cidr.extend(field_varint(2, 32));
        let mut entry = field_bytes(1, format!("C{:02}", index % 90).as_bytes());
        entry.extend(field_bytes(2, &cidr));
        database.extend(field_bytes(1, &entry));
    }
    database
}

fn clash_inputs(count: usize) -> Vec<Source> {
    (0..count)
        .map(|index| Source {
            proto: "vless".into(),
            name: format!("[C{:02}] - {index:03}", index % 90),
            server: format!("192.0.2.{}", index % 255),
            port: 443,
            country: format!("C{:02}", index % 90),
            settings: json!({
                "address": format!("192.0.2.{}", index % 255),
                "port": 443,
                "id": format!("uuid-{index}"),
                "encryption": "none"
            }),
            stream: Some(json!({"network": "raw", "security": "none"})),
        })
        .collect()
}

fn field_bytes(field: u64, bytes: &[u8]) -> Vec<u8> {
    let mut result = varint((field << 3) | 2);
    result.extend(varint(bytes.len() as u64));
    result.extend_from_slice(bytes);
    result
}

fn field_varint(field: u64, value: u64) -> Vec<u8> {
    let mut result = varint(field << 3);
    result.extend(varint(value));
    result
}

fn varint(mut value: u64) -> Vec<u8> {
    let mut result = Vec::new();
    while value >= 0x80 {
        result.push((value as u8) | 0x80);
        value >>= 7;
    }
    result.push(value as u8);
    result
}
