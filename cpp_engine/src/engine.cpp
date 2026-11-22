#include "engine.hpp"

#include <algorithm>
#include <bit>
#include <cerrno>
#include <charconv>
#include <chrono>
#include <cstdio>
#include <cstdlib>
#include <deque>
#include <exception>
#include <fstream>
#include <iostream>
#include <limits>
#include <memory>
#include <stdexcept>
#include <string>
#include <system_error>
#include <thread>
#include <tuple>
#include <unordered_map>
#include <vector>

#include <nlohmann/json.hpp>

#if defined(__linux__)
#include <fcntl.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <unistd.h>
#endif

namespace shopping {
namespace {

using Path = std::filesystem::path;
using Key = std::uint32_t;
using Clock = std::chrono::steady_clock;
using View = std::string_view;
constexpr Integer day_ms = 86'400'000;
constexpr Integer attribution_ms = 7 * day_ms;
constexpr Integer session_ms = 1'800'000;
constexpr Integer maximum = std::numeric_limits<Integer>::max();

std::optional<Integer> decimal(View value, Integer limit) {
    if (value.empty() || value.front() < '0' || value.front() > '9') {
        return std::nullopt;
    }
    Integer result = 0;
    const auto end = value.data() + value.size();
    const auto parsed = std::from_chars(value.data(), end, result);
    if (parsed.ec != std::errc{} || parsed.ptr != end || result > limit) {
        return std::nullopt;
    }
    return result;
}

std::optional<int> fixed_digits(View text) {
    int result = 0;
    for (const unsigned char byte : text) {
        if (byte < '0' || byte > '9') {
            return std::nullopt;
        }
        result = result * 10 + byte - '0';
    }
    return result;
}

void add(Integer& total, Integer value) {
    if (value < 0 || total > maximum - value) {
        throw std::overflow_error("BIGINT aggregation overflow");
    }
    total += value;
}

double seconds(Clock::time_point start) {
    return std::chrono::duration<double>(Clock::now() - start).count();
}

void profile(View stage, Clock::time_point start) {
    std::cout << "[INNER] " << stage << ' ' << std::fixed << seconds(start) << '\n';
}

class Partition {
public:
    explicit Partition(std::string bytes) : owned_(std::move(bytes)) {}

    View view() const {
#if defined(__linux__)
        if (mapping_) {
            return {static_cast<const char*>(mapping_.get()), mapping_.get_deleter().size};
        }
#endif
        return owned_;
    }

#if defined(__linux__)
    static Partition map(const Path& path) {
        struct Descriptor {
            int value;
            ~Descriptor() { if (value >= 0) ::close(value); }
        };
        const Descriptor fd{::open(path.c_str(), O_RDONLY | O_CLOEXEC)};
        if (fd.value < 0) {
            throw std::system_error(errno, std::generic_category(), "open " + path.string());
        }
        struct stat status {};
        if (::fstat(fd.value, &status) != 0) {
            throw std::system_error(errno, std::generic_category(), "stat " + path.string());
        }
        if (!S_ISREG(status.st_mode) || status.st_size < 0) {
            throw std::runtime_error("invalid CSV partition " + path.string());
        }
        if (status.st_size == 0) {
            return Partition(std::string{});
        }
        const auto size = static_cast<std::size_t>(status.st_size);
        void* address = ::mmap(nullptr, size, PROT_READ, MAP_PRIVATE, fd.value, 0);
        if (address == MAP_FAILED) {
            throw std::system_error(errno, std::generic_category(), "mmap " + path.string());
        }
        Partition result(std::string{});
        result.mapping_ = Mapping(address, Unmap{size});
        return result;
    }
#endif

private:
    std::string owned_;
#if defined(__linux__)
    struct Unmap {
        std::size_t size;
        void operator()(void* address) const { ::munmap(address, size); }
    };
    using Mapping = std::unique_ptr<void, Unmap>;
    Mapping mapping_{nullptr, Unmap{0}};
#endif
};

struct Input {
    std::uintmax_t bytes = 0;
    double read_seconds = 0;
    double map_seconds = 0;
    bool map_partitions = false;

    Input() {
#if defined(__linux__)
        const auto* mode = std::getenv("SESSION_MMAP");
        map_partitions = mode && View(mode) == "1";
#endif
    }

