#pragma once

#include <cstdint>
#include <filesystem>
#include <memory>
#include <optional>
#include <stdexcept>
#include <string_view>
#include <tuple>
#include <utility>

#include <arrow/api.h>
#include <arrow/io/file.h>
#include <parquet/arrow/writer.h>

namespace shopping {

inline void check_arrow(const arrow::Status& status) {
    if (!status.ok()) {
        throw std::runtime_error(status.ToString());
    }
}

template<class T>
T arrow_value(arrow::Result<T> result) {
    check_arrow(result.status());
    return std::move(result).ValueUnsafe();
}

inline std::int32_t parquet_day(std::int64_t day) {
    if (!std::in_range<std::int32_t>(day)) {
        throw std::overflow_error("report date exceeds Parquet DATE range");
    }
    return static_cast<std::int32_t>(day);
}

template<class Builder, class Value>
void append_parquet(Builder& builder, Value value) {
    check_arrow(builder.Append(value));
}

inline void append_parquet(arrow::StringBuilder& builder, std::optional<std::string_view> value) {
    check_arrow(value ? builder.Append(*value) : builder.AppendNull());
}

template<class... Columns>
class ParquetOutput {
public:
    ParquetOutput(const std::filesystem::path& path, arrow::FieldVector fields)
        : schema_(arrow::schema(std::move(fields))) {
        std::filesystem::create_directories(path.parent_path());
        file_ = arrow_value(arrow::io::FileOutputStream::Open(path.string()));
        const auto properties = parquet::WriterProperties::Builder()
            .compression(parquet::Compression::SNAPPY)
            ->max_row_group_length(batch_rows)
            ->build();
        writer_ = arrow_value(parquet::arrow::FileWriter::Open(
            *schema_, arrow::default_memory_pool(), file_, properties));
    }

    template<class... Values>
    void append(Values... values) {
        std::apply([&](auto&... columns) {
            (append_parquet(columns, values), ...);
        }, columns_);
        if (++rows_ == batch_rows) {
            flush();
        }
    }

    void finish() {
        if (rows_ != 0) {
            flush();
        }
        check_arrow(writer_->Close());
        check_arrow(file_->Close());
    }

private:
    static constexpr std::int64_t batch_rows = 64 * 1024;
    std::shared_ptr<arrow::Schema> schema_;
    std::shared_ptr<arrow::io::FileOutputStream> file_;
    std::unique_ptr<parquet::arrow::FileWriter> writer_;
    std::tuple<Columns...> columns_;
    std::int64_t rows_ = 0;

    void flush() {
        arrow::ArrayVector arrays;
        arrays.reserve(sizeof...(Columns));
        std::apply([&](auto&... columns) {
            (arrays.push_back(arrow_value(static_cast<arrow::ArrayBuilder&>(columns).Finish())), ...);
        }, columns_);
        const auto batch = arrow::RecordBatch::Make(schema_, rows_, std::move(arrays));
        check_arrow(writer_->WriteRecordBatch(*batch));
        rows_ = 0;
    }
};

} // namespace shopping
