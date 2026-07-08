"""Tests for the fork-only release notes generator."""

import unittest
from unittest import mock

from scripts import fork_release_notes


class BuildForkNotesTest(unittest.TestCase):
    def build(self, subjects, previous="fork-v0.7.2-1111111"):
        with mock.patch.object(
            fork_release_notes.preview, "commit_subjects", return_value=subjects
        ):
            return fork_release_notes.build_fork_notes(
                previous=previous,
                commit="f2634a6000000000000000000000000000000000",
                version="0.7.3-f2634a6",
                base_version="0.7.3",
                repo="saguarocloud/herdr",
            )

    def test_groups_conventional_commits_by_type(self):
        notes = self.build(
            [
                "feat: add fork release pipeline",
                "fix: handle pane focus",
                "perf: cache screen snapshots",
                "docs: update fork notes",
            ]
        )

        self.assertIn("### Added\n- Add fork release pipeline", notes)
        self.assertIn("### Fixed\n- Handle pane focus", notes)
        self.assertIn("### Performance\n- Cache screen snapshots", notes)
        self.assertIn("### Maintenance\n- Update fork notes", notes)

    def test_merge_subjects_are_excluded(self):
        notes = self.build(
            [
                "Merge remote-tracking branch 'upstream/master'",
                "Merge pull request #3 from saguarocloud/feat/x",
                "feat: keep this one",
            ]
        )

        self.assertNotIn("Merge", notes)
        self.assertIn("- Keep this one", notes)

    def test_non_conventional_subject_lands_in_other(self):
        notes = self.build(["tidy up readme wording"])

        self.assertIn("### Other\n- Tidy up readme wording", notes)

    def test_header_includes_version_commit_and_compare_link(self):
        notes = self.build(["feat: something"])

        self.assertIn("Fork build `0.7.3-f2634a6`", notes)
        self.assertIn(
            "[`f2634a6`](https://github.com/saguarocloud/herdr/commit/"
            "f2634a6000000000000000000000000000000000)",
            notes,
        )
        self.assertIn("Base version: 0.7.3", notes)
        self.assertIn(
            "Compare: https://github.com/saguarocloud/herdr/compare/"
            "fork-v0.7.2-1111111...f2634a6000000000000000000000000000000000",
            notes,
        )

    def test_first_release_without_previous_tag_uses_fallback_section(self):
        notes = self.build([], previous="")

        self.assertNotIn("Compare:", notes)
        self.assertIn("### Changed\n- Rebuilt fork artifacts from master.", notes)

    def test_empty_range_uses_fallback_section(self):
        notes = self.build([])

        self.assertIn("### Changed\n- Rebuilt fork artifacts from master.", notes)


if __name__ == "__main__":
    unittest.main()