    std::string read(const Path& path) {
        const auto start = Clock::now();
        std::ifstream stream(path, std::ios::binary | std::ios::ate);
        const auto end = stream.tellg();
        if (end < 0 || end > std::streamoff(std::numeric_limits<std::streamsize>::max())) {
            throw std::runtime_error("cannot open or size " + path.string());
        }
        std::string data(static_cast<std::size_t>(end), '\0');
        stream.seekg(0);
        if (!stream.read(data.data(), static_cast<std::streamsize>(data.size()))) {
            throw std::runtime_error("cannot read " + path.string());
        }
        bytes += data.size();
        read_seconds += seconds(start);
        return data;
    }

    Partition partition(const Path& path) {
#if defined(__linux__)
        if (map_partitions) {
            const auto start = Clock::now();
            auto file = Partition::map(path);
            bytes += file.view().size();
            map_seconds += seconds(start);
            return file;
        }
#endif
        return Partition(read(path));
    }
};

std::vector<Path> partitions(const Path& directory) {
    std::vector<Path> paths;
    for (const auto& entry : std::filesystem::directory_iterator(directory)) {
        if (entry.path().extension() == ".csv") {
            paths.push_back(entry.path());
        }
    }
    std::ranges::sort(paths);
    return paths;
}

View data_rows(View file) {
    const auto header = file.find('\n');
    return header == file.npos ? View{} : file.substr(header + 1);
}

template<class Visitor>
void visit_rows(View file, Visitor visit) {
    while (!file.empty()) {
        const auto newline = file.find('\n');
        if (newline == file.npos) {
            visit(file);
            return;
        }
        visit(file.substr(0, newline));
        file.remove_prefix(newline + 1);
    }
}

template<class Visitor>
void rows(View file, Visitor visit) {
    visit_rows(data_rows(file), visit);
}

class Names {
public:
    Names() = default;
    Names(const Names&) = delete;
    Names& operator=(const Names&) = delete;

    std::optional<Key> find(View name) const {
        if (const auto entry = ids_.find(name); entry != ids_.end()) {
            return entry->second;
        }
        return std::nullopt;
    }

    Key intern(View name) {
        if (const auto key = find(name)) {
            return *key;
        }
        if (names_.size() >= std::numeric_limits<Key>::max()) {
            throw std::overflow_error("too many distinct identifiers");
        }
        const auto key = static_cast<Key>(names_.size());
        names_.emplace_back(name);
        ids_.emplace(names_.back(), key);
        return key;
    }

    View name(Key key) const { return names_[key]; }
    std::size_t size() const { return names_.size(); }

private:
    // deque insertion keeps the strings and all map views stable.
    std::deque<std::string> names_;
    std::unordered_map<View, Key> ids_;
};

class Identities {
public:
    void link(View a, View b) {
        Key left = root(intern(a));
        Key right = root(intern(b));
        if (left == right) {
            return;
        }
        if (components_[left].size < components_[right].size) {
            std::swap(left, right);
        }
        auto& keep = components_[left];
        auto& merge = components_[right];
        keep.size += merge.size;
        merge.parent = left;
        if (names_.name(merge.minimum) < names_.name(keep.minimum)) {
            keep.minimum = merge.minimum;
        }
    }

    Key canonical(View user) { return components_[root(intern(user))].minimum; }
    View name(Key key) const { return names_.name(key); }

private:
    struct Component {
        Key parent;
        Key minimum;
        Key size;
    };
    Names names_;
    std::vector<Component> components_;

    Key intern(View name) {
        const Key key = names_.intern(name);
        if (key == components_.size()) {
            components_.push_back({key, key, 1});
        }
        return key;
    }

    Key root(Key key) {
        while (key != components_[key].parent) {
            auto& node = components_[key];
            node.parent = components_[node.parent].parent;
            key = node.parent;
        }
        return key;
    }
};

struct Catalog {
    Names ids;
    Names labels;
    std::vector<Key> by_id;

    Key insert(View id, View label) {
        const Key key = ids.intern(id);
        if (key == by_id.size()) {
            by_id.push_back(labels.intern(label));
        } else {
            by_id[key] = labels.intern(label);
        }
        return key;
    }

