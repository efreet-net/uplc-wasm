#!/usr/bin/env python3
"""Fetch content-verified source archives into an ignored local cache (Python 3.12+)."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile

ROOT = Path(__file__).resolve().parents[1]


def verify_archive(archive, pin):
    with archive.open("rb") as source:
        digest = hashlib.file_digest(source, "sha256").hexdigest()
    if digest != pin["archive_sha256"]:
        raise ValueError(f"archive checksum mismatch: {archive}")


def verify_source(name, pin):
    """Verify cached source bytes too: a marker alone cannot attest a working tree."""
    cache = ROOT / ".cache/upstreams"
    archive = cache / f"{name}-{pin['revision']}.tar.gz"
    verify_archive(archive, pin)
    destination = cache / name
    marker = destination / ".uplc-scaffold-source.json"
    if json.loads(marker.read_text()) != pin:
        raise ValueError(f"{name}: source pin mismatch")
    expected = set()
    with tarfile.open(archive) as tar:
        for member in tar:
            relative = Path(member.name).parts[1:]
            path = destination.joinpath(*relative)
            if not member.isdir():
                expected.add(Path(*relative))
            if member.isfile() or member.islnk():
                if path.is_symlink():
                    raise ValueError(f"{name}: cached source file replaced by a symlink: {path}")
                with tar.extractfile(member) as original, path.open("rb") as actual:
                    if hashlib.file_digest(original, "sha256").digest() != hashlib.file_digest(actual, "sha256").digest():
                        raise ValueError(f"{name}: cached source differs from pinned archive: {path}")
            elif member.issym() and (not path.is_symlink() or path.readlink() != Path(member.linkname)):
                raise ValueError(f"{name}: cached symlink differs from pinned archive: {path}")
    for directory, dirs, files in os.walk(destination, followlinks=False):
        dirs[:] = [name for name in dirs if name != "target"]
        for filename in files + [name for name in dirs if (Path(directory) / name).is_symlink()]:
            relative = (Path(directory) / filename).relative_to(destination)
            if relative != Path(".uplc-scaffold-source.json") and relative not in expected:
                raise ValueError(f"{name}: unexpected cached source file: {relative}")
    return destination


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
    verify_archive(archive, pin)
    destination = cache / name
    marker = destination / ".uplc-scaffold-source.json"
    if marker.exists() and json.loads(marker.read_text()) == pin:
        verify_source(name, pin)
        print(f"{name}: {pin['revision']} cached source verified")
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
