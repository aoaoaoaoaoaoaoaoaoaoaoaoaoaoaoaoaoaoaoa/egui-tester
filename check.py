#!/usr/bin/env python3
from __future__ import annotations

import argparse
import os
import subprocess
import tomllib
from pathlib import Path, PurePosixPath

ROOT = Path(__file__).resolve().parent
IGNORED = frozenset(
    {".git", ".hg", ".jj", ".svn", "__pycache__", "node_modules", "target", "vendor"}
)


def command(value: object, key: str) -> tuple[str, ...]:
    if not isinstance(value, list) or not value or not all(
        isinstance(part, str) and part for part in value
    ):
        raise SystemExit(f"[check] invalid {key}: expected a non-empty string list")
    return tuple(value)


def run(label: str, argv: tuple[str, ...]) -> None:
    print(f"[check] {label}: {' '.join(argv)}", flush=True)
    result = subprocess.run(argv, cwd=ROOT)
    if result.returncode:
        raise SystemExit(result.returncode)


def matches(path: PurePosixPath, pattern: str) -> bool:
    return path.match(pattern) or (
        pattern.startswith("**/") and path.match(pattern.removeprefix("**/"))
    )


def enforce_source_cap(metadata: dict[str, object]) -> None:
    raw = metadata["source_files"]
    if not isinstance(raw, dict):
        raise SystemExit("[check] source_files must be a table")
    limit = raw["max_lines"]
    includes = raw["include"]
    excludes = raw["exclude"]
    if not isinstance(limit, int) or limit <= 0:
        raise SystemExit("[check] max_lines must be a positive integer")
    if not isinstance(includes, list) or not isinstance(excludes, list):
        raise SystemExit("[check] include and exclude must be lists")

    violations: list[tuple[PurePosixPath, int]] = []
    for root, dirs, files in os.walk(ROOT):
        dirs[:] = sorted(name for name in dirs if name not in IGNORED)
        for name in files:
            path = Path(root, name)
            relative = PurePosixPath(path.relative_to(ROOT).as_posix())
            if not any(matches(relative, pattern) for pattern in includes):
                continue
            if any(matches(relative, pattern) for pattern in excludes):
                continue
            lines = len(path.read_text(encoding="utf-8").splitlines())
            if lines > limit:
                violations.append((relative, lines))
    print(f"[check] source-files: max {limit} lines", flush=True)
    for path, lines in violations:
        print(f"[check] source-files: {path}: {lines} lines", flush=True)
    if violations:
        raise SystemExit(1)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "mode",
        nargs="?",
        choices=("check", "verify", "deep", "fix", "canon"),
        default="check",
    )
    args = parser.parse_args()
    workspace = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))
    metadata = workspace["workspace"]["metadata"]["rust-starter"]
    canonical = tuple(
        command(value, f"canonicalize_commands[{index}]")
        for index, value in enumerate(metadata["canonicalize_commands"], 1)
    )
    if args.mode in {"fix", "canon"}:
        for index, argv in enumerate(canonical, 1):
            run(f"canonicalize.{index}", argv)
        return
    enforce_source_cap(metadata)
    if args.mode != "verify":
        for index, argv in enumerate(canonical, 1):
            run(f"canonicalize.{index}", argv)
    run("fmt", command(metadata["format_command"], "format_command"))
    run("clippy", command(metadata["clippy_command"], "clippy_command"))
    run("test", command(metadata["test_command"], "test_command"))
    if args.mode == "deep":
        run("doc", command(metadata["doc_command"], "doc_command"))


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        raise SystemExit(130)
