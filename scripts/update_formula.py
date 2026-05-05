#!/usr/bin/env python3
"""Update version and sha256 values in a Homebrew formula file.

Usage:
    python3 update_formula.py <formula_path> <version> <arm64_sha256> <x86_64_sha256>

The formula must have exactly one `version "..."` line and exactly two
`sha256 "..."` lines (arm64 first, x86_64 second).
"""
import re
import sys


def main() -> None:
    if len(sys.argv) != 5:
        print(f"Usage: {sys.argv[0]} <formula> <version> <arm_sha> <x86_sha>", file=sys.stderr)
        sys.exit(1)

    formula_path, version, arm_sha, x86_sha = sys.argv[1:]

    with open(formula_path, encoding="utf-8") as f:
        content = f.read()

    # Update version line.
    content, n = re.subn(r'version "[^"]*"', f'version "{version}"', content)
    if n != 1:
        print(f"ERROR: expected 1 version line, found {n}", file=sys.stderr)
        sys.exit(1)

    # Replace sha256 values in document order: first = arm64, second = x86_64.
    replacements = iter([arm_sha, x86_sha])

    def _replace(m: re.Match) -> str:
        sha = next(replacements, None)
        if sha is None:
            print("ERROR: more than 2 sha256 lines found", file=sys.stderr)
            sys.exit(1)
        return f'sha256 "{sha}"'

    content, n = re.subn(r'sha256 "[^"]*"', _replace, content)
    if n != 2:
        print(f"ERROR: expected 2 sha256 lines, found {n}", file=sys.stderr)
        sys.exit(1)

    with open(formula_path, "w", encoding="utf-8") as f:
        f.write(content)

    print(f"Updated {formula_path}")
    print(f"  version : {version}")
    print(f"  arm64   : {arm_sha[:16]}...")
    print(f"  x86_64  : {x86_sha[:16]}...")


if __name__ == "__main__":
    main()
