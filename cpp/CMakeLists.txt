cmake_minimum_required(VERSION 3.20)
project(qianxing_strategy_sdk LANGUAGES CXX)

if(POLICY CMP0135)
    cmake_policy(SET CMP0135 NEW)
endif()

add_executable(qianxing_ring_smoke tests/ring_smoke.cpp)
target_include_directories(qianxing_ring_smoke PRIVATE ${CMAKE_CURRENT_SOURCE_DIR}/include)
target_compile_features(qianxing_ring_smoke PRIVATE cxx_std_17)

add_library(qianxing_strategy_example SHARED examples/momentum_strategy.cpp)
target_include_directories(qianxing_strategy_example PRIVATE ${CMAKE_CURRENT_SOURCE_DIR}/include)
target_compile_features(qianxing_strategy_example PRIVATE cxx_std_17)

option(QX_BUILD_JSONL_EXAMPLE "Build the external JSONL strategy example" OFF)
if(QX_BUILD_JSONL_EXAMPLE)
    find_package(nlohmann_json CONFIG QUIET)
    if(NOT TARGET nlohmann_json::nlohmann_json)
        # 只有 Linux 发行版提供现成的 CONFIG 包；Windows/macOS 上退化为按 URL 拉取
        # 同一版本的 header-only 目标，让三平台跑同一条 JSONL + 共享内存环验收。
        include(FetchContent)
        set(JSON_BuildTests OFF CACHE INTERNAL "")
        set(JSON_Install OFF CACHE INTERNAL "")
        FetchContent_Declare(nlohmann_json
            URL https://github.com/nlohmann/json/releases/download/v3.11.3/json.tar.xz)
        FetchContent_MakeAvailable(nlohmann_json)
    endif()
    add_executable(qianxing_strategy_jsonl examples/jsonl_strategy.cpp)
    target_link_libraries(qianxing_strategy_jsonl PRIVATE nlohmann_json::nlohmann_json)
    target_include_directories(qianxing_strategy_jsonl PRIVATE ${CMAKE_CURRENT_SOURCE_DIR}/include)
    target_compile_features(qianxing_strategy_jsonl PRIVATE cxx_std_17)
endif()
