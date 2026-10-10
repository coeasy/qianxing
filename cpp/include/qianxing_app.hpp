#ifndef QIANXING_APP_HPP
#define QIANXING_APP_HPP

/*
 * Qianxing application API client v1.
 *
 * This header routes versioned JSON documents to the shared qx-app HTTP
 * contract. It does not implement trading or backtesting in C++; the caller
 * supplies an HTTP transport and remains responsible for TLS/authentication.
 */

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

class AppClient {
public:
    explicit AppClient(AppTransport transport) : transport_(std::move(transport)) {}

    AppResponse validate_dataset(std::string_view spec_json) const {
        return post("/app/validate-dataset", spec_json);
    }

    AppResponse run_backtest(std::string_view spec_json) const {
        return post("/app/backtest", spec_json);
    }

    AppResponse verify_run(std::string_view outcome_json) const {
        return post("/app/verify", outcome_json);
    }

    AppResponse compare_runs(std::string_view spec_json) const {
        return post("/app/compare-runs", spec_json);
    }

    AppResponse run_experiment(std::string_view spec_json) const {
        return post("/app/run-experiment", spec_json);
    }

    AppResponse run_depth_backtest(std::string_view spec_json) const {
        return post("/app/depth-backtest", spec_json);
    }

    AppResponse verify_depth_run(std::string_view outcome_json) const {
        return post("/app/verify-depth", outcome_json);
    }

private:
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
