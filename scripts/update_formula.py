#!/usr/bin/env python3
"""Update version and sha256 in a Homebrew formula file.

Usage:
    python3 update_formula.py <formula_path> <version> <arm64_sha256>

The formula must have exactly one `version "..."` line and exactly one
`sha256 "..."` line (arm64 only).
"""
import re
import sys


def main() -> None:
    if len(sys.argv) != 4:
        print(f"Usage: {sys.argv[0]} <formula> <version> <arm_sha>", file=sys.stderr)
        sys.exit(1)

    formula_path, version, arm_sha = sys.argv[1:]

    with open(formula_path, encoding="utf-8") as f:
        content = f.read()

    content, n = re.subn(r'version "[^"]*"', f'version "{version}"', content)
    if n != 1:
        print(f"ERROR: expected 1 version line, found {n}", file=sys.stderr)
        sys.exit(1)

    content, n = re.subn(r'sha256 "[^"]*"', f'sha256 "{arm_sha}"', content)
    if n != 1:
        print(f"ERROR: expected 1 sha256 line, found {n}", file=sys.stderr)
        sys.exit(1)

    with open(formula_path, "w", encoding="utf-8") as f:
        f.write(content)

    print(f"Updated {formula_path}")
    print(f"  version : {version}")
    print(f"  arm64   : {arm_sha[:16]}...")


if __name__ == "__main__":
    main()
