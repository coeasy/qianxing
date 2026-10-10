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

    const std::string spec = R"({"schema_version":1,"experiment_id":"smoke"})";
    const auto response = client.run_experiment(spec);
    assert(response.ok());
    assert(response.body == R"({"runs":[]})");
    assert(observed_path == "/app/run-experiment");

    const auto depth = client.run_depth_backtest(spec);
    assert(depth.ok());
    assert(observed_path == "/app/depth-backtest");
    assert(observed_body == spec);

    const auto started = client.start_backtest(spec);
    assert(started.ok());
    assert(observed_method == "POST");
    assert(observed_path == "/app/backtest/start");
    assert(observed_body == spec);

    const auto started_depth = client.start_depth_backtest(spec);
    assert(started_depth.ok());
    assert(observed_path == "/app/depth-backtest/start");

    const auto started_experiment = client.start_experiment(spec);
    assert(started_experiment.ok());
    assert(observed_path == "/app/run-experiment/start");

    const auto status = client.run_status("run-1");
    assert(status.ok());
    assert(observed_method == "GET");
    assert(observed_path == "/app/runs/run-1");

    const auto cancelled = client.cancel_run("run-1");
    assert(cancelled.ok());
    assert(observed_method == "POST");
    assert(observed_path == "/app/runs/run-1/cancel");
    assert(observed_body == "{}");

    const auto depth_verification = client.verify_depth_run(spec);
    assert(depth_verification.ok());
    assert(observed_path == "/app/verify-depth");
    assert(observed_body == spec);

    qianxing::AppClient unavailable({});
    assert(unavailable.run_experiment("{}").status == 0);
    assert(unavailable.verify_run("{}").status == 0);
}
