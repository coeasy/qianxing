cmake_minimum_required(VERSION 3.20)
project(qianxing_sdk_consumer LANGUAGES CXX)

find_package(QianxingStrategySDK 0.1 CONFIG REQUIRED)
add_executable(qianxing_sdk_consumer main.cpp)
target_link_libraries(qianxing_sdk_consumer PRIVATE Qianxing::StrategySDK)
