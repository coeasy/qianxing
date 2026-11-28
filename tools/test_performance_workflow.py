"""Guard paired performance builds against target-path mismatches."""

from __future__ import annotations

import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
WORKFLOW = ROOT / ".github" / "workflows" / "ci.yml"


class PerformanceWorkflowTests(unittest.TestCase):
    def test_paired_benchmarks_use_the_same_profile_and_full_scope_is_release_only(self) -> None:
        workflow = WORKFLOW.read_text(encoding="utf-8")
        expected = (
            "${{ github.event_name == 'workflow_call' && 'full' || "
            "(github.event_name == 'pull_request' && steps.criterion-scope.outputs.profile || 'pr') }}"
        )
        self.assertEqual(workflow.count(expected), 2)

        detection = workflow.index(
            "- name: Detect changes to measured Rust sources and workload inputs"
        )
        selection = workflow.index("- name: Select PR Criterion scope")
        baseline = workflow.index("- name: Build and measure the parent revision on this runner")
        self.assertLess(detection, selection)
        self.assertLess(selection, baseline)
        detection_step = workflow[detection:selection]
        self.assertIn("benchmarks/measurement_source_changes.py", detection_step)
        self.assertIn("git fetch --no-tags --depth=1 origin", detection_step)

    def test_non_measured_changes_skip_benchmarks_but_release_calls_keep_them(self) -> None:
        workflow = WORKFLOW.read_text(encoding="utf-8")
        performance = workflow[
            workflow.index("  performance-trend:") : workflow.index("  python-wheel-build:")
        ]
        baseline = performance[
            performance.index("- name: Build and measure the parent revision on this runner") :
        ]
        self.assertIn(
            "if: (github.event_name == 'pull_request' || github.event_name == 'push') && "
            "steps.measured_source.outputs.changed == 'true'",
            baseline,
        )
        for step in (
            "- name: Build release CLI",
            "- name: Measure frozen real-data backtest under Linux resource quotas",
            "- name: Measure candidate end-to-end and kernel performance",
            "- name: Block registered regressions above ledger thresholds",
            "- uses: actions/upload-artifact@v4",
        ):
            start = performance.index(step)
            next_step = performance.find("\n      - ", start + len(step))
            block = performance[start : next_step if next_step >= 0 else None]
            self.assertIn(
                "if: github.event_name == 'workflow_call' || steps.measured_source.outputs.changed == 'true'",
                block,
                step,
            )

    def test_parent_and_candidate_benchmarks_use_separate_cargo_targets(self) -> None:
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
            "- name: Block registered regressions above ledger thresholds",
            candidate_benchmark_start,
        )
        baseline_step = workflow[baseline_start:candidate_start]
        candidate_step = workflow[candidate_start:candidate_build_end]
        candidate_benchmark = workflow[candidate_benchmark_start:candidate_benchmark_end]

        self.assertIn(
            "CARGO_TARGET_DIR: ${{ runner.temp }}/qianxing-performance-baseline-target",
            baseline_step,
        )
        self.assertIn("CARGO_TARGET_DIR: ${{ github.workspace }}/target", candidate_step)
        self.assertNotIn('ln -s "$GITHUB_WORKSPACE/target" "$base_dir/target"', baseline_step)
        self.assertIn('mkdir -p "$base_dir/target"', baseline_step)
        self.assertIn('--workspace-root "$base_dir"', baseline_step)
        self.assertIn('--target-dir "$CARGO_TARGET_DIR"', baseline_step)
        self.assertIn('--target-dir "$CARGO_TARGET_DIR"', candidate_benchmark)

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
        self.assertIn("paired reports remain available in the uploaded artifact", comparison_step)


if __name__ == "__main__":
    unittest.main()
