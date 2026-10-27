#!/usr/bin/env python3
"""Package a native proxy-rs build with its source catalog and documentation."""

from __future__ import annotations

import argparse
import json
import shutil
import subprocess
import tarfile
import zipfile
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
README_FILES = ("README.md", "README.en.md", "README.zh-CN.md")


def rust_host() -> str:
    output = subprocess.run(
        ["rustc", "-vV"], check=True, capture_output=True, text=True
    ).stdout
    for line in output.splitlines():
        if line.startswith("host: "):
            return line.removeprefix("host: ").strip()
    raise RuntimeError("could not determine the Rust host target")


def package_version() -> str:
    output = subprocess.run(
        ["cargo", "metadata", "--no-deps", "--format-version", "1"],
        check=True,
        capture_output=True,
        text=True,
        cwd=ROOT,
    ).stdout
    metadata = json.loads(output)
    for package in metadata["packages"]:
        if package["name"] == "proxy-rs":
            return package["version"]
    raise RuntimeError("proxy-rs package is missing from Cargo metadata")


def architecture(target: str) -> str:
    arch = target.split("-", maxsplit=1)[0]
    if arch not in {"x86_64", "aarch64"}:
        raise ValueError(f"unsupported release architecture: {arch}")
    return arch


def verify_platform(target: str, platform: str) -> None:
    expected = {
        "linux": "-unknown-linux-",
        "macos": "-apple-darwin",
        "windows": "-pc-windows-",
    }[platform]
    if expected not in target:
        raise ValueError(
            f"runner platform mismatch: requested {platform}, Rust target is {target}"
        )


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tag", required=True, help="release tag, for example v0.1.0")
    parser.add_argument("--platform", required=True, choices=("linux", "macos", "windows"))
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()

    current_version = package_version()
    tag_version = args.tag.removeprefix("v")
    if tag_version != current_version:
        raise ValueError(
            f"tag {args.tag} does not match Cargo.toml version {current_version}"
        )

    target = rust_host()
    verify_platform(target, args.platform)
    arch = architecture(target)
    binary = (ROOT / args.binary).resolve()
    if not binary.is_file():
        raise FileNotFoundError(f"release binary does not exist: {binary}")

    archive_stem = f"proxy-rs-{args.tag}-{args.platform}-{arch}"
    archive_root = f"proxy-rs-{args.tag}-{args.platform}-{arch}"
    staging = args.output.resolve() / archive_root
    shutil.rmtree(staging, ignore_errors=True)
    staging.mkdir(parents=True)

    shutil.copy2(binary, staging / binary.name)
    shutil.copy2(ROOT / "bin" / "sources.json", staging / "sources.json")
    shutil.copy2(ROOT / "CHANGELOG.md", staging / "CHANGELOG.md")
    for readme in README_FILES:
        shutil.copy2(ROOT / readme, staging / readme)
    shutil.copytree(ROOT / "docs", staging / "docs")

    args.output.mkdir(parents=True, exist_ok=True)
    archive = args.output.resolve() / (
        f"{archive_stem}.zip" if args.platform == "windows" else f"{archive_stem}.tar.gz"
    )
    if args.platform == "windows":
        with zipfile.ZipFile(archive, "w", compression=zipfile.ZIP_DEFLATED) as bundle:
            for path in sorted(staging.rglob("*")):
                if path.is_file():
                    bundle.write(path, path.relative_to(staging.parent))
    else:
        with tarfile.open(archive, "w:gz") as bundle:
            bundle.add(staging, arcname=archive_root)

    shutil.rmtree(staging)
    print(f"Created {archive}")


if __name__ == "__main__":
    main()
