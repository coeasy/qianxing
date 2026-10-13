#ifndef QIANXING_APP_HPP
#define QIANXING_APP_HPP

/*
 * Qianxing application API client v1.
 *
 * This header routes versioned JSON documents to the shared qx-app HTTP
 * contract. It does not implement trading or backtesting in C++; the caller
 * supplies an HTTP transport and remains responsible for TLS/authentication.
 */

#include <array>
#include <cstdint>
#include <functional>
#include <optional>
#include <string>
#include <string_view>
#include <utility>

namespace qianxing {

struct AppResponse {
    int status = 0;
    std::string body;

    bool ok() const noexcept { return status >= 200 && status < 300; }
};

using AppTransport = std::function<AppResponse(
    std::string_view method, std::string_view path, std::string_view json_body)>;

// This is the C++ client's single declaration surface for the qx-api route
// table. The `client_route_wiring_check` gate reads `kDeclaredRoutes` and
// matches every path against the qx-api dispatch table, so a path the server
// does not serve fails the gate instead of surfacing as a runtime 404.
// `{run_id}` is a path template placeholder filled in by run_path /
// run_cancel_path. Keep this header ASCII-only: it is compiled under the
// Windows code page without a `/utf-8` flag.
namespace routes {

inline constexpr std::string_view kValidateDataset = "/app/validate-dataset";
inline constexpr std::string_view kBacktest = "/app/backtest";
inline constexpr std::string_view kProviderBacktest = "/app/provider-backtest";
inline constexpr std::string_view kProviderDepthBacktest = "/app/provider-depth-backtest";
inline constexpr std::string_view kMultiAccountBacktest = "/app/multi-account-backtest";
inline constexpr std::string_view kMultiAccountBacktestStart = "/app/multi-account-backtest/start";
inline constexpr std::string_view kMultiMarketBacktest = "/app/multi-market-backtest";
inline constexpr std::string_view kMultiMarketBacktestStart = "/app/multi-market-backtest/start";
inline constexpr std::string_view kBacktestStart = "/app/backtest/start";
inline constexpr std::string_view kVerify = "/app/verify";
inline constexpr std::string_view kVerifyDepth = "/app/verify-depth";
inline constexpr std::string_view kCompareRuns = "/app/compare-runs";
inline constexpr std::string_view kPlanRiskParity = "/app/plan-risk-parity";
inline constexpr std::string_view kPlanAlgoExecution = "/app/plan-algo-execution";
inline constexpr std::string_view kPlanCapitalAllocation = "/app/plan-capital-allocation";
inline constexpr std::string_view kPaperAlgoStart = "/app/paper/algo-executions/start";
inline constexpr std::string_view kPaperAlgoGet = "/app/paper/algo-executions/get";
inline constexpr std::string_view kPaperAlgoCancel = "/app/paper/algo-executions/cancel";
inline constexpr std::string_view kPaperAlgoList = "/app/paper/algo-executions/list";
inline constexpr std::string_view kRunExperiment = "/app/run-experiment";
inline constexpr std::string_view kRunExperimentStart = "/app/run-experiment/start";
inline constexpr std::string_view kDepthBacktest = "/app/depth-backtest";
inline constexpr std::string_view kDepthBacktestStart = "/app/depth-backtest/start";
inline constexpr std::string_view kDepthExperiment = "/app/run-depth-experiment";
inline constexpr std::string_view kDepthExperimentStart = "/app/run-depth-experiment/start";
inline constexpr std::string_view kMultiLegBacktest = "/app/multi-leg-backtest";
inline constexpr std::string_view kMultiLegBacktestStart = "/app/multi-leg-backtest/start";
inline constexpr std::string_view kMultiLegExperiment = "/app/run-multi-leg-experiment";
inline constexpr std::string_view kMultiLegExperimentStart =
    "/app/run-multi-leg-experiment/start";
inline constexpr std::string_view kMultiMarketExperiment = "/app/run-multi-market-experiment";
inline constexpr std::string_view kMultiMarketExperimentStart =
    "/app/run-multi-market-experiment/start";
inline constexpr std::string_view kPaperCommands = "/app/paper/commands";
inline constexpr std::string_view kLiveCommands = "/app/live/commands";
inline constexpr std::string_view kRunStatusTemplate = "/app/runs/{run_id}";
inline constexpr std::string_view kRunEventsTemplate = "/app/runs/{run_id}/events";
inline constexpr std::string_view kRunCancelTemplate = "/app/runs/{run_id}/cancel";
inline constexpr std::string_view kLiveEvents = "/events/live";

// The gate's read surface: every route template the client claims to call.
// Every name below must also be consumed by a method on AppClient -- a route
// that is only listed here is decoration, and the gate rejects it.
inline constexpr std::array<std::string_view, 37> kDeclaredRoutes = {
    kValidateDataset,       kBacktest, kProviderBacktest, kProviderDepthBacktest,
    kMultiAccountBacktest,  kMultiAccountBacktestStart,
    kMultiMarketBacktest,   kMultiMarketBacktestStart,
    kMultiMarketExperiment, kMultiMarketExperimentStart,
    kBacktestStart,         kVerify,
    kVerifyDepth,           kCompareRuns,
    kPlanRiskParity,
    kPlanAlgoExecution,
    kPlanCapitalAllocation,
    kPaperAlgoStart,        kPaperAlgoGet,
    kPaperAlgoCancel,       kPaperAlgoList,
    kRunExperiment,         kRunExperimentStart,
    kDepthBacktest,         kDepthBacktestStart,
    kDepthExperiment,       kDepthExperimentStart,
    kMultiLegBacktest,      kMultiLegBacktestStart,
    kMultiLegExperiment,    kMultiLegExperimentStart,
    kPaperCommands,         kLiveCommands,
    kRunStatusTemplate,     kRunEventsTemplate, kRunCancelTemplate,
    kLiveEvents,
};

// Fill the `{run_id}` placeholder of a declared template. The path the client
// actually requests and the template the gate compares against the qx-api
// dispatch table are therefore the same string, so they cannot drift apart.
inline std::string fill_run_id(std::string_view route_template, std::string_view run_id) {
    constexpr std::string_view placeholder = "{run_id}";
    const auto at = route_template.find(placeholder);
    if (at == std::string_view::npos) {
        return std::string(route_template);
    }
    std::string path(route_template.substr(0, at));
    // Treat the caller-supplied identifier as one path segment. Valid run IDs
    // (ASCII alphanumeric plus `-_.`) are unchanged; malformed values cannot
    // inject a slash, query, or fragment into the request path.
    constexpr char hex[] = "0123456789ABCDEF";
    for (const unsigned char byte : run_id) {
        const bool unreserved = (byte >= 'a' && byte <= 'z') ||
                                (byte >= 'A' && byte <= 'Z') ||
                                (byte >= '0' && byte <= '9') || byte == '-' ||
                                byte == '_' || byte == '.' || byte == '~';
        if (unreserved) {
            path.push_back(static_cast<char>(byte));
        } else {
            path.push_back('%');
            path.push_back(hex[byte >> 4]);
            path.push_back(hex[byte & 0x0F]);
        }
    }
    path.append(route_template.substr(at + placeholder.size()));
    return path;
}

}  // namespace routes

class AppClient {
public:
    explicit AppClient(AppTransport transport) : transport_(std::move(transport)) {}

