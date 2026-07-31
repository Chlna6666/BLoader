"""Compile an auditable JSON symbol pack into BLoader's compressed BLSYM format."""

from __future__ import annotations

import argparse
import json
import struct
import zlib
from pathlib import Path

MAGIC = b"BLSYM01\0"
MAX_SOURCE_BYTES = 8 * 1024 * 1024


def compile_pack(source: Path, destination: Path) -> None:
    raw = source.read_bytes()
    if not raw or len(raw) > MAX_SOURCE_BYTES:
        raise ValueError("symbol-pack JSON must be between 1 byte and 8 MiB")

    # Parse before packing so invalid source is never deployed as a runtime pack.
    document = json.loads(raw)
    if document.get("format_version") != 1:
        raise ValueError("symbol-pack format_version must be 1")

    destination.parent.mkdir(parents=True, exist_ok=True)
    payload = zlib.compress(raw, level=9, wbits=-zlib.MAX_WBITS)
    destination.write_bytes(MAGIC + struct.pack("<I", len(raw)) + payload)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path, help="source JSON symbol pack")
    parser.add_argument("destination", type=Path, help="output .blsym pack")
    args = parser.parse_args()
    compile_pack(args.source, args.destination)


if __name__ == "__main__":
    main()
