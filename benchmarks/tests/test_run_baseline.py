from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from process_metrics import ProcessMeasurement
from run_baseline import resource_summary


def sample_measurement(
    *,
    rss: int | None,
    tree_rss: int | None,
    process_count: int,
    cpu: float | None,
    read_bytes: int | None,
    write_bytes: int | None,
) -> ProcessMeasurement:
    return ProcessMeasurement(
        elapsed_ms=100.0,
        stdout="",
        stderr="",
        return_code=0,
        peak_rss_bytes=rss,
        peak_tree_rss_bytes=tree_rss,
        peak_process_count=process_count,
        tree_cpu_seconds=cpu,
        tree_io_read_bytes=read_bytes,
        tree_io_write_bytes=write_bytes,
    )


class ResourceSummaryTests(unittest.TestCase):
    def test_peak_and_median_metrics_preserve_measurement_semantics(self) -> None:
        result = resource_summary(
            [
                sample_measurement(
                    rss=100,
                    tree_rss=150,
                    process_count=2,
                    cpu=0.2,
                    read_bytes=40,
                    write_bytes=10,
                ),
                sample_measurement(
                    rss=120,
                    tree_rss=180,
                    process_count=3,
                    cpu=0.4,
                    read_bytes=60,
                    write_bytes=30,
                ),
            ]
        )
        self.assertEqual(result["peak_process_rss_bytes"], 120)
        self.assertEqual(result["peak_process_tree_rss_bytes"], 180)
        self.assertEqual(result["peak_process_count"], 3)
        self.assertEqual(result["process_tree_cpu_seconds"], 0.3)
        self.assertEqual(result["process_tree_io_read_bytes"], 50.0)
        self.assertEqual(result["process_tree_io_write_bytes"], 20.0)
        self.assertEqual(result["resource_samples"], 2)
        self.assertEqual(result["peak_rss_samples_available"], 2)

    def test_unavailable_measurements_remain_null(self) -> None:
        result = resource_summary(
            [
                sample_measurement(
                    rss=None,
                    tree_rss=None,
                    process_count=1,
                    cpu=None,
                    read_bytes=None,
                    write_bytes=None,
                )
            ]
        )
        self.assertIsNone(result["peak_process_rss_bytes"])
        self.assertIsNone(result["peak_process_tree_rss_bytes"])
        self.assertIsNone(result["process_tree_cpu_seconds"])
        self.assertIsNone(result["process_tree_io_read_bytes"])
        self.assertIsNone(result["process_tree_io_write_bytes"])
        self.assertEqual(result["peak_rss_samples_available"], 0)


if __name__ == "__main__":
    unittest.main()
