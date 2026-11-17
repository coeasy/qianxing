"""tools/check_markdown_links.py 的引用判据，钉在合成数据上。

CI 的 workflow-lint 步骤跑的是仓库当前状态：今天 0 条错拼，它就绿。但「0 条」和「判据看不见」
是同一张脸——把扫描正则弄瞎（少一个斜杠分组、结尾锚写错）之后，这一步照样退 0。本地门禁用
examined 下限接住了这一类，可那个门禁在 .task-tmp/ 里，不在任何人的签出里。

所以这里把四张脸固定下来，不依赖磁盘也不依赖 git：唯一命中的错拼要判、多个大小写变体故意不判、
./ 前缀是导航不是拼写、裸文件名与 URL 根本不参与判定。任何一次把判据改瞎的提交，都会在
workflow-lint 之外多一条红。
"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path, PurePosixPath

TOOLS = Path(__file__).resolve().parents[2] / "tools"
if str(TOOLS) not in sys.path:
    sys.path.insert(0, str(TOOLS))

import check_markdown_links as checker  # noqa: E402

SOURCE = PurePosixPath("docs/changelog/current.md")


def index(*paths: str) -> tuple[set[str], dict[str, list[str]]]:
    """Rebuild the two structures main() feeds path_citations, without touching git or disk."""
    tracked = set(paths)
    by_lowercase: dict[str, list[str]] = {}
    for path in sorted(tracked):
        by_lowercase.setdefault(path.lower(), []).append(path)
    return tracked, by_lowercase


def judge(content: str, *paths: str) -> tuple[int, int, list[str]]:
    tracked, by_lowercase = index(*paths)
    return checker.path_citations(Path(SOURCE), content, tracked, by_lowercase)


class PathCitationRuleTest(unittest.TestCase):
    def test_exact_spelling_is_examined_and_cleared(self) -> None:
        examined, ambiguous, failures = judge("`docs/a.md` 已跟踪", "docs/a.md")
        self.assertEqual((examined, ambiguous), (1, 0))
        self.assertEqual(failures, [])

    def test_unique_case_variant_is_flagged_once_per_occurrence(self) -> None:
        _tracked, _low, failures = judge("`Docs/A.md` 与 `Docs/A.md` 各算一次", "docs/a.md")
        self.assertEqual(len(failures), 2)
        for row in failures:
            self.assertIn("case-mismatched path citation", row)
            # 判据行必须同时给出两种拼写：只说「错了」的报错没人会去修。
            self.assertIn("Docs/A.md (tracked as docs/a.md)", row)

    def test_multiple_case_variants_are_left_unjudged(self) -> None:
        examined, ambiguous, failures = judge("`DOCS/A.md`", "docs/a.md", "Docs/A.md")
        self.assertEqual(examined, 1)
        self.assertEqual(failures, [])
        self.assertEqual(ambiguous, 1)

    def test_dot_slash_prefix_is_navigation_not_spelling(self) -> None:
        examined, _ambiguous, failures = judge("`./build.sh`", "build.sh")
        self.assertEqual((examined, failures), (1, []))
        # 归一化不能顺手把判据关掉：带前缀的错拼仍然是错拼。
        _tracked, _low, mis_cased = judge("`./Build.sh`", "build.sh")
        self.assertEqual(len(mis_cased), 1)
        self.assertIn("./Build.sh (tracked as build.sh)", mis_cased[0])

    def test_bare_file_name_is_not_a_path_citation(self) -> None:
        # 斜杠分组是这条规则的边界。本轮实测：带斜杠的引用 4617 行、裸名 3363 行，两个集合几乎不重叠，
        # 所以去掉分组不是「更严」，而是换一批对象去判——判据的可达性也会从 4617 塌下去。
        self.assertEqual(judge("`clock.rs`", "crates/core/src/clock.rs"), (0, 0, []))
        self.assertIsNone(checker.PATH_CITATION.match("clock.rs"))
        self.assertIsNotNone(checker.PATH_CITATION.match("crates/core/src/clock.rs"))

    def test_remote_and_non_path_backticks_are_skipped(self) -> None:
        content = "`https://example.com/docs/A.md` 与 `plain words` 与 `docs/a.md.bak.bak.longext`"
        self.assertEqual(judge(content, "docs/A.md"), (0, 0, []))

    def test_link_grammar_leaves_non_local_targets_alone(self) -> None:
        for target in ("#section", "https://example.com/x", ""):
            self.assertTrue(checker.local_target_exists(SOURCE, target), target)


if __name__ == "__main__":
    unittest.main()
