#!/usr/bin/env python3
from __future__ import annotations

import argparse
import re
import subprocess
from pathlib import Path

ALLOWED_TYPES = {
    "feat",
    "fix",
    "perf",
    "docs",
    "ci",
    "test",
    "refactor",
    "chore",
    "release",
}
SUBJECT_RE = re.compile(r"^(?P<kind>[a-z]+)(?:\([^)]+\))?!?:\s+\S")

# Merge commits that pull upstream history into the fork. Their non-first
# parents are upstream commits the fork merges verbatim and cannot rewrite, so
# their (often non-conventional) subjects must not fail this check. They are
# still grouped into fork release notes by fork_release_notes.py, which
# tolerates non-conventional subjects.
UPSTREAM_SYNC_RE = re.compile(
    r"sync with upstream|merge remote-tracking branch '?upstream", re.IGNORECASE
)


def upstream_merged_parents(rev_range: str) -> list[str]:
    """Non-first parents of upstream-sync merge commits within ``rev_range``.

    Excluding history reachable from these tips drops the synced-in upstream
    commits from validation while still checking the fork's own commits
    (including the sync's ``chore:``/``docs:`` commits, which land on the
    mainline, not the upstream side).
    """
    output = subprocess.check_output(
        ["git", "log", "--merges", "--pretty=format:%s%x1f%P", rev_range], text=True
    ).strip()
    excludes: list[str] = []
    for line in output.splitlines():
        subject, _, parents = line.partition("\x1f")
        parent_shas = parents.split()
        if len(parent_shas) > 1 and UPSTREAM_SYNC_RE.search(subject):
            excludes.extend(parent_shas[1:])
    return excludes


def git_subjects(rev_range: str) -> list[str]:
    # Skip merge commits: their subjects (e.g. "Merge remote-tracking branch
    # 'upstream/master'") are not conventional and are excluded from preview
    # release notes anyway. Also exclude upstream commits pulled in by sync
    # merges — the fork cannot rewrite them.
    cmd = ["git", "log", "--no-merges", "--pretty=format:%s", rev_range]
    cmd.extend(f"^{parent}" for parent in upstream_merged_parents(rev_range))
    output = subprocess.check_output(cmd, text=True).strip()
    return [line.strip() for line in output.splitlines() if line.strip()]


def valid_subject(subject: str) -> bool:
    match = SUBJECT_RE.match(subject)
    return bool(match and match.group("kind") in ALLOWED_TYPES)


def commit_message_subject(path: Path) -> str | None:
    for line in path.read_text(encoding="utf-8").splitlines():
        subject = line.strip()
        if subject and not subject.startswith("#"):
            return subject
    return None


def main() -> int:
    parser = argparse.ArgumentParser(description="Validate conventional commit subjects")
    parser.add_argument("subjects", nargs="*")
    parser.add_argument("--range", dest="rev_range")
    parser.add_argument("--message-file")
    args = parser.parse_args()

    subjects = list(args.subjects)
    if args.rev_range:
        subjects.extend(git_subjects(args.rev_range))
    if args.message_file:
        subject = commit_message_subject(Path(args.message_file))
        if subject:
            subjects.append(subject)

    invalid = [subject for subject in subjects if not valid_subject(subject)]
    if invalid:
        print("invalid commit subject(s):")
        for subject in invalid:
            print(f"  {subject}")
        print(
            "commit subjects must use conventional commits because preview notes are generated from them."
        )
        print("example: fix(update): install selected channel")
        print("expected: type(optional-scope): subject")
        print("allowed types: " + ", ".join(sorted(ALLOWED_TYPES)))
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
