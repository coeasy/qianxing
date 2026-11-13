from __future__ import annotations

import unittest

from hardware import _linux_hardware_details


class LinuxHardwareDetailsTests(unittest.TestCase):
    def test_reads_model_and_deduplicates_hyperthreads_by_socket_and_core(self) -> None:
        cpuinfo = """processor : 0
model name : Example CPU
physical id : 0
core id : 0

processor : 1
model name : Example CPU
physical id : 0
core id : 0

processor : 2
model name : Example CPU
physical id : 1
core id : 0
"""
        self.assertEqual(_linux_hardware_details(cpuinfo), (2, "Example CPU"))

    def test_leaves_physical_cores_unknown_when_topology_is_not_exposed(self) -> None:
        cpuinfo = "processor : 0\nmodel name : Virtual CPU\n\nprocessor : 1\nmodel name : Virtual CPU\n"
        self.assertEqual(_linux_hardware_details(cpuinfo), (None, "Virtual CPU"))
