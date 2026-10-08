#!/usr/bin/env python3
"""Fetch content-verified source archives into an ignored local cache (Python 3.12+)."""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile

ROOT = Path(__file__).resolve().parents[1]


def fetch(name, pin, archives_dir=None):
    cache = ROOT / ".cache" / "upstreams"
    cache.mkdir(parents=True, exist_ok=True)
    archive = cache / f"{name}-{pin['revision']}.tar.gz"
    if not archive.exists():
        if archives_dir:
            shutil.copyfile(archives_dir / f"{name}.tar.gz", archive)
        else:
            partial = archive.with_suffix(".partial")
            subprocess.run([
                "curl", "--fail", "--silent", "--show-error", "--location",
                f"https://codeload.github.com/{pin['repository']}/tar.gz/{pin['revision']}",
                "--output", str(partial),
            ], check=True)
            partial.rename(archive)
    digest = hashlib.file_digest(archive.open("rb"), "sha256").hexdigest()
    if digest != pin["archive_sha256"]:
        raise ValueError(f"{name}: archive checksum mismatch; remove {archive} before retrying")
    destination = cache / name
    marker = destination / ".uplc-scaffold-source.json"
    if marker.exists() and json.loads(marker.read_text()) == pin:
        print(f"{name}: {pin['revision']} already available")
        return
    if destination.exists():
        raise ValueError(f"refusing to replace existing source directory {destination}; move it aside first")
    with tempfile.TemporaryDirectory(dir=cache) as temp:
        with tarfile.open(archive) as tar:
            tar.extractall(temp, filter="data")
        roots = list(Path(temp).iterdir())
        if len(roots) != 1 or not roots[0].is_dir():
            raise ValueError(f"unexpected archive layout for {name}")
        roots[0].rename(destination)
    marker.write_text(json.dumps(pin, indent=2) + "\n")
    print(f"{name}: {pin['revision']} verified and extracted")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--only", nargs="+", choices=["aiken", "amaru", "plutus"])
    parser.add_argument("--archives-dir", type=Path, help="offline input directory containing <name>.tar.gz")
    args = parser.parse_args()
    pins = json.loads((ROOT / "upstreams.lock.json").read_text())
    for name in args.only or pins:
        fetch(name, pins[name], args.archives_dir)
