#pragma once

#include <array>
#include <bit>
#include <cstdint>
#include <filesystem>
#include <optional>
#include <string_view>

#if defined(__AVX2__)
#include <immintrin.h>
#endif

namespace shopping {

using Integer = std::int64_t;

namespace detail {

template<std::size_t N>
std::optional<std::array<std::string_view, N>> parse_csv_scalar(std::string_view row) {
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

#if defined(__AVX2__)
template<std::size_t N>
std::optional<std::array<std::string_view, N>> parse_csv_avx2(std::string_view row) {
    constexpr std::size_t width = 32;
    const auto comma = _mm256_set1_epi8(',');
    const auto quote = _mm256_set1_epi8('"');
    std::array<std::string_view, N> fields;
    std::size_t begin = 0;
    std::size_t column = 0;
    std::size_t offset = 0;

    const auto separator = [&](std::size_t position) {
        if (column + 1 == N) {
            return false;
        }
        fields[column++] = row.substr(begin, position - begin);
        begin = position + 1;
        return true;
    };

    for (; row.size() - offset >= width; offset += width) {
        const auto bytes = _mm256_loadu_si256(
            reinterpret_cast<const __m256i*>(row.data() + offset));
        if (_mm256_movemask_epi8(_mm256_cmpeq_epi8(bytes, quote)) != 0) {
            return parse_csv_scalar<N>(row);
        }
        auto delimiters = static_cast<std::uint32_t>(
            _mm256_movemask_epi8(_mm256_cmpeq_epi8(bytes, comma)));
        while (delimiters != 0) {
            if (!separator(offset + static_cast<std::size_t>(std::countr_zero(delimiters)))) {
                return std::nullopt;
            }
            delimiters &= delimiters - 1;
        }
    }
    for (; offset < row.size(); ++offset) {
        if (row[offset] == '"') {
            return parse_csv_scalar<N>(row);
        }
        if (row[offset] == ',' && !separator(offset)) {
            return std::nullopt;
        }
    }
    if (column + 1 != N) {
        return std::nullopt;
    }
    fields[column] = row.substr(begin);
    return fields;
}
#endif

} // namespace detail

template<std::size_t N>
std::optional<std::array<std::string_view, N>> parse_csv(std::string_view row) {
#if defined(__AVX2__)
    if constexpr (N > 2) {
        if (row.size() >= 32) {
            return detail::parse_csv_avx2<N>(row);
        }
    }
#endif
    return detail::parse_csv_scalar<N>(row);
}

bool valid_utf8(std::string_view value);
std::optional<Integer> timestamp(std::string_view value);
std::optional<Integer> purchase_price(std::string_view value);
bool gap_exceeds(Integer newer, Integer older, Integer limit);
Integer utc_day(Integer time);
void run(const std::filesystem::path& data, const std::filesystem::path& reports);

} // namespace shopping
