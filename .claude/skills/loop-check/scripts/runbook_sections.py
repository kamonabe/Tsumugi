#!/usr/bin/env python3
"""docs/loop-runbook.md から、指定した節だけを取り出して表示する。

手順書の正本は loop-runbook.md 1 つのままにし、モードごとに要る節だけを読むために使う。
節番号は見出しの番号で指定する。`6` なら §6 とその小節（6.1〜6.5）すべて、`8.1` なら §8.1 だけを出す。

使い方:
    python3 runbook_sections.py 3 4 5 6 7
    python3 runbook_sections.py --list          # 見出しの一覧だけを出す
    python3 runbook_sections.py --file <path> 6 # 手順書の場所を指定する
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

HEADING = re.compile(r"^(#{2,3}) (\d+(?:\.\d+)?)[.\s]")
DEFAULT = Path(__file__).resolve().parents[4] / "docs" / "loop-runbook.md"


def split(text: str) -> list[tuple[str, int, str]]:
    """見出しごとに (番号, 見出しの深さ, その見出しから次の見出しの手前まで) に分ける。"""
    parts: list[tuple[str, int, list[str]]] = []
    in_code = False
    for line in text.splitlines():
        if line.startswith("```"):
            in_code = not in_code
        m = None if in_code else HEADING.match(line)
        if m:
            parts.append((m.group(2), len(m.group(1)), [line]))
        elif parts:
            parts[-1][2].append(line)
    return [(num, depth, "\n".join(lines).rstrip() + "\n") for num, depth, lines in parts]


def select(parts: list[tuple[str, int, str]], wanted: list[str]) -> tuple[str, list[str]]:
    """指定の節（と、番号が `N.` で始まる小節）を手順書の順に連結する。見つからない番号も返す。"""
    out, hit = [], set()
    for num, _depth, body in parts:
        for w in wanted:
            if num == w or num.startswith(w + "."):
                out.append(body)
                hit.add(w)
                break
    return "\n".join(out), [w for w in wanted if w not in hit]


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("sections", nargs="*", help="節番号（例: 6 7 8.1 14.4）")
    ap.add_argument("--file", type=Path, default=DEFAULT)
    ap.add_argument("--list", action="store_true", help="見出しの一覧を出す")
    args = ap.parse_args()
    parts = split(args.file.read_text(encoding="utf-8"))
    if args.list or not args.sections:
        for _num, _depth, body in parts:
            print(body.splitlines()[0])
        return 0
    text, missing = select(parts, args.sections)
    sys.stdout.write(text)
    if missing:
        # 見つからない番号があれば、推測で読み替えずに手順書全体を読むよう促す
        print(f"\n[runbook_sections] 見つからない節: {' '.join(missing)}。"
              f"手順書の番号が変わった可能性がある。{args.file} 全体を読むこと。", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
