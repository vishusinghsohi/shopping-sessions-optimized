#include "engine.hpp"

#include <iostream>
#include <limits>
#include <stdexcept>

using namespace std::string_view_literals;
using namespace shopping;

void require(bool result, const char* description) {
    if (!result) {
        throw std::runtime_error(description);
    }
}

int main() {
    try {
        const auto row = parse_csv<3>("\" u\",\"\",1000\r"sv);
        require(row && (*row)[0] == " u" && (*row)[1].empty() && (*row)[2] == "1000\r", "CSV values");
        require(!parse_csv<2>("a,b,c"sv) && !parse_csv<2>("a"sv), "CSV column count");
        require(!parse_csv<2>("a,\"b"sv) && !parse_csv<2>("a,b\""sv), "CSV malformed quotes");
        require(!parse_csv<2>("\"a,b\",c"sv) && !parse_csv<2>(""sv), "CSV dialect");
        require(parse_csv<2>("a,"sv)->at(1).empty(), "CSV trailing null");
        const auto utc = timestamp("2026-10-03T23:58:10Z");
        require(utc && utc == timestamp("2026-10-03T19:58:10-04:00"), "UTC offset");
        require(utc == timestamp("2026-10-03T23:58:10-00:00"), "negative zero");
        require(timestamp("000001") == 1 && timestamp("0000") == 0, "leading zeros");
        require(timestamp("9223372036854775807") == std::numeric_limits<Integer>::max(), "largest timestamp");
        require(!timestamp("9223372036854775808"), "timestamp overflow");
        require(timestamp("2028-02-29T00:00:00Z").has_value(), "leap day");
        require(!timestamp("2027-02-29T00:00:00Z") && !timestamp("2026-10-03T23:58:10+24:00"), "calendar bounds");
        require(!timestamp("2026-10-03T23:58:10.000Z") && !timestamp("2026-10-03t23:58:10z"), "unsupported timestamps");
        require(!timestamp("２０２６") && !timestamp("-1") && !timestamp("1.2"), "ASCII timestamps");
        require(purchase_price("0250") == 250 && purchase_price("10000000") == 10'000'000, "price bounds");
        require(!purchase_price("10000001") && !purchase_price("0") && !purchase_price("+1"), "invalid price");
        require(!purchase_price("1000\r") && !purchase_price("٢٥٠"), "price dialect");
        require(valid_utf8("héllo") && !valid_utf8("\xed\xa0\x80"sv), "UTF-8 decoding");
        require(!valid_utf8("\xc0\xaf"sv) && !valid_utf8("\xf4\x90\x80\x80"sv), "UTF-8 bounds");
        require(!gap_exceeds(1'800'000, 0, 1'800'000) && gap_exceeds(1'800'001, 0, 1'800'000), "session boundary");
        require(gap_exceeds(std::numeric_limits<Integer>::max(), std::numeric_limits<Integer>::min(), 1), "gap overflow");
        require(!gap_exceeds(std::numeric_limits<Integer>::max(), std::numeric_limits<Integer>::max(), 1), "gap saturation");
        require(utc_day(-1) == -1 && utc_day(0) == 0, "UTC floor");
    } catch (const std::exception& error) {
        std::cerr << error.what() << '\n';
        return 1;
    }
}