    View label(Key id) const { return labels.name(by_id[id]); }
};

void load_catalog(Input& input, const Path& path, Catalog& result) {
    const auto file = input.read(path);
    rows(file, [&](View line) {
        const auto fields = parse_csv<2>(line);
        if (!fields || !valid_utf8((*fields)[0]) || !valid_utf8((*fields)[1])) {
            throw std::runtime_error("malformed lookup table " + path.string());
        }
        result.insert((*fields)[0], (*fields)[1]);
    });
}

struct Window {
    Integer start;
    Integer end;
};

Window load_window(Input& input, const Path& data) {
    const auto manifest = nlohmann::json::parse(input.read(data / "manifest.json"));
    const auto start = timestamp(manifest.at("window_start").get<std::string>());
    const auto end = timestamp(manifest.at("window_end").get<std::string>());
    if (!start || !end || *start >= *end) {
        throw std::runtime_error("invalid manifest window");
    }
    return {*start, *end};
}

struct Event {
    Integer time;
    Integer revenue;
    Key user;
    Key category;
};

struct Winner {
    View line;
    Integer ingest;
};

struct ValidEvent {
    Integer time;
    Integer revenue;
    View user;
    Key category;
};

std::optional<ValidEvent> validate_event(const Winner& winner, Window window,
                                         const Catalog& products, Key unknown) {
    const auto fields = *parse_csv<7>(winner.line);
    const auto [event_id, user, event_time, ingest_time, kind, product, price] = fields;
    const auto time = timestamp(event_time);
    if (user.empty() || !valid_utf8(user) || product.empty() || !valid_utf8(product) ||
        !time || *time < window.start || *time >= window.end ||
        gap_exceeds(winner.ingest, *time, day_ms)) {
        return std::nullopt;
    }
    if (kind == "view" || kind == "cart") {
        return ValidEvent{*time, 0, user, unknown};
    }
    if (kind != "purchase") {
        return std::nullopt;
    }
    const auto cents = purchase_price(price);
    if (!cents) {
        return std::nullopt;
    }
    const auto product_id = products.ids.find(product);
    return ValidEvent{*time, *cents, user, product_id ? products.by_id[*product_id] : unknown};
}

View routing_id(View row) {
    auto id = row.substr(0, row.find(','));
    if (id.size() >= 2 && id.front() == '"' && id.back() == '"') {
        id.remove_prefix(1);
        id.remove_suffix(1);
    }
    return id;
}

std::size_t worker_budget() {
    const auto available = std::max(1U, std::thread::hardware_concurrency());
    unsigned limit = std::min(8U, available);
    if (const auto* setting = std::getenv("NCPU")) {
        const View text(setting);
        unsigned requested = 0;
        const auto [end, error] = std::from_chars(text.data(), text.data() + text.size(), requested);
        if (error == std::errc{} && end == text.data() + text.size() && requested > 0) {
            limit = std::min(limit, requested);
        }
    }
    return limit;
}

template<class Task>
void run_workers(std::size_t count, Task task) {
    std::vector<std::exception_ptr> failures(count);
    {
        std::vector<std::jthread> threads;
        threads.reserve(count - 1);
        const auto run = [&](std::size_t index) {
            try {
                task(index);
            } catch (...) {
                failures[index] = std::current_exception();
            }
        };
        for (std::size_t i = 1; i < count; ++i) {
            threads.emplace_back(run, i);
        }
        run(0);
    }
    for (const auto& failure : failures) {
        if (failure) {
            std::rethrow_exception(failure);
        }
    }
}

using RowBatches = std::vector<std::vector<std::vector<View>>>;

RowBatches route_events(const std::vector<Partition>& storage, std::size_t workers) {
    std::size_t bytes = 0;
    std::vector<View> bodies;
    bodies.reserve(storage.size());
    for (const auto& file : storage) {
        const auto body = data_rows(file.view());
        bodies.push_back(body);
        bytes += body.size();
    }

    // Readers own contiguous ranges in file order. Split large files at a
    // newline to balance work without reordering equal-ingest duplicates.
    std::vector<std::vector<View>> ranges(workers);
    const auto target = bytes / workers + (bytes % workers != 0);
    std::size_t reader = 0;
    auto remaining = target;
    for (auto body : bodies) {
        while (!body.empty()) {
            auto size = body.size();
            if (reader + 1 < workers && size > remaining) {
                const auto newline = body.find('\n', remaining - 1);
                if (newline != body.npos) {
                    size = newline + 1;
                }
            }
            ranges[reader].push_back(body.substr(0, size));
            body.remove_prefix(size);
            if (reader + 1 < workers) {
                if (size >= remaining) {
                    ++reader;
                    remaining = target;
                } else {
                    remaining -= size;
                }
            }
        }
    }

    RowBatches routed(workers);
    run_workers(workers, [&](std::size_t index) {
        auto& batches = routed[index];
        batches.resize(workers);
        for (const auto range : ranges[index]) {
            visit_rows(range, [&](View line) {
                const auto shard = std::hash<View>{}(routing_id(line)) % workers;
                batches[shard].push_back(line);
            });
        }
    });
    return routed;
}

struct IndexedWinner {
    View id;
    Winner record;
};

std::vector<IndexedWinner> choose_winners(RowBatches& routed, std::size_t shard) {
    std::size_t row_count = 0;
    for (const auto& batches : routed) {
        row_count += batches[shard].size();
    }
    if (row_count >= std::numeric_limits<Key>::max()) {
        throw std::overflow_error("event shard index overflow");
    }
    // The physical input row count bounds the number of distinct keys. Keep
    // buckets below 75% occupancy. Index zero denotes an empty bucket.
    const auto bucket_count = std::bit_ceil(row_count + row_count / 3 + 2);
    std::vector<Key> buckets(bucket_count, 0);
    std::vector<IndexedWinner> winners;
    winners.reserve(row_count);
    const auto consider = [&](View row) {
        const auto fields = parse_csv<7>(row);
        if (!fields || (*fields)[0].empty()) {
            return;
        }
        const auto ingest = timestamp((*fields)[3]);
        if (!ingest) {
            return;
        }
        const auto id = (*fields)[0];
        auto slot = std::hash<View>{}(id) & (bucket_count - 1);
        while (buckets[slot] != 0 && winners[buckets[slot] - 1].id != id) {
            slot = (slot + 1) & (bucket_count - 1);
        }
        if (buckets[slot] == 0) {
            winners.push_back({id, {row, *ingest}});
            buckets[slot] = static_cast<Key>(winners.size());
        } else {
            auto& current = winners[buckets[slot] - 1].record;
            if (*ingest < current.ingest) {
                current = {row, *ingest};
            }
        }
    };
    for (auto& batches : routed) {
        for (const auto row : batches[shard]) {
            consider(row);
        }
        std::vector<View>().swap(batches[shard]);
    }
    return winners;
}

struct EventShard {
    std::vector<View> users;
    std::vector<Event> events;
    double dedup_seconds = 0;
    double validate_seconds = 0;
};

EventShard load_shard(RowBatches& routed, std::size_t index, Window window,
                      const Catalog& products, Key unknown) {
    EventShard shard;
    auto start = Clock::now();
    const auto winners = choose_winners(routed, index);
    shard.dedup_seconds = seconds(start);

    start = Clock::now();
    std::unordered_map<View, Key> users;
    shard.events.reserve(winners.size());
    for (const auto& winner : winners) {
        if (const auto valid = validate_event(winner.record, window, products, unknown)) {
            const auto [key, inserted] = users.try_emplace(valid->user, static_cast<Key>(shard.users.size()));
            if (inserted) {
                shard.users.push_back(valid->user);
            }
            shard.events.push_back({valid->time, valid->revenue, key->second, valid->category});
        }
    }
    shard.validate_seconds = seconds(start);
    return shard;
}

std::vector<Event> load_events(Input& input, const Path& data, Window window, Identities& ids,
                               const Catalog& products, Key unknown) {
    const auto files = partitions(data / "events");
    std::vector<Partition> storage;
    storage.reserve(files.size());
    auto start = Clock::now();
    for (const auto& path : files) {
        storage.push_back(input.partition(path));
    }
    profile(input.map_partitions ? "event_map_seconds" : "event_read_seconds", start);

    // Every copy of an ID goes to the same shard in file/line order. The
    // views in the shard inputs and outputs borrow from immutable storage.
    start = Clock::now();
    const auto workers = worker_budget();
    auto routed = route_events(storage, workers);
    profile("event_route_seconds", start);

    start = Clock::now();
    std::vector<EventShard> shards(workers);
    run_workers(workers, [&](std::size_t i) {
        shards[i] = load_shard(routed, i, window, products, unknown);
    });
    profile("event_shards_wall_seconds", start);
    std::size_t count = 0;
    double dedup = 0;
    double validate = 0;
    for (const auto& shard : shards) {
        count += shard.events.size();
        dedup = std::max(dedup, shard.dedup_seconds);
        validate = std::max(validate, shard.validate_seconds);
    }
    std::cout << "Event shards: " << workers << '\n'
              << "[INNER] event_shard_max_parse_dedup_seconds " << dedup << '\n'
              << "[INNER] event_shard_max_validate_seconds " << validate << '\n';

    start = Clock::now();
    std::vector<Event> events;
    events.reserve(count);
    for (auto& shard : shards) {
        std::vector<Key> keys;
        keys.reserve(shard.users.size());
        for (const auto user : shard.users) {
            keys.push_back(ids.canonical(user));
        }
        for (auto event : shard.events) {
            event.user = keys[event.user];
            events.push_back(event);
        }
        std::vector<Event>().swap(shard.events);
    }
    profile("event_merge_identity_seconds", start);
    return events;
}

struct Click {
    View id;
    Integer time;
    Key user;
    Key campaign;
};

struct Clicks {
    std::vector<Partition> storage;
    std::vector<Click> records;
};

class ClickIds {
public:
    bool insert(View id) {
        auto* slot = find_slot(slots_, id);
        if (!slot->empty()) {
            return false;
        }
        if (size_ >= slots_.size() - slots_.size() / 4) {
            std::vector<View> grown(slots_.size() * 2);
            for (const auto entry : slots_) {
                if (!entry.empty()) {
                    *find_slot(grown, entry) = entry;
                }
            }
            slots_.swap(grown);
            slot = find_slot(slots_, id);
        }
        *slot = id;
        ++size_;
        return true;
    }

private:
    std::vector<View> slots_ = std::vector<View>(256);
    std::size_t size_ = 0;

