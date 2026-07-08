#!/usr/bin/env python3
"""Generate fork release notes grouped by conventional commit type.

Fork-only script (saguarocloud/herdr). Reuses the conventional-commit
parsing and grouping from scripts/preview.py so note sections stay in
sync with upstream's preview notes without editing upstream files.
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

try:
    from scripts import preview
except ImportError:  # executed directly as scripts/fork_release_notes.py
    import preview


def _is_merge_subject(subject: str) -> bool:
    return subject.lower().startswith("merge ")


def build_fork_notes(
    previous: str,
    commit: str,
    version: str,
    base_version: str,
    repo: str,
) -> str:
    short = commit[:7]
    lines = [
        f"Fork build `{version}`",
        "",
        f"Built from [`{short}`](https://github.com/{repo}/commit/{commit}) on `master`.",
        f"Base version: {preview.normalize_version(base_version)}",
    ]
    if previous:
        lines.append(f"Compare: https://github.com/{repo}/compare/{previous}...{commit}")
    lines.append("")

    grouped: dict[str, list[str]] = {heading: [] for heading in preview.TYPE_ORDER}
    if previous:
        for subject in preview.commit_subjects(previous, commit):
            if _is_merge_subject(subject):
                continue
            heading, body = preview.humanize_subject(subject)
            grouped.setdefault(heading, []).append(body)

    wrote = False
    for heading in preview.TYPE_ORDER:
        items = grouped.get(heading, [])
        if not items:
            continue
        wrote = True
        lines.append(f"### {heading}")
        for item in items:
            lines.append(f"- {item}")
        lines.append("")

    if not wrote:
        lines.extend(["### Changed", "- Rebuilt fork artifacts from master.", ""])

    return "\n".join(lines).rstrip() + "\n"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--previous", default="", help="previous fork release tag; empty for the first release")
    parser.add_argument("--commit", required=True, help="full commit SHA being released")
    parser.add_argument("--version", required=True, help="fork version, e.g. 0.7.3-f2634a6")
    parser.add_argument("--base-version", required=True, help="Cargo.toml base version")
    parser.add_argument("--repo", required=True, help="owner/name of the fork repository")
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()

    notes = build_fork_notes(
        previous=args.previous.strip(),
        commit=args.commit,
        version=args.version,
        base_version=args.base_version,
        repo=args.repo,
    )
    args.output.write_text(notes, encoding="utf-8")
    print(notes, end="")
    return 0


if __name__ == "__main__":
    sys.exit(main())
