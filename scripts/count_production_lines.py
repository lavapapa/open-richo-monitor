#!/usr/bin/env python3
"""Count handwritten production and test lines under apps/ and crates/."""

from __future__ import annotations

import json
import re
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SOURCE_SUFFIXES = {".rs", ".ts", ".tsx", ".js", ".jsx", ".svelte", ".css", ".html"}
IGNORED_DIRS = {".git", ".svelte-kit", "build", "coverage", "dist", "node_modules", "target"}
TEST_MODULE = re.compile(r"#\s*\[\s*cfg\s*\(\s*test\s*\)\s*\]\s*\n\s*mod\s+\w+\s*\{")


def code_mask(source: str) -> str:
    """Blank comments and Rust literals while preserving offsets and newlines."""
    result = list(source)
    i = 0

    def blank(start: int, end: int) -> None:
        for position in range(start, end):
            if result[position] != "\n":
                result[position] = " "

    while i < len(source):
        if source.startswith("//", i):
            end = source.find("\n", i)
            end = len(source) if end == -1 else end
            blank(i, end)
            i = end
            continue
        if source.startswith("/*", i):
            start = i
            depth = 1
            i += 2
            while i < len(source) and depth:
                if source.startswith("/*", i):
                    depth += 1
                    i += 2
                elif source.startswith("*/", i):
                    depth -= 1
                    i += 2
                else:
                    i += 1
            blank(start, i)
            continue

        raw_start = i
        prefix = i
        if source.startswith(("br", "cr"), i):
            prefix += 1
        if prefix < len(source) and source[prefix] == "r":
            quote = prefix + 1
            while quote < len(source) and source[quote] == "#":
                quote += 1
            if quote < len(source) and source[quote] == '"':
                hashes = quote - prefix - 1
                ending = '"' + "#" * hashes
                end = source.find(ending, quote + 1)
                end = len(source) if end == -1 else end + len(ending)
                blank(raw_start, end)
                i = end
                continue

        if source[i] == '"':
            start = i
            i += 1
            while i < len(source):
                if source[i] == "\\":
                    i += 2
                elif source[i] == '"':
                    i += 1
                    break
                else:
                    i += 1
            blank(start, min(i, len(source)))
            continue

        if source[i] == "'":
            end = i + 1
            escaped = False
            while end < len(source) and source[end] != "\n":
                if not escaped and source[end] == "'":
                    end += 1
                    blank(i, end)
                    i = end
                    break
                if source[end] == "\\" and not escaped:
                    escaped = True
                else:
                    escaped = False
                end += 1
            else:
                i += 1
            continue

        i += 1
    return "".join(result)


def matching_brace(masked: str, opening: int) -> int:
    depth = 0
    for index in range(opening, len(masked)):
        if masked[index] == "{":
            depth += 1
        elif masked[index] == "}":
            depth -= 1
            if depth == 0:
                return index
    raise ValueError(f"unclosed test module at character {opening}")


def rust_test_ranges(source: str) -> list[tuple[int, int]]:
    masked = code_mask(source)
    ranges = []
    for match in TEST_MODULE.finditer(masked):
        opening = masked.find("{", match.start(), match.end())
        closing = matching_brace(masked, opening)
        first_line = source.count("\n", 0, match.start()) + 1
        last_line = source.count("\n", 0, closing) + 1
        ranges.append((first_line, last_line))
    return ranges


def should_skip(path: Path) -> bool:
    return any(part in IGNORED_DIRS for part in path.parts)


def self_test() -> None:
    source = '''#[cfg(test)]
mod tests {
    fn braces() { let _ = r#" } { still literal "#; }
    /* nested /* { */ comment */
    fn quote() { let _ = '}'; }
}
fn production() { let _ = "{ not a block }"; }
'''
    assert rust_test_ranges(source) == [(1, 6)]
    assert rust_test_ranges("#[cfg(test)]\nmod tests { fn f() {} }\nfn after() {}\n") == [(1, 2)]


def count() -> dict[str, object]:
    files = []
    for base in (ROOT / "apps", ROOT / "crates"):
        for path in base.rglob("*"):
            if not path.is_file() or should_skip(path.relative_to(ROOT)) or path.suffix not in SOURCE_SUFFIXES:
                continue
            relative = path.relative_to(ROOT).as_posix()
            source = path.read_text(encoding="utf-8")
            lines = source.splitlines()
            ranges: list[tuple[int, int]] = []
            if "/tests/" in f"/{relative}/" or path.name.endswith(".test.ts"):
                test_lines = len(lines)
                production_lines = 0
                ranges = [(1, len(lines))] if lines else []
            else:
                ranges = rust_test_ranges(source) if path.suffix == ".rs" else []
                test_indexes = {
                    line
                    for first, last in ranges
                    for line in range(first, last + 1)
                }
                test_lines = len(test_indexes)
                production_lines = len(lines) - test_lines
            files.append({
                "path": relative,
                "production_lines": production_lines,
                "test_lines": test_lines,
                "source_lines": len(lines),
                "test_module_ranges_1based_inclusive": [list(item) for item in ranges],
            })

    files.sort(key=lambda item: str(item["path"]))
    return {
        "baseline_production_lines": 9542,
        "current_source_files": len(files),
        "current_production_lines": sum(int(item["production_lines"]) for item in files),
        "current_test_lines": sum(int(item["test_lines"]) for item in files),
        "files": files,
    }


if __name__ == "__main__":
    import sys

    if sys.argv[1:] == ["--self-test"]:
        self_test()
        print("Rust test-module boundary checks passed.")
    else:
        print(json.dumps(count(), ensure_ascii=False, indent=2))