    static View* find_slot(std::vector<View>& slots, View id) {
        const auto mask = slots.size() - 1;
        auto index = std::hash<View>{}(id) & mask;
        while (!slots[index].empty() && slots[index] != id) {
            index = (index + 1) & mask;
        }
        return &slots[index];
    }
};

void load_clicks(Input& input, const Path& data, Window window, Identities& ids,
                 const Catalog& campaigns, Clicks& clicks) {
    const auto files = partitions(data / "clicks");
    clicks.storage.reserve(files.size());
    auto start = Clock::now();
    for (const auto& path : files) {
        clicks.storage.push_back(input.partition(path));
    }
    profile(input.map_partitions ? "click_map_seconds" : "click_read_seconds", start);
    start = Clock::now();
    ClickIds seen;
    for (const auto& file : clicks.storage) {
        rows(file.view(), [&](View line) {
            const auto fields = parse_csv<4>(line);
            if (!fields || (*fields)[0].empty()) {
                return;
            }
            const auto [id, user, campaign_name, click_time] = *fields;
            if (!seen.insert(id) || user.empty() || !valid_utf8(id) || !valid_utf8(user)) {
                return;
            }
            const auto campaign = campaigns.ids.find(campaign_name);
            const auto time = timestamp(click_time);
            if (campaign && time && *time < window.end &&
                !gap_exceeds(window.start, *time, attribution_ms)) {
                clicks.records.push_back({id, *time, ids.canonical(user), *campaign});
            }
        });
    }
    profile("click_parse_dedup_validate_seconds", start);
}

struct Session {
    std::size_t first;
    std::size_t end;
    Integer revenue;
};

std::vector<Session> sessions_for(const std::vector<Event>& events) {
    std::vector<Session> sessions;
    for (std::size_t i = 0; i < events.size(); ++i) {
        const auto& event = events[i];
        if (i == 0 || event.user != events[i - 1].user ||
            gap_exceeds(event.time, events[i - 1].time, session_ms)) {
            sessions.push_back({i, i + 1, event.revenue});
        } else {
            sessions.back().end = i + 1;
            add(sessions.back().revenue, event.revenue);
        }
    }
    return sessions;
}

struct UserRow {
    Key user;
    Integer day;
    Integer sessions = 0;
    Integer events = 0;
    Integer revenue = 0;
    std::optional<Key> highest_category;
};

std::vector<UserRow> aggregate_users(const std::vector<Event>& events,
                                     const std::vector<Session>& sessions, const Names& categories) {
    std::vector<UserRow> result;
    std::unordered_map<Key, Integer> category_revenue;
    const auto finish_group = [&] {
        if (result.empty()) {
            return;
        }
        auto& best = result.back().highest_category;
        Integer best_revenue = 0;
        for (const auto& [category, revenue] : category_revenue) {
            if (!best || revenue > best_revenue ||
                (revenue == best_revenue && categories.name(category) < categories.name(*best))) {
                best = category;
                best_revenue = revenue;
            }
        }
        category_revenue.clear();
    };
    for (const auto& session : sessions) {
        const auto& first = events[session.first];
        const auto day = utc_day(first.time);
        if (result.empty() || result.back().user != first.user || result.back().day != day) {
            finish_group();
            result.push_back(UserRow{first.user, day, 0, 0, 0, std::nullopt});
        }
        auto& row = result.back();
        add(row.sessions, 1);
        add(row.events, static_cast<Integer>(session.end - session.first));
        add(row.revenue, session.revenue);
        for (auto i = session.first; i < session.end; ++i) {
            const auto& event = events[i];
            if (event.revenue > 0) {
                add(category_revenue[event.category], event.revenue);
            }
        }
    }
    finish_group();
    return result;
}

struct CampaignTotals {
    Integer sessions = 0;
    Integer purchasing_sessions = 0;
    Integer revenue = 0;
    Integer assisted_sessions = 0;
};

using CampaignDaily = std::vector<std::unordered_map<Integer, CampaignTotals>>;

CampaignDaily attribute(const std::vector<Event>& events, const std::vector<Session>& sessions,
                        const std::vector<Click>& clicks, Key organic, std::size_t campaigns) {
    CampaignDaily daily(campaigns);
    std::unordered_map<Key, std::size_t> active;
    std::size_t left = 0;
    std::size_t right = 0;
    std::optional<Key> previous;
    for (const auto& session : sessions) {
        const auto& first = events[session.first];
        const auto user = first.user;
        const auto day = utc_day(first.time);
        if (!previous || *previous != user) {
            while (right < clicks.size() && clicks[right].user < user) {
                ++right;
            }
            left = right;
            active.clear();
            previous = user;
        }
        while (right < clicks.size() && clicks[right].user == user && clicks[right].time <= first.time) {
            ++active[clicks[right++].campaign];
        }
        while (left < right && gap_exceeds(first.time, clicks[left].time, attribution_ms)) {
            const auto entry = active.find(clicks[left++].campaign);
            if (--entry->second == 0) {
                active.erase(entry);
            }
        }
        for (const auto& [campaign, count] : active) {
            add(daily[campaign][day].assisted_sessions, 1);
        }
        Key credited = organic;
        if (left < right) {
            const auto first_latest = std::lower_bound(clicks.begin() + static_cast<std::ptrdiff_t>(left),
                clicks.begin() + static_cast<std::ptrdiff_t>(right), clicks[right - 1].time,
                [](const Click& click, Integer time) { return click.time < time; });
            credited = first_latest->campaign;
        }
        auto& row = daily[credited][day];
        add(row.sessions, 1);
        add(row.purchasing_sessions, session.revenue > 0 ? 1 : 0);
        add(row.revenue, session.revenue);
    }
    return daily;
}

std::string date_text(Integer day) {
    // Gregorian conversion over the full range of UTC days derived from i64
    // milliseconds. std::chrono::year has a narrower representation.
    const Integer serial = day + 719'468;
    const Integer era = (serial >= 0 ? serial : serial - 146'096) / 146'097;
    const Integer day_of_era = serial - era * 146'097;
    const Integer year_of_era =
        (day_of_era - day_of_era / 1'460 + day_of_era / 36'524 - day_of_era / 146'096) / 365;
    const Integer day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    const Integer march_month = (5 * day_of_year + 2) / 153;
    const Integer month = march_month + (march_month < 10 ? 3 : -9);
    const Integer month_day = day_of_year - (153 * march_month + 2) / 5 + 1;
    const Integer year = year_of_era + era * 400 + (month <= 2);
    char date[32];
    const int count = std::snprintf(date, sizeof(date), "%04lld-%02d-%02d",
        static_cast<long long>(year), static_cast<int>(month), static_cast<int>(month_day));
    if (count <= 0 || static_cast<std::size_t>(count) >= sizeof(date)) {
        throw std::runtime_error("unrepresentable report date");
    }
    return {date, static_cast<std::size_t>(count)};
}

struct ReportFile {
    std::ofstream stream;

    explicit ReportFile(const Path& path) {
        stream.exceptions(std::ios::badbit | std::ios::failbit);
        stream.open(path, std::ios::binary);
    }

    void finish() { stream.close(); }
};

void write_reports(const Path& output, const std::vector<UserRow>& users, const Identities& identities,
                   const Names& categories, const CampaignDaily& daily, const Catalog& campaigns,
                   Integer event_count, Integer session_count) {
    std::filesystem::create_directories(output);
    ReportFile user(output / "user_daily.csv");
    user.stream << "user_id,day,sessions,events,revenue_cents,top_category\n";
    Integer user_sessions = 0;
    Integer user_events = 0;
    Integer user_revenue = 0;
    for (const auto& row : users) {
        add(user_sessions, row.sessions);
        add(user_events, row.events);
        add(user_revenue, row.revenue);
        user.stream << identities.name(row.user) << ',' << date_text(row.day) << ',' << row.sessions
                    << ',' << row.events << ',' << row.revenue << ',';
        if (row.highest_category) {
            user.stream << categories.name(*row.highest_category);
        }
        user.stream << '\n';
    }
    user.finish();

    ReportFile campaign(output / "campaign_daily.csv");
    campaign.stream << "campaign_id,channel,day,sessions,purchasing_sessions,revenue_cents,assisted_sessions\n";
    Integer campaign_sessions = 0;
    Integer campaign_revenue = 0;
    for (Key id = 0; id < daily.size(); ++id) {
        for (const auto& [day, row] : daily[id]) {
            add(campaign_sessions, row.sessions);
            add(campaign_revenue, row.revenue);
            campaign.stream << campaigns.ids.name(id) << ',' << campaigns.label(id) << ','
                            << date_text(day) << ',' << row.sessions << ',' << row.purchasing_sessions
                            << ',' << row.revenue << ',' << row.assisted_sessions << '\n';
        }
    }
    campaign.finish();
    if (user_sessions != session_count || campaign_sessions != session_count ||
        user_events != event_count || user_revenue != campaign_revenue) {
        throw std::runtime_error("report reconciliation failed");
    }
    std::cout << "Reconciliation passed, " << event_count << " events, " << session_count
              << " sessions and " << user_revenue << " cents\n";
    std::cout << "CSV output bytes: " << std::filesystem::file_size(output / "user_daily.csv") +
        std::filesystem::file_size(output / "campaign_daily.csv") << '\n';
}

} // namespace

bool valid_utf8(View value) {
    for (std::size_t i = 0; i < value.size();) {
        const auto lead = static_cast<unsigned char>(value[i++]);
        if (lead < 0x80) {
            continue;
        }
        int remaining = 0;
        unsigned char low = 0x80;
        unsigned char high = 0xbf;
        if (lead >= 0xc2 && lead <= 0xdf) {
            remaining = 1;
        } else if (lead >= 0xe0 && lead <= 0xef) {
            remaining = 2;
            if (lead == 0xe0) low = 0xa0;
            if (lead == 0xed) high = 0x9f;
        } else if (lead >= 0xf0 && lead <= 0xf4) {
            remaining = 3;
            if (lead == 0xf0) low = 0x90;
            if (lead == 0xf4) high = 0x8f;
        } else {
            return false;
        }
        if (static_cast<std::size_t>(remaining) > value.size() - i) {
            return false;
        }
        while (remaining-- > 0) {
            const auto byte = static_cast<unsigned char>(value[i++]);
            if (byte < low || byte > high) {
                return false;
            }
            low = 0x80;
            high = 0xbf;
        }
    }
    return true;
}

std::optional<Integer> timestamp(View value) {
    using namespace std::chrono;
    const bool utc = value.size() == 20 && value[19] == 'Z';
    const bool offset = value.size() == 25 && (value[19] == '+' || value[19] == '-') && value[22] == ':';
    if (!utc && !offset) {
        return decimal(value, maximum);
    }
    if (value[4] != '-' || value[7] != '-' || value[10] != 'T' || value[13] != ':' || value[16] != ':') {
        return std::nullopt;
    }
    const auto y = fixed_digits(value.substr(0, 4));
    const auto m = fixed_digits(value.substr(5, 2));
    const auto d = fixed_digits(value.substr(8, 2));
    const auto h = fixed_digits(value.substr(11, 2));
    const auto min = fixed_digits(value.substr(14, 2));
    const auto s = fixed_digits(value.substr(17, 2));
    if (!y || !m || !d || !h || !min || !s || *y == 0 || *h > 23 || *min > 59 || *s > 59) {
        return std::nullopt;
    }
    const year_month_day civil{year{*y}, month{static_cast<unsigned>(*m)}, day{static_cast<unsigned>(*d)}};
    if (!civil.ok()) {
        return std::nullopt;
    }
    Integer correction = 0;
    if (offset) {
        const auto oh = fixed_digits(value.substr(20, 2));
        const auto om = fixed_digits(value.substr(23, 2));
        if (!oh || !om || *oh > 23 || *om > 59) {
            return std::nullopt;
        }
        correction = (*oh * 3600 + *om * 60) * (value[19] == '-' ? -1 : 1);
    }
    return sys_days{civil}.time_since_epoch().count() * day_ms +
           ((*h * 3600 + *min * 60 + *s) - correction) * 1000;
}

std::optional<Integer> purchase_price(View value) {
    const auto cents = decimal(value, 10'000'000);
    return cents && *cents > 0 ? cents : std::nullopt;
}

bool gap_exceeds(Integer newer, Integer older, Integer limit) {
    return older <= maximum - limit && newer > older + limit;
}

Integer utc_day(Integer time) {
    return time / day_ms - (time % day_ms < 0 ? 1 : 0);
}

void run(const Path& data, const Path& reports) {
    Input input;
    auto start = Clock::now();
    const auto window = load_window(input, data);
    Identities identities;
    for (const auto& path : partitions(data / "links")) {
        const auto file = input.read(path);
        rows(file, [&](View line) {
            const auto fields = parse_csv<2>(line);
            if (fields && !(*fields)[0].empty() && !(*fields)[1].empty()) {
                if (!valid_utf8((*fields)[0]) || !valid_utf8((*fields)[1])) {
                    throw std::runtime_error("invalid UTF-8 in a link");
                }
                identities.link((*fields)[0], (*fields)[1]);
            }
        });
    }
    profile("links_union_seconds", start);

    Catalog products;
    load_catalog(input, data / "products.csv", products);
    const Key unknown = products.labels.intern("unknown");
    auto events = load_events(input, data, window, identities, products, unknown);
    start = Clock::now();
    std::ranges::sort(events, {}, [](const Event& event) { return std::pair{event.user, event.time}; });
    profile("event_sort_seconds", start);

    start = Clock::now();
    const auto sessions = sessions_for(events);
    profile("sessionize_seconds", start);
    start = Clock::now();
    const auto user_rows = aggregate_users(events, sessions, products.labels);
    profile("user_aggregate_seconds", start);

    Catalog campaigns;
    load_catalog(input, data / "campaigns.csv", campaigns);
    Clicks clicks;
    load_clicks(input, data, window, identities, campaigns, clicks);
    const Key organic = campaigns.insert("organic", "organic");
    start = Clock::now();
    std::ranges::sort(clicks.records, {}, [](const Click& click) {
        return std::tuple{click.user, click.time, click.id};
    });
    profile("click_sort_seconds", start);

    start = Clock::now();
    const auto campaign_rows = attribute(events, sessions, clicks.records, organic, campaigns.ids.size());
    profile("attribute_seconds", start);
    start = Clock::now();
    write_reports(reports, user_rows, identities, products.labels, campaign_rows, campaigns,
                  static_cast<Integer>(events.size()), static_cast<Integer>(sessions.size()));
    profile("csv_export_seconds", start);
    std::cout << "[INNER] input_read_seconds " << input.read_seconds << '\n';
    if (input.map_partitions) {
        std::cout << "[INNER] input_map_seconds " << input.map_seconds << '\n';
    }
    std::cout << "Input bytes: " << input.bytes << '\n';
}

} // namespace shopping
