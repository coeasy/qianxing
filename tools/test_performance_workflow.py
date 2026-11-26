"""Guard paired performance builds against target-path mismatches."""

from __future__ import annotations

import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
WORKFLOW = ROOT / ".github" / "workflows" / "ci.yml"


class PerformanceWorkflowTests(unittest.TestCase):
    def test_parent_and_candidate_benchmarks_use_the_shared_cargo_target(self) -> None:
        workflow = WORKFLOW.read_text(encoding="utf-8")
        baseline_start = workflow.index("- name: Build and measure the parent revision on this runner")
        candidate_start = workflow.index("- name: Build release CLI", baseline_start)
        candidate_build_end = workflow.index(
            "- name: Measure frozen real-data backtest under Linux resource quotas",
            candidate_start,
        )
        candidate_benchmark_start = workflow.index(
            "- name: Measure candidate end-to-end and kernel performance",
            candidate_build_end,
        )
        candidate_benchmark_end = workflow.index(
            "- name: Detect changes to measured Rust sources and workload inputs",
            candidate_benchmark_start,
        )
        baseline_step = workflow[baseline_start:candidate_start]
        candidate_step = workflow[candidate_start:candidate_build_end]
        candidate_benchmark = workflow[candidate_benchmark_start:candidate_benchmark_end]

        target_dir = "CARGO_TARGET_DIR: ${{ github.workspace }}/target"
        self.assertIn(target_dir, baseline_step)
        self.assertIn(target_dir, candidate_step)
        self.assertIn('ln -s "$GITHUB_WORKSPACE/target" "$base_dir/target"', baseline_step)
        self.assertIn('--workspace-root "$base_dir"', baseline_step)
        self.assertIn('--target-dir "$CARGO_TARGET_DIR"', baseline_step)
        self.assertIn('--target-dir "$CARGO_TARGET_DIR"', candidate_benchmark)
        self.assertIn("paired reports remain available in the uploaded artifact", workflow)

    def test_regression_gate_runs_only_when_measured_inputs_changed(self) -> None:
        workflow = WORKFLOW.read_text(encoding="utf-8")
        detection = workflow.index(
            "- name: Detect changes to measured Rust sources and workload inputs"
        )
        comparison = workflow.index("- name: Block registered regressions above ledger thresholds")
        comparison_end = workflow.index("- uses: actions/upload-artifact@v4", comparison)
        detection_step = workflow[detection:comparison]
        comparison_step = workflow[comparison:comparison_end]

        self.assertIn("benchmarks/measurement_source_changes.py", detection_step)
        self.assertIn("steps.measured_source.outputs.changed", comparison_step)
        self.assertIn(
            'if [[ "${{ steps.paired_baseline.outputs.available }}" == "true" && '
            '"${{ steps.measured_source.outputs.changed }}" == "true" ]]; then',
            comparison_step,
        )
<<<<<<< HEAD
        self.assertIn("paired reports remain available in the uploaded artifact", comparison_step)
=======
        self.assertIn('--target-dir "$CARGO_TARGET_DIR"', baseline_step)
        self.assertIn('--target-dir "$CARGO_TARGET_DIR"', candidate_benchmark)
        self.assertNotIn('ln -s "$GITHUB_WORKSPACE/target" "$base_dir/target"', baseline_step)
>>>>>>> 2ed4bfa8 (ci: route benchmarks to isolated cargo targets)


if __name__ == "__main__":
    unittest.main()
