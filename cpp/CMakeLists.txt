cmake_minimum_required(VERSION 3.20)
project(qianxing_strategy_sdk LANGUAGES CXX)

add_executable(qianxing_ring_smoke tests/ring_smoke.cpp)
target_include_directories(qianxing_ring_smoke PRIVATE ${CMAKE_CURRENT_SOURCE_DIR}/include)
target_compile_features(qianxing_ring_smoke PRIVATE cxx_std_17)

add_library(qianxing_strategy_example SHARED examples/momentum_strategy.cpp)
target_include_directories(qianxing_strategy_example PRIVATE ${CMAKE_CURRENT_SOURCE_DIR}/include)
target_compile_features(qianxing_strategy_example PRIVATE cxx_std_17)

option(QX_BUILD_JSONL_EXAMPLE "Build the external JSONL strategy example" OFF)
if(QX_BUILD_JSONL_EXAMPLE)
    find_package(nlohmann_json CONFIG REQUIRED)
    add_executable(qianxing_strategy_jsonl examples/jsonl_strategy.cpp)
    target_link_libraries(qianxing_strategy_jsonl PRIVATE nlohmann_json::nlohmann_json)
    target_include_directories(qianxing_strategy_jsonl PRIVATE ${CMAKE_CURRENT_SOURCE_DIR}/include)
    target_compile_features(qianxing_strategy_jsonl PRIVATE cxx_std_17)
endif()
