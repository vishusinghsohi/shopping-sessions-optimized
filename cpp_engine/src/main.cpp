#include "engine.hpp"

#include <chrono>
#include <cstdlib>
#include <exception>
#include <iostream>

#if defined(SHOPPING_NATIVE_PARQUET) && defined(__linux__)
#include <sys/resource.h>
#endif

int main(int argc, char** argv) {
#if defined(SHOPPING_NATIVE_PARQUET)
    if (argc != 1) {
        std::cerr << "Set DATA_DIR and OUTPUT_DIR to override /data and /output\n";
        return 2;
    }
#else
    if (argc != 2) {
        std::cerr << "Usage: cpp_engine DATA_DIR\n";
        return 2;
    }
#endif
    try {
#if defined(SHOPPING_NATIVE_PARQUET)
        const auto* data = std::getenv("DATA_DIR");
        const auto* output = std::getenv("OUTPUT_DIR");
        const auto started = std::chrono::steady_clock::now();
        shopping::run(data && *data ? data : "/data", output && *output ? output : "/output");
        const std::chrono::duration<double> elapsed = std::chrono::steady_clock::now() - started;
        std::cout << "C++ processing: " << std::fixed << elapsed.count() << " s\n";
#if defined(__linux__)
        struct rusage usage {};
        if (::getrusage(RUSAGE_SELF, &usage) == 0) {
            std::cout << "C++ peak RSS: " << static_cast<double>(usage.ru_maxrss) / 1024.0 << " MiB\n";
        }
#endif
#else
        shopping::run(argv[1], "out");
#endif
    } catch (const std::exception& error) {
        std::cerr << "Shopping Sessions: " << error.what() << '\n';
        return 1;
    }
}
