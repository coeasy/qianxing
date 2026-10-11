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
#include <functional>
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
inline constexpr std::string_view kBacktestStart = "/app/backtest/start";
inline constexpr std::string_view kVerify = "/app/verify";
inline constexpr std::string_view kVerifyDepth = "/app/verify-depth";
inline constexpr std::string_view kCompareRuns = "/app/compare-runs";
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
inline constexpr std::string_view kPaperCommands = "/app/paper/commands";
inline constexpr std::string_view kLiveCommands = "/app/live/commands";
inline constexpr std::string_view kRunStatusTemplate = "/app/runs/{run_id}";
inline constexpr std::string_view kRunCancelTemplate = "/app/runs/{run_id}/cancel";
inline constexpr std::string_view kRunsPrefix = "/app/runs/";
inline constexpr std::string_view kRunCancelSuffix = "/cancel";

// The gate's read surface: every route template the client claims to call
// (without the prefix/suffix fragments used for concatenation).
inline constexpr std::array<std::string_view, 20> kDeclaredRoutes = {
    kValidateDataset,       kBacktest,
    kBacktestStart,         kVerify,
    kVerifyDepth,           kCompareRuns,
    kRunExperiment,         kRunExperimentStart,
    kDepthBacktest,         kDepthBacktestStart,
    kDepthExperiment,       kDepthExperimentStart,
    kMultiLegBacktest,      kMultiLegBacktestStart,
    kMultiLegExperiment,    kMultiLegExperimentStart,
    kPaperCommands,         kLiveCommands,
    kRunStatusTemplate,     kRunCancelTemplate,
};

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

    AppResponse start_backtest(std::string_view spec_json) const {
        return post(routes::kBacktestStart, spec_json);
    }

    AppResponse verify_run(std::string_view outcome_json) const {
        return post(routes::kVerify, outcome_json);
    }

    AppResponse compare_runs(std::string_view spec_json) const {
        return post(routes::kCompareRuns, spec_json);
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
        return std::string(routes::kRunsPrefix) + std::string(run_id);
    }

    static std::string run_cancel_path(std::string_view run_id) {
        return std::string(routes::kRunsPrefix) + std::string(run_id) +
               std::string(routes::kRunCancelSuffix);
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