    AppResponse validate_dataset(std::string_view spec_json) const {
        return post(routes::kValidateDataset, spec_json);
    }

    AppResponse run_backtest(std::string_view spec_json) const {
        return post(routes::kBacktest, spec_json);
    }

    AppResponse run_provider_backtest_with_manifest(std::string_view spec_json) const {
        return post(routes::kProviderBacktest, spec_json);
    }

    AppResponse run_provider_depth_backtest(std::string_view spec_json) const {
        return post(routes::kProviderDepthBacktest, spec_json);
    }

    AppResponse run_multi_account_backtest(std::string_view spec_json) const {
        return post(routes::kMultiAccountBacktest, spec_json);
    }

    AppResponse start_multi_account_backtest(std::string_view spec_json) const {
        return post(routes::kMultiAccountBacktestStart, spec_json);
    }

    AppResponse run_multi_market_backtest(std::string_view spec_json) const {
        return post(routes::kMultiMarketBacktest, spec_json);
    }

    AppResponse start_multi_market_backtest(std::string_view spec_json) const {
        return post(routes::kMultiMarketBacktestStart, spec_json);
    }

    AppResponse run_multi_market_experiment(std::string_view spec_json) const {
        return post(routes::kMultiMarketExperiment, spec_json);
    }

    AppResponse start_multi_market_experiment(std::string_view spec_json) const {
        return post(routes::kMultiMarketExperimentStart, spec_json);
    }

    AppResponse start_backtest(std::string_view spec_json) const {
        return post(routes::kBacktestStart, spec_json);
    }

    AppResponse verify_run(std::string_view outcome_json) const {
        return post(routes::kVerify, outcome_json);
    }

