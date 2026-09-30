"""Tests for the fork-only release notes generator."""

import unittest
from unittest import mock

from scripts import fork_release_notes


class BuildForkNotesTest(unittest.TestCase):
    def build(self, subjects, previous="fork-v0.7.2-1111111"):
        # Patched on fork_release_notes itself: the grouping helpers moved here
        # when upstream v0.9.1 deleted them from scripts/preview.py.
        with mock.patch.object(
            fork_release_notes, "commit_subjects", return_value=subjects
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


class UpstreamDependencySurfaceTest(unittest.TestCase):
    """Guard the fork's dependency on upstream's scripts/preview.py.

    Upstream v0.9.1 deleted `commit_subjects`, `humanize_subject` and
    `TYPE_ORDER` from preview.py, which broke this generator and was only
    caught because its tests happened to patch one of them. Nothing asserted
    that the borrowed surface still existed. This does.
    """

    def test_borrowed_upstream_helpers_still_exist(self):
        from scripts import preview

        for name in fork_release_notes.UPSTREAM_PREVIEW_HELPERS:
            self.assertTrue(
                hasattr(preview, name),
                f"scripts/preview.py no longer provides {name!r}; inline it into "
                "scripts/fork_release_notes.py rather than editing upstream",
            )

    def test_grouping_helpers_are_fork_owned(self):
        # These must NOT come back from preview.py: they are fork-only
        # behaviour and upstream has already deleted them once.
        for name in ("TYPE_ORDER", "commit_subjects", "humanize_subject"):
            self.assertTrue(hasattr(fork_release_notes, name))


class HumanizeSubjectTest(unittest.TestCase):
    def test_maps_conventional_types_to_headings(self):
        cases = [
            ("feat: add a thing", "Added", "Add a thing"),
            ("fix(scope): repair a thing", "Fixed", "Repair a thing"),
            ("perf!: speed a thing up", "Performance", "Speed a thing up"),
            ("chore: tidy", "Maintenance", "Tidy"),
            ("not conventional at all", "Other", "Not conventional at all"),
        ]
        for subject, heading, body in cases:
            with self.subTest(subject=subject):
                self.assertEqual(
                    fork_release_notes.humanize_subject(subject), (heading, body)
                )

    def test_release_automation_subjects_are_hidden(self):
        self.assertTrue(
            fork_release_notes.hidden_subject("docs: update preview manifest")
        )
        self.assertFalse(fork_release_notes.hidden_subject("feat: a real change"))
