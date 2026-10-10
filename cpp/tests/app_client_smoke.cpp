#include <qianxing_app.hpp>

#include <cassert>
#include <string>

int main() {
    std::string observed_path;
    std::string observed_body;
    qianxing::AppClient client([&](std::string_view method, std::string_view path,
                                  std::string_view body) {
        assert(method == "POST");
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

    const auto depth_verification = client.verify_depth_run(spec);
    assert(depth_verification.ok());
    assert(observed_path == "/app/verify-depth");
    assert(observed_body == spec);

    qianxing::AppClient unavailable({});
    assert(unavailable.run_experiment("{}").status == 0);
    assert(unavailable.verify_run("{}").status == 0);
}
