from __future__ import annotations

import tempfile
import sys
import unittest
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import runtime_limits


class RuntimeLimitsTest(unittest.TestCase):
    def test_cgroup_v2_quota_cpuset_and_memory_are_reported_as_observed(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "cgroup.controllers").write_text("cpu cpuset memory", encoding="ascii")
            (root / "cpu.max").write_text("400000 100000", encoding="ascii")
            (root / "cpuset.cpus.effective").write_text("2-5,7", encoding="ascii")
            (root / "memory.max").write_text("8589934592", encoding="ascii")
            (root / "memory.current").write_text("1048576", encoding="ascii")

            with patch.object(runtime_limits.platform, "system", return_value="Linux"):
                actual = runtime_limits.runtime_limits(
                    cgroup_root=root, cpu_affinity={2, 3, 4, 5}
                )

        self.assertEqual(actual["cgroup_version"], 2)
        self.assertEqual(actual["cgroup_controllers_visible"], ["cpu", "cpuset", "memory"])
        self.assertEqual(actual["cgroup_cpu_limit_count"], 4.0)
        self.assertEqual(actual["cgroup_cpuset_cpus_effective"], [2, 3, 4, 5, 7])
        self.assertEqual(actual["cgroup_memory_limit_bytes"], 8589934592)
        self.assertEqual(actual["cgroup_memory_current_bytes"], 1048576)
        self.assertEqual(actual["cpu_affinity_count"], 4)
        self.assertTrue(actual["cgroup_files_visible"])

    def test_unavailable_cgroup_limits_remain_null_and_are_marked_unavailable(self):
        with tempfile.TemporaryDirectory() as temporary:
            with patch.object(runtime_limits.platform, "system", return_value="Linux"):
                actual = runtime_limits.runtime_limits(
                    cgroup_root=Path(temporary), cpu_affinity={1, 3}
                )

        self.assertIsNone(actual["cgroup_version"])
        self.assertIsNone(actual["cgroup_cpu_limit_count"])
        self.assertIsNone(actual["cgroup_memory_limit_bytes"])
        self.assertFalse(actual["cgroup_files_visible"])
        self.assertEqual(actual["cpu_affinity"], [1, 3])

    def test_unlimited_cpu_and_memory_do_not_become_fabricated_limits(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "cgroup.controllers").write_text("cpu memory", encoding="ascii")
            (root / "cpu.max").write_text("max 100000", encoding="ascii")
            (root / "memory.max").write_text("max", encoding="ascii")
            with patch.object(runtime_limits.platform, "system", return_value="Linux"):
                actual = runtime_limits.runtime_limits(cgroup_root=root, cpu_affinity={0})

        self.assertEqual(actual["cgroup_version"], 2)
        self.assertIsNone(actual["cgroup_cpu_quota_micros"])
        self.assertEqual(actual["cgroup_cpu_period_micros"], 100000)
        self.assertIsNone(actual["cgroup_cpu_limit_count"])
        self.assertIsNone(actual["cgroup_memory_limit_bytes"])


if __name__ == "__main__":
    unittest.main()
