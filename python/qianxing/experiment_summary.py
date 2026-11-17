"""Compact review projections for experiment result documents."""

from __future__ import annotations

from dataclasses import asdict, is_dataclass


def _document(value: object) -> object:
    return asdict(value) if is_dataclass(value) else value


def summarize_experiment(result: object) -> dict[str, object]:
    """Render the long experiment report as a compact strategy review."""
    document = _document(result)
    if not isinstance(document, dict):
        return {"result": document}

    candidates = document.get("candidates", [])
    if isinstance(document.get("ranking"), (list, tuple)) and not isinstance(
        document.get("comparison"), dict
    ):
        by_ordinal = {
            candidate.get("ordinal"): candidate
            for candidate in candidates
            if isinstance(candidate, dict)
        }
        ranking = []
        for item in document.get("ranking", []):
            if not isinstance(item, dict):
                continue
            candidate = by_ordinal.get(item.get("ordinal"), {})
            ranking.append(
                {
                    **item,
                    "parameters": candidate.get("parameters", {}),
                }
            )
        out_of_sample = document.get("out_of_sample")
        fold_selections = []
        if isinstance(out_of_sample, dict):
            for fold in out_of_sample.get("folds", []):
                fold_selections.append(
                    {
                        "fold": fold.get("fold"),
                        "selected_ordinal": fold.get("selected_ordinal"),
                        "parameters": fold.get("selected_parameters", {}),
                        "in_sample_return_bps": fold.get("in_sample_return_bps"),
                        "out_of_sample_return_bps": fold.get("out_of_sample_return_bps"),
                    }
                )
        search_report = document.get("search")
        search_summary = None
        if isinstance(search_report, dict):
            search_folds = []
            for fold in search_report.get("fold_searches", []):
                if not isinstance(fold, dict):
                    continue
                trials = [
                    trial
                    for trial in fold.get("trials", [])
                    if isinstance(trial, dict)
                ]
                trials.sort(
                    key=lambda trial: (
                        trial.get("training_score_bps") is None,
                        -(trial.get("training_score_bps") or 0),
                        trial.get("ordinal", 0),
                    )
                )
                search_folds.append(
                    {
                        "fold": fold.get("fold"),
                        "seed": fold.get("seed"),
                        "top_training_trials": trials[:10],
                    }
                )
            search_summary = {
                "algorithm": search_report.get("algorithm"),
                "objective": search_report.get("objective"),
                "seed": search_report.get("seed"),
                "budget": search_report.get("budget"),
                "fold_local_search": search_report.get("fold_local_search"),
                "max_training_drawdown_bps": search_report.get(
                    "max_training_drawdown_bps"
                ),
                "fold_searches": search_folds,
            }
        return {
            "experiment_id": document.get("experiment_id"),
            "target_strategy_id": document.get("target_strategy_id"),
            "candidate_count": document.get("total_candidates"),
            "succeeded_candidates": document.get("succeeded_candidates"),
            "failed_candidates": document.get("failed_candidates"),
            "ranking_scope": (
                "first_training_fold"
                if isinstance(out_of_sample, dict)
                else "full_input_in_sample"
            ),
            "top_candidates": ranking[:10],
            "search": search_summary,
            "fold_candidate_failures": document.get("fold_candidate_failures", []),
            "out_of_sample": (
                {
                    "in_sample_return_bps": out_of_sample.get("in_sample_return_bps"),
                    "out_of_sample_return_bps": out_of_sample.get("out_of_sample_return_bps"),
                    "degradation_bps": out_of_sample.get("degradation_bps"),
                    "cross_fold_robustness": out_of_sample.get("cross_fold_robustness"),
                    "fold_selections": fold_selections,
                }
                if isinstance(out_of_sample, dict)
                else None
            ),
            **(
                {}
                if isinstance(out_of_sample, dict)
                else {"note": "multi-market grid has no time split; in-sample ranking alone does not establish out-of-sample effectiveness"}
            ),
            "artifact_path": document.get("artifact_path"),
        }

    by_run_id = {
        candidate.get("outcome", {}).get("run_id"): candidate
        for candidate in candidates
        if candidate.get("outcome") is not None
    }


    ranking = []
    comparison = document.get("comparison") or {}
    for item in comparison.get("runs", []):
        run = item.get("run", {})
        candidate = by_run_id.get(run.get("run_id"), {})
        ranking.append(
            {
                "rank": item.get("rank"),
                "ordinal": candidate.get("ordinal"),
                "parameters": candidate.get("parameters", {}),
                "return_bps": run.get("return_bps"),
                "max_drawdown_bps": run.get("max_drawdown_bps"),
                "fills": (candidate.get("outcome") or {}).get("fills"),
                "run_id": run.get("run_id"),
            }
        )

    out_of_sample = document.get("out_of_sample")
    fold_selections = []
    if isinstance(out_of_sample, dict):
        for fold in out_of_sample.get("folds", []):
            selected = next(
                (
                    candidate
                    for candidate in candidates
                    if candidate.get("ordinal") == fold.get("selected_ordinal")
                ),
                {},
            )
            fold_selections.append(
                {
                    "fold": fold.get("fold"),
                    "parameters": fold.get("selected_parameters")
                    or selected.get("parameters", {}),
                    "in_sample_return_bps": fold.get("in_sample_return_bps"),
                    "out_of_sample_return_bps": fold.get("out_of_sample_return_bps"),
                }
            )

    search = document.get("search") or {}
    neighborhood = search.get("neighborhood") or {}
    return {
        "experiment_id": document.get("experiment_id"),
        "candidate_count": document.get("total_candidates"),
        "succeeded_candidates": document.get("succeeded_candidates"),
        "failed_candidates": document.get("failed_candidates"),
        "top_candidates_by_comparison": ranking[:10],
        "out_of_sample": (
            {
                "in_sample_return_bps": out_of_sample.get("in_sample_return_bps"),
                "out_of_sample_return_bps": out_of_sample.get("out_of_sample_return_bps"),
                "degradation_bps": out_of_sample.get("degradation_bps"),
                "cross_fold_robustness": out_of_sample.get("cross_fold_robustness"),
                "fold_selections": fold_selections,
            }
            if isinstance(out_of_sample, dict)
            else None
        ),
        "baselines": document.get("baselines", []),
        "cost_sensitivity": [
            {
                "name": item.get("name"),
                "cost_robustness_passed": item.get("cost_robustness_passed"),
                "return_delta_bps": item.get("return_delta_bps"),
                "error": item.get("error"),
            }
            for item in document.get("cost_sensitivity", [])
        ],
        "parameter_neighborhood_assessment": neighborhood.get("assessment"),
        "artifact_path": document.get("artifact_path"),
    }
