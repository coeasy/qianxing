import json
import sys
import unittest
from datetime import date
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from run_ashare_profile_backtest import CALENDAR_PATH, trading_days


class AShareProfileCalendarTests(unittest.TestCase):
    def test_fixture_is_sorted_weekdays_and_covers_the_64k_workload(self):
        fixture = json.loads(CALENDAR_PATH.read_text(encoding="utf-8"))
        values = fixture["trading_days"]
        self.assertEqual(fixture["schema_version"], 1)
        self.assertEqual(fixture["calendar_id"], "cn-equity-sse-szse-2024-2025")
        self.assertTrue(fixture["source"])
        self.assertEqual(len(values), 485)
        self.assertEqual(values, sorted(set(values)))
        self.assertTrue(all(date.fromisoformat(value).weekday() < 5 for value in values))
        self.assertGreaterEqual(len(trading_days((64_000 + 239) // 240)), 267)

    def test_published_holidays_are_not_trading_days(self):
        values = set(json.loads(CALENDAR_PATH.read_text(encoding="utf-8"))["trading_days"])
        self.assertFalse({"2024-02-09", "2024-10-01", "2025-01-29", "2025-10-01"} & values)
        self.assertIn("2024-02-19", values)
        self.assertIn("2025-02-05", values)

    def test_calendar_does_not_silently_extend_beyond_source_coverage(self):
        with self.assertRaisesRegex(ValueError, "covers only 485"):
            trading_days(486)


if __name__ == "__main__":
    unittest.main()
