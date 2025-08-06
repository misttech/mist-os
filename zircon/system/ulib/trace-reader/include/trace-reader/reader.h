// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef TRACE_READER_READER_H_
#define TRACE_READER_READER_H_

#include <lib/fit/function.h>
#include <zircon/assert.h>

#include <memory>
#include <optional>
#include <span>
#include <string>
#include <string_view>
#include <unordered_map>
#include <utility>
#include <vector>

#include <trace-reader/records.h>

namespace trace {

class Chunk;

// Reads trace records.
// The input is a collection of |Chunk| objects (see class Chunk below).
//
// One use-case is reading records across an entire trace, which means across
// multiple providers, and makes no assumptions about the ordering of records
// it receives other than requiring objects referenced by ID (threads, strings)
// are defined before they are used. Note that as a consequence of this, one
// |TraceReader| class will maintain state of reading the entire trace: it
// generally doesn't work to create multiple |TraceReader| classes for one
// trace.
class TraceReader {
 public:
  // Called once for each record read by |ReadRecords|.
  // TODO(jeffbrown): It would be nice to get rid of this by making |ReadRecords|
  // return std::optional<Record> as an out parameter.
  using RecordConsumer = fit::function<void(Record)>;

  // Callback invoked when decoding errors are detected in the trace.
  using ErrorHandler = fit::function<void(std::string_view)>;

  explicit TraceReader(RecordConsumer record_consumer, ErrorHandler error_handler);

  // Reads as many records as possible from the chunk, invoking the
  // record consumer for each one.  Returns true if the stream could possibly
  // contain more records if the chunk were extended with new data.
  // Returns false if the trace stream is irrecoverably corrupt and no
  // further decoding is possible.  May be called repeatedly with new
  // chunks as they become available to resume decoding.
  bool ReadRecords(Chunk& chunk);

  // Gets the current trace provider id.
  // Returns 0 if no providers have been registered yet.
  ProviderId current_provider_id() const { return current_provider_->id; }

  // Gets the name of the current trace provider.
  // Returns an empty string if the current provider id is 0.
  const std::string& current_provider_name() const { return current_provider_->name; }

  // Gets the name of the specified provider, or an empty string if there is
  // no such provider.
  std::string GetProviderName(ProviderId id) const;

  const ErrorHandler& error_handler() const { return error_handler_; }

 protected:
  void ReportError(std::string_view error) const;

 private:
  bool ReadMetadataRecord(Chunk& record, RecordHeader header);
  bool ReadInitializationRecord(Chunk& record, RecordHeader header);
  bool ReadStringRecord(Chunk& record, RecordHeader header);
  bool ReadThreadRecord(Chunk& record, RecordHeader header);
  bool ReadEventRecord(Chunk& record, RecordHeader header);
  bool ReadBlobRecord(Chunk& record, RecordHeader header, void** out_blob);
  bool ReadKernelObjectRecord(Chunk& record, RecordHeader header);
  bool ReadSchedulerRecord(Chunk& record, RecordHeader header);
  bool ReadLogRecord(Chunk& record, RecordHeader header);
  bool ReadArguments(Chunk& record, size_t count, std::vector<Argument>* out_arguments);

  bool ReadLargeRecord(Chunk& record, RecordHeader header);
  bool ReadLargeBlob(Chunk& record, RecordHeader header);

  void SetCurrentProvider(ProviderId id);
  void RegisterProvider(ProviderId id, std::string name);
  void RegisterString(trace_string_index_t index, const std::string& string);
  void RegisterThread(trace_thread_index_t index, const ProcessThread& process_thread);

  bool DecodeStringRef(Chunk& chunk, trace_encoded_string_ref_t string_ref,
                       std::string* out_string) const;
  bool DecodeThreadRef(Chunk& chunk, trace_encoded_thread_ref_t thread_ref,
                       ProcessThread* out_process_thread) const;

  RecordConsumer const record_consumer_;
  ErrorHandler const error_handler_;

  RecordHeader pending_header_ = 0u;

  struct StringTableEntry {
    StringTableEntry(trace_string_index_t index, std::string string)
        : index(index), string(std::move(string)) {}

    trace_string_index_t const index;
    std::string const string;

    // Used by the hash table.
    trace_string_index_t GetKey() const { return index; }
    static size_t GetHash(trace_string_index_t key) { return key; }
  };

  struct ThreadTableEntry {
    ThreadTableEntry(trace_thread_index_t index, const ProcessThread& process_thread)
        : index(index), process_thread(process_thread) {}

    trace_thread_index_t const index;
    ProcessThread const process_thread;

    // Used by the hash table.
    trace_thread_index_t GetKey() const { return index; }
    static size_t GetHash(trace_thread_index_t key) { return key; }
  };

  struct ProviderInfo {
    ProviderId id;
    std::string name;

    std::unordered_map<trace_string_index_t, StringTableEntry> string_table;
    std::unordered_map<trace_thread_index_t, ThreadTableEntry> thread_table;
  };

  std::unordered_map<ProviderId, std::unique_ptr<ProviderInfo>> providers_;
  ProviderInfo* current_provider_ = nullptr;

  TraceReader(const TraceReader&) = delete;
  TraceReader& operator=(const TraceReader&) = delete;
  TraceReader(TraceReader&&) = delete;
  TraceReader& operator=(TraceReader&&) = delete;
};

// Provides support for reading sequences of 64-bit words from a contiguous
// region of memory. The main use-case of this class is input to |TraceReader|.
class Chunk final {
 public:
  Chunk(const uint64_t* begin, size_t num_words);

  uint64_t current_byte_offset() const {
    return (reinterpret_cast<const uint8_t*>(current_) - reinterpret_cast<const uint8_t*>(begin_));
  }
  uint64_t remaining_words() const { return end_ - current_; }

  // Reads from the chunk, maintaining proper alignment.
  // Returns true on success, false if the chunk has insufficient remaining
  // words to satisfy the request.
  std::optional<uint64_t> ReadUint64();
  std::optional<int64_t> ReadInt64();
  std::optional<double> ReadDouble();
  std::optional<std::string_view> ReadString(size_t length);
  std::optional<Chunk> ReadChunk(size_t num_words);
  std::optional<void const*> ReadInPlace(size_t num_words);

  std::span<const uint64_t> Words() const { return {begin_, end_}; }

 private:
  const uint64_t* begin_;
  const uint64_t* current_;
  const uint64_t* end_;
};

}  // namespace trace

#endif  // TRACE_READER_READER_H_