    AppResponse compare_runs(std::string_view spec_json) const {
        return post(routes::kCompareRuns, spec_json);
    }

    AppResponse plan_risk_parity(std::string_view spec_json) const {
        return post(routes::kPlanRiskParity, spec_json);
    }

    AppResponse plan_algo_execution(std::string_view spec_json) const {
        return post(routes::kPlanAlgoExecution, spec_json);
    }

    AppResponse plan_capital_allocation(std::string_view spec_json) const {
        return post(routes::kPlanCapitalAllocation, spec_json);
    }

    AppResponse start_paper_algo_execution(std::string_view request_json) const {
        return post(routes::kPaperAlgoStart, request_json);
    }

    AppResponse get_paper_algo_execution(std::string_view request_json) const {
        return post(routes::kPaperAlgoGet, request_json);
    }

    AppResponse cancel_paper_algo_execution(std::string_view request_json) const {
        return post(routes::kPaperAlgoCancel, request_json);
    }

    AppResponse list_paper_algo_executions(std::string_view request_json) const {
        return post(routes::kPaperAlgoList, request_json);
    }

    AppResponse run_experiment(std::string_view spec_json) const {
        return post(routes::kRunExperiment, spec_json);
    }

    AppResponse run_depth_backtest(std::string_view spec_json) const {
        return post(routes::kDepthBacktest, spec_json);
    }

    AppResponse start_depth_backtest(std::string_view spec_json) const {
        return post(routes::kDepthBacktestStart, spec_json);
    }

    AppResponse run_depth_experiment(std::string_view spec_json) const {
        return post(routes::kDepthExperiment, spec_json);
    }

    AppResponse start_depth_experiment(std::string_view spec_json) const {
        return post(routes::kDepthExperimentStart, spec_json);
    }

    AppResponse run_multi_leg_backtest(std::string_view spec_json) const {
        return post(routes::kMultiLegBacktest, spec_json);
    }

    AppResponse start_multi_leg_backtest(std::string_view spec_json) const {
        return post(routes::kMultiLegBacktestStart, spec_json);
    }

    AppResponse run_multi_leg_experiment(std::string_view spec_json) const {
        return post(routes::kMultiLegExperiment, spec_json);
    }

    AppResponse start_multi_leg_experiment(std::string_view spec_json) const {
        return post(routes::kMultiLegExperimentStart, spec_json);
    }

    AppResponse start_experiment(std::string_view spec_json) const {
        return post(routes::kRunExperimentStart, spec_json);
    }

    AppResponse run_status(std::string_view run_id) const {
        return get(run_path(run_id));
    }

    AppResponse run_events(std::string_view run_id,
                           std::uint64_t after_seq = 0) const {
        return get(routes::fill_run_id(routes::kRunEventsTemplate, run_id) +
                   "?after_seq=" + std::to_string(after_seq));
    }

    AppResponse get_live_events(std::optional<std::uint64_t> after_seq = std::nullopt) const {
        if (!after_seq) {
            return get(routes::kLiveEvents);
        }
        const auto path = std::string(routes::kLiveEvents) + "?after=" +
                          std::to_string(*after_seq);
        return get(path);
    }

    AppResponse cancel_run(std::string_view run_id) const {
        return post(run_cancel_path(run_id), "{}");
    }

    AppResponse verify_depth_run(std::string_view outcome_json) const {
        return post(routes::kVerifyDepth, outcome_json);
    }

    // The body is a versioned TradeControlSpec. The Rust API authenticates the
    // operator, validates the command in qx-app, then uses its configured workers.
    AppResponse submit_paper_command(std::string_view spec_json) const {
        return post(routes::kPaperCommands, spec_json);
    }

    AppResponse submit_live_command(std::string_view spec_json) const {
        return post(routes::kLiveCommands, spec_json);
    }

private:
    static std::string run_path(std::string_view run_id) {
        return routes::fill_run_id(routes::kRunStatusTemplate, run_id);
    }

    static std::string run_cancel_path(std::string_view run_id) {
        return routes::fill_run_id(routes::kRunCancelTemplate, run_id);
    }

    AppResponse get(std::string_view path) const {
        if (!transport_) {
            return {0, "Qianxing AppClient transport is not configured"};
        }
        return transport_("GET", path, {});
    }

    AppResponse post(std::string_view path, std::string_view body) const {
        if (!transport_) {
            return {0, "Qianxing AppClient transport is not configured"};
        }
        return transport_("POST", path, body);
    }

    AppTransport transport_;
};

}  // namespace qianxing

#endif  // QIANXING_APP_HPP
