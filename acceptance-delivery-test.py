#!/usr/bin/env python3
"""Acceptance check for the scoped repository delivery capability.

Reads ``acceptance-delivery.txt`` as bytes, asserts it equals the requested
UTF-8 payload exactly (one trailing newline, no decoding), and prints the
file's SHA-256 digest.

Standard library only; run with ``python3 acceptance-delivery-test.py``.
"""

# Acceptance D2: actual Source-validated PR report

import hashlib
import pathlib
import sys

TEXT_FILE = pathlib.Path(__file__).with_name("acceptance-delivery.txt")

# "xharness repository delivery " + U+1F600 GRINNING FACE + one LF, as UTF-8.
EXPECTED = "xharness repository delivery \U0001F600\n".encode("utf-8")


def main() -> int:
    actual = TEXT_FILE.read_bytes()
    if actual != EXPECTED:
        print(f"FAIL: byte mismatch for {TEXT_FILE.name}", file=sys.stderr)
        print(f"  expected ({len(EXPECTED)} bytes): {EXPECTED!r}", file=sys.stderr)
        print(f"  actual   ({len(actual)} bytes): {actual!r}", file=sys.stderr)
        return 1

    print(f"OK: {TEXT_FILE.name} is exactly {len(EXPECTED)} UTF-8 bytes")
    print("sha256: " + hashlib.sha256(actual).hexdigest())
    return 0


if __name__ == "__main__":
    sys.exit(main())
