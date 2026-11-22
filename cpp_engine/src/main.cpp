#include "engine.hpp"

#include <exception>
#include <iostream>

int main(int argc, char** argv) {
    if (argc != 2) {
        std::cerr << "Usage: cpp_engine DATA_DIR\n";
        return 2;
    }
    try {
        shopping::run(argv[1], "out");
    } catch (const std::exception& error) {
        std::cerr << "Shopping Sessions: " << error.what() << '\n';
        return 1;
    }
}
