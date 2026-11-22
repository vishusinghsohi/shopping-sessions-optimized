#pragma once

#include <array>
#include <cstdint>
#include <filesystem>
#include <optional>
#include <string_view>

namespace shopping {

using Integer = std::int64_t;

template<std::size_t N>
std::optional<std::array<std::string_view, N>> parse_csv(std::string_view row) {
    static_assert(N > 0);
    if (row.empty()) {
        return std::nullopt;
    }
    std::array<std::string_view, N> fields{};
    for (std::size_t i = 0; i < N; ++i) {
        const auto comma = row.find(',');
        if ((comma == row.npos) != (i + 1 == N)) {
            return std::nullopt;
        }
        auto field = row.substr(0, comma);
        if (!field.empty() && field.front() == '"') {
            if (field.size() < 2 || field.back() != '"') {
                return std::nullopt;
            }
            field.remove_prefix(1);
            field.remove_suffix(1);
        }
        if (field.find('"') != field.npos) {
            return std::nullopt;
        }
        fields[i] = field;
        if (comma != row.npos) {
            row.remove_prefix(comma + 1);
        }
    }
    return fields;
}

bool valid_utf8(std::string_view value);
std::optional<Integer> timestamp(std::string_view value);
std::optional<Integer> purchase_price(std::string_view value);
bool gap_exceeds(Integer newer, Integer older, Integer limit);
Integer utc_day(Integer time);
void run(const std::filesystem::path& data, const std::filesystem::path& reports);

} // namespace shopping
