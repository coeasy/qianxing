#include <qianxing_app.hpp>

#include <cassert>
#include <string>

int main() {
    std::string observed_path;
    std::string observed_body;
    std::string observed_method;
    qianxing::AppClient client([&](std::string_view method, std::string_view path,
                                  std::string_view body) {
        observed_method.assign(method);
        observed_path.assign(path);
        observed_body.assign(body);
        return qianxing::AppResponse{200, R"({"runs":[]})"};
    });

    const std::string spec =
        R"({"schema_version":1,"experiment_id":"smoke","strategy_kinds":["ema_cross","sma_cross"]})";
    const std::string sized_backtest_spec =
        R"({"schema_version":1,"run_id":"sized","instrument":"BTCUSDT.BINANCE","bars_path":"bars.json","settlement_currency":"USDT","initial_cash_raw":1000000000000,"seed":1,"output_dir":"runs","account_id":"acct.cpp-sdk","strategy":{"kind":"sma_cross","strategy_id":"sized-sma","quantity_raw":1000000000,"target_volatility_bps":250,"fast_window":2,"slow_window":4}})";
    const std::string depth_spec =
        R"({"schema_version":1,"tier":"l1","market_spec":{"product":"perpetual"},"order_policy":{"margin_mode":"cross","leverage":2}})";
    const std::string depth_experiment_spec =
        R"({"schema_version":1,"experiment_id":"depth-classic","base":{"schema_version":1,"run_id":"unused-depth","depth_path":"ticks.json","tier":"l2","settlement_currency":"USDT","initial_cash_raw":1000000000000,"fee_bps":0,"latency_snapshots":0,"queue_position_bps":0,"market_impact_bps":0,"output_dir":"runs","strategy":{"kind":"sma_cross","strategy_id":"depth-sma","quantity_raw":1000000000,"fast_window":2,"slow_window":3}},"parameter_space":[],"strategy_kinds":["ema_cross","sma_cross"]})";
    const std::string multi_leg_experiment_spec =
        R"({"schema_version":1,"experiment_id":"multi-leg-classics","base":{"schema_version":1,"run_id":"unused-pairs","primary_bars_path":"primary.json","reference_bars_path":"reference.json","settlement_currency":"USDT","primary_initial_cash_raw":1000000000000,"reference_initial_cash_raw":1000000000000,"seed":7,"output_dir":"runs","strategy":{"kind":"pairs_arbitrage","strategy_id":"pairs","quantity_raw":1000000000}},"parameter_space":[],"strategy_kinds":["pairs_arbitrage","basis_arbitrage","cross_venue_arbitrage","spot_futures_arbitrage"]})";
    const auto response = client.run_experiment(spec);
    assert(response.ok());
    assert(response.body == R"({"runs":[]})");
    assert(observed_path == "/app/run-experiment");
    assert(observed_body == spec);

    const auto strategy_catalog = client.builtin_strategy_catalog();
    assert(strategy_catalog.ok());
    assert(observed_method == "GET");
    assert(observed_path == "/app/strategies");

    const auto multi_market = client.run_multi_market_backtest(spec);
    assert(multi_market.ok());
    assert(observed_path == "/app/multi-market-backtest");
    assert(observed_body == spec);

    const auto started_multi_market = client.start_multi_market_backtest(spec);
    assert(started_multi_market.ok());
    assert(observed_path == "/app/multi-market-backtest/start");
    assert(observed_body == spec);

    const auto market_experiment = client.run_multi_market_experiment(spec);
    assert(market_experiment.ok());
    assert(observed_path == "/app/run-multi-market-experiment");
    assert(observed_body == spec);

    const auto started_market_experiment = client.start_multi_market_experiment(spec);
    assert(started_market_experiment.ok());
    assert(observed_path == "/app/run-multi-market-experiment/start");
    assert(observed_body == spec);

    const auto depth = client.run_depth_backtest(depth_spec);
    assert(depth.ok());
    assert(observed_path == "/app/depth-backtest");
    assert(observed_body == depth_spec);

    const auto depth_experiment = client.run_depth_experiment(depth_experiment_spec);
    assert(depth_experiment.ok());
    assert(observed_path == "/app/run-depth-experiment");
    assert(observed_body == depth_experiment_spec);

    const auto algo_start = client.start_paper_algo_execution(spec);
    assert(algo_start.ok());
    assert(observed_path == "/app/paper/algo-executions/start");
    assert(observed_body == spec);
    const auto algo_get = client.get_paper_algo_execution(spec);
    assert(algo_get.ok());
    assert(observed_path == "/app/paper/algo-executions/get");
    const auto algo_cancel = client.cancel_paper_algo_execution(spec);
    assert(algo_cancel.ok());
    assert(observed_path == "/app/paper/algo-executions/cancel");
    const auto algo_list = client.list_paper_algo_executions(spec);
    assert(algo_list.ok());
    assert(observed_path == "/app/paper/algo-executions/list");

    const auto live_events = client.get_live_events(42);
    assert(live_events.ok());
    assert(observed_method == "GET");
    assert(observed_path == "/events/live?after=42");
    const auto initial_events = client.get_live_events();
    assert(initial_events.ok());
    assert(observed_method == "GET");
    assert(observed_path == "/events/live");

    const auto risk_parity = client.plan_risk_parity(spec);
    assert(risk_parity.ok());
    assert(observed_path == "/app/plan-risk-parity");
    assert(observed_body == spec);

    const auto capital_allocation = client.plan_capital_allocation(spec);
    assert(capital_allocation.ok());
    assert(observed_path == "/app/plan-capital-allocation");
    assert(observed_body == spec);

    const auto multi_leg = client.run_multi_leg_backtest(spec);
    assert(multi_leg.ok());
    assert(observed_path == "/app/multi-leg-backtest");
    assert(observed_body == spec);

    const auto multi_account = client.run_multi_account_backtest(spec);
    assert(multi_account.ok());
    assert(observed_path == "/app/multi-account-backtest");
    assert(observed_body == spec);

    const auto started_multi_account = client.start_multi_account_backtest(spec);
    assert(started_multi_account.ok());
    assert(observed_path == "/app/multi-account-backtest/start");
    assert(observed_body == spec);

    const auto started_multi_leg = client.start_multi_leg_backtest(spec);
    assert(started_multi_leg.ok());
    assert(observed_path == "/app/multi-leg-backtest/start");

    const auto multi_leg_experiment = client.run_multi_leg_experiment(multi_leg_experiment_spec);
    assert(multi_leg_experiment.ok());
    assert(observed_path == "/app/run-multi-leg-experiment");
    assert(observed_body == multi_leg_experiment_spec);

    const auto started_multi_leg_experiment = client.start_multi_leg_experiment(multi_leg_experiment_spec);
    assert(started_multi_leg_experiment.ok());
    assert(observed_path == "/app/run-multi-leg-experiment/start");

    const auto started = client.start_backtest(spec);
    assert(started.ok());
    assert(observed_method == "POST");
    assert(observed_path == "/app/backtest/start");
    assert(observed_body == spec);

    const auto started_depth = client.start_depth_backtest(spec);
    assert(started_depth.ok());
    assert(observed_path == "/app/depth-backtest/start");

    const auto started_depth_experiment = client.start_depth_experiment(spec);
    assert(started_depth_experiment.ok());
    assert(observed_path == "/app/run-depth-experiment/start");

    const auto started_experiment = client.start_experiment(spec);
    assert(started_experiment.ok());
    assert(observed_path == "/app/run-experiment/start");

    const auto status = client.run_status("run-1");
    assert(status.ok());
    assert(observed_method == "GET");
    assert(observed_path == "/app/runs/run-1");
    assert(qianxing::routes::fill_run_id(qianxing::routes::kRunEventsTemplate, "run-1") ==
           "/app/runs/run-1/events");
    assert(qianxing::routes::fill_run_id(qianxing::routes::kRunStatusTemplate,
                                         "../other?admin=true#fragment") ==
           "/app/runs/..%2Fother%3Fadmin%3Dtrue%23fragment");

    const auto run_events = client.run_events("run-1", 9);
    assert(run_events.ok());
    assert(observed_method == "GET");
    assert(observed_path == "/app/runs/run-1/events?after_seq=9");

    const auto cancelled = client.cancel_run("run-1");
    assert(cancelled.ok());
    assert(observed_method == "POST");
    assert(observed_path == "/app/runs/run-1/cancel");
    assert(observed_body == "{}");

    const auto depth_verification = client.verify_depth_run(spec);
    assert(depth_verification.ok());
    assert(observed_path == "/app/verify-depth");
    assert(observed_body == spec);

    const auto paper = client.submit_paper_command(spec);
    assert(paper.ok());
    assert(observed_path == "/app/paper/commands");
    assert(observed_body == spec);

    const auto live = client.submit_live_command(spec);
    assert(live.ok());
    assert(observed_path == "/app/live/commands");
    assert(observed_body == spec);

    const auto dataset = client.validate_dataset(spec);
    assert(dataset.ok());
    assert(observed_method == "POST");
    assert(observed_path == "/app/validate-dataset");
    assert(observed_body == spec);

    const auto backtest = client.run_backtest(sized_backtest_spec);
    assert(backtest.ok());
    assert(observed_path == "/app/backtest");
    assert(observed_body == sized_backtest_spec);
    assert(observed_body.find("\"target_volatility_bps\":250") != std::string::npos);
    assert(observed_body.find("\"account_id\":\"acct.cpp-sdk\"") != std::string::npos);

    const auto provider_backtest = client.run_provider_backtest_with_manifest(sized_backtest_spec);
    assert(provider_backtest.ok());
    assert(observed_path == "/app/provider-backtest");

    const auto provider_depth_backtest = client.run_provider_depth_backtest(sized_backtest_spec);
    assert(provider_depth_backtest.ok());
    assert(observed_path == "/app/provider-depth-backtest");
    assert(observed_body == sized_backtest_spec);

    const auto verified = client.verify_run(spec);
    assert(verified.ok());
    assert(observed_path == "/app/verify");
    assert(observed_body == spec);

    const auto compared = client.compare_runs(spec);
    assert(compared.ok());
    assert(observed_path == "/app/compare-runs");
    assert(observed_body == spec);

    const auto algo_plan = client.plan_algo_execution(spec);
    assert(algo_plan.ok());
    assert(observed_method == "POST");
    assert(observed_path == "/app/plan-algo-execution");
    assert(observed_body == spec);

    qianxing::AppClient unavailable({});
    assert(unavailable.run_experiment("{}").status == 0);
    assert(unavailable.verify_run("{}").status == 0);
}
