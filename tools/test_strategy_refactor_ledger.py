import unittest

from tools import check_strategy_refactor_ledger as ledger


class ArchitecturePlanStatusSyncTests(unittest.TestCase):
    def setUp(self) -> None:
        statuses = {
            "R01": "complete",
            "R02": "partial",
            "R03": "blocked",
            "R04": "not_started",
        }
        self.packages = [
            {
                "id": package_id,
                "status": statuses.get(package_id, "partial"),
            }
            for package_id in sorted(ledger.EXPECTED_IDS)
        ]

    def markdown(self) -> str:
        rows = []
        for package in self.packages:
            label = ledger.DOC_STATUS_LABELS[package["status"]]
            rows.append(f"| {package['id']} 示例 | P1/{label} | 代码 | — | 验收 |")
        return "\n".join(rows)

    def test_accepts_rows_when_every_status_matches_the_ledger(self) -> None:
        self.assertEqual(
            [],
            ledger.validate_architecture_status_sync(self.packages, self.markdown()),
        )

    def test_reports_a_stale_status(self) -> None:
        markdown = self.markdown().replace("| R01 示例 | P1/完成 |", "| R01 示例 | P1/部分 |")
        errors = ledger.validate_architecture_status_sync(self.packages, markdown)
        self.assertIn(
            "R01: architecture plan status '部分' does not match ledger 'complete'",
            errors,
        )

    def test_reports_a_missing_work_package_row(self) -> None:
        markdown = "\n".join(
            line for line in self.markdown().splitlines() if not line.startswith("| R23 ")
        )
        errors = ledger.validate_architecture_status_sync(self.packages, markdown)
        self.assertTrue(any("missing=['R23']" in error for error in errors))


if __name__ == "__main__":
    unittest.main()
