// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef TRACE_READER_RECORDS_H_
#define TRACE_READER_RECORDS_H_

#include <lib/stdcompat/variant.h>
#include <lib/trace-engine/types.h>
#include <stdint.h>
#include <zircon/assert.h>
#include <zircon/compiler.h>
#include <zircon/syscalls/object.h>
#include <zircon/types.h>

#include <new>
#include <type_traits>
#include <utility>
#include <vector>

#include <fbl/array.h>
#include <fbl/macros.h>
#include <fbl/string.h>

namespace trace {

// Holds a process koid and thread koid as a pair.
// Sorts by process koid then by thread koid.
class ProcessThread final {
 public:
  constexpr ProcessThread() : process_koid_(ZX_KOID_INVALID), thread_koid_(ZX_KOID_INVALID) {}
  constexpr explicit ProcessThread(zx_koid_t process_koid, zx_koid_t thread_koid)
      : process_koid_(process_koid), thread_koid_(thread_koid) {}
  constexpr ProcessThread(const ProcessThread& other)
      : process_koid_(other.process_koid_), thread_koid_(other.thread_koid_) {}

  constexpr explicit operator bool() const { return thread_koid_ != 0u || process_koid_ != 0u; }

  constexpr bool operator==(const ProcessThread& other) const {
    return process_koid_ == other.process_koid_ && thread_koid_ == other.thread_koid_;
  }

  constexpr bool operator!=(const ProcessThread& other) const { return !(*this == other); }

  constexpr bool operator<(const ProcessThread& other) const {
    if (process_koid_ != other.process_koid_) {
      return process_koid_ < other.process_koid_;
    }
    return thread_koid_ < other.thread_koid_;
  }

  constexpr zx_koid_t process_koid() const { return process_koid_; }
  constexpr zx_koid_t thread_koid() const { return thread_koid_; }

  ProcessThread& operator=(const ProcessThread& other) {
    process_koid_ = other.process_koid_;
    thread_koid_ = other.thread_koid_;
    return *this;
  }

  fbl::String ToString() const;

 private:
  zx_koid_t process_koid_;
  zx_koid_t thread_koid_;
};

// A typed argument value.
class ArgumentValue final {
 public:
  static ArgumentValue MakeNull() { return ArgumentValue(); }
  static ArgumentValue MakeBool(bool value) { return ArgumentValue(Bool{value}); }
  static ArgumentValue MakeInt32(int32_t value) { return ArgumentValue(value); }
  static ArgumentValue MakeUint32(uint32_t value) { return ArgumentValue(value); }
  static ArgumentValue MakeInt64(int64_t value) { return ArgumentValue(value); }
  static ArgumentValue MakeUint64(uint64_t value) { return ArgumentValue(value); }
  static ArgumentValue MakeDouble(double value) { return ArgumentValue(value); }
  static ArgumentValue MakeString(fbl::String value) { return ArgumentValue(std::move(value)); }
  static ArgumentValue MakePointer(uint64_t value) { return ArgumentValue(Pointer{value}); }
  static ArgumentValue MakeKoid(zx_koid_t value) { return ArgumentValue(Koid{value}); }

  ~ArgumentValue() = default;

  ArgumentValue(const ArgumentValue&) = default;
  ArgumentValue& operator=(const ArgumentValue&) = default;

  ArgumentValue(ArgumentValue&& other) noexcept { *this = std::move(other); }
  ArgumentValue& operator=(ArgumentValue&& other) noexcept {
    if (this != &other) {
      value_ = std::move(other.value_);
      other.value_.emplace<Null>();
    }
    return *this;
  }

  ArgumentType type() const { return static_cast<ArgumentType>(value_.index()); }

  uint32_t GetBool() const {
    ZX_DEBUG_ASSERT(type() == ArgumentType::kBool);
    return cpp17::get<Bool>(value_).value;
  }
  int32_t GetInt32() const {
    ZX_DEBUG_ASSERT(type() == ArgumentType::kInt32);
    return cpp17::get<int32_t>(value_);
  }
  uint32_t GetUint32() const {
    ZX_DEBUG_ASSERT(type() == ArgumentType::kUint32);
    return cpp17::get<uint32_t>(value_);
  }
  int64_t GetInt64() const {
    ZX_DEBUG_ASSERT(type() == ArgumentType::kInt64);
    return cpp17::get<int64_t>(value_);
  }
  uint64_t GetUint64() const {
    ZX_DEBUG_ASSERT(type() == ArgumentType::kUint64);
    return cpp17::get<uint64_t>(value_);
  }
  double GetDouble() const {
    ZX_DEBUG_ASSERT(type() == ArgumentType::kDouble);
    return cpp17::get<double>(value_);
  }
  const fbl::String& GetString() const {
    ZX_DEBUG_ASSERT(type() == ArgumentType::kString);
    return cpp17::get<fbl::String>(value_);
  }
  uint64_t GetPointer() const {
    ZX_DEBUG_ASSERT(type() == ArgumentType::kPointer);
    return cpp17::get<Pointer>(value_).value;
  }
  zx_koid_t GetKoid() const {
    ZX_DEBUG_ASSERT(type() == ArgumentType::kKoid);
    return cpp17::get<Koid>(value_).value;
  }

  fbl::String ToString() const;

 private:
  // Strong wrapper types for argument types that have no value or have ambiguous implicit
  // conversions.
  struct Null {};
  struct Bool {
    bool value;
  };
  struct Pointer {
    uint64_t value;
  };
  struct Koid {
    zx_koid_t value;
  };

  ArgumentValue() : value_{Null{}} {}
  explicit ArgumentValue(Bool b) : value_(b) {}
  explicit ArgumentValue(int32_t int32) : value_(int32) {}
  explicit ArgumentValue(uint32_t uint32) : value_(uint32) {}
  explicit ArgumentValue(int64_t int64) : value_(int64) {}
  explicit ArgumentValue(uint64_t uint64) : value_(uint64) {}
  explicit ArgumentValue(double d) : value_(d) {}
  explicit ArgumentValue(fbl::String string) : value_(std::move(string)) {}
  explicit ArgumentValue(Pointer pointer) : value_(pointer) {}
  explicit ArgumentValue(Koid koid) : value_(koid) {}

  using Variant = cpp17::variant<Null, int32_t, uint32_t, int64_t, uint64_t, double, fbl::String,
                                 Pointer, Koid, Bool>;
  Variant value_;

  // Ensure the variant size and type order matches the type enum values.
  template <typename T, size_t I>
  static constexpr bool TypeIndexCheck =
      std::is_same_v<T, cpp17::variant_alternative_t<I, Variant>>;
  static_assert(TypeIndexCheck<Bool, TRACE_ARG_BOOL>);
  static_assert(TypeIndexCheck<int32_t, TRACE_ARG_INT32>);
  static_assert(TypeIndexCheck<uint32_t, TRACE_ARG_UINT32>);
  static_assert(TypeIndexCheck<int64_t, TRACE_ARG_INT64>);
  static_assert(TypeIndexCheck<uint64_t, TRACE_ARG_UINT64>);
  static_assert(TypeIndexCheck<double, TRACE_ARG_DOUBLE>);
  static_assert(TypeIndexCheck<fbl::String, TRACE_ARG_STRING>);
  static_assert(TypeIndexCheck<Pointer, TRACE_ARG_POINTER>);
  static_assert(TypeIndexCheck<Koid, TRACE_ARG_KOID>);
  static_assert(TypeIndexCheck<Bool, TRACE_ARG_BOOL>);
};

// Named argument and value.
class Argument final {
 public:
  explicit Argument(fbl::String name, ArgumentValue value)
      : name_(std::move(name)), value_(std::move(value)) {}

  Argument(const Argument&) = default;
  Argument& operator=(const Argument&) = default;

  Argument(Argument&&) = default;
  Argument& operator=(Argument&&) = default;

  const fbl::String& name() const { return name_; }
  const ArgumentValue& value() const { return value_; }
  ArgumentType type() const { return value_.type(); }

  fbl::String ToString() const;

  static const Argument* Find(const fbl::String& name, const std::vector<Argument>& arguments) {
    for (const Argument& argument : arguments) {
      if (argument.name() == name) {
        return &argument;
      }
    }
    return nullptr;
  }

 private:
  fbl::String name_;
  ArgumentValue value_;
};

// Trace Info type specific data
class TraceInfoContent final {
 public:
  // Magic number record data
  struct MagicNumberInfo {
    uint32_t magic_value;
  };

  explicit TraceInfoContent(MagicNumberInfo magic_number_info)
      : type_(TraceInfoType::kMagicNumber), magic_number_info_(std::move(magic_number_info)) {}

  const MagicNumberInfo& GetMagicNumberInfo() const {
    ZX_DEBUG_ASSERT(type_ == TraceInfoType::kMagicNumber);
    return magic_number_info_;
  }

  TraceInfoContent(TraceInfoContent&& other) : type_(other.type_) { MoveFrom(std::move(other)); }

  ~TraceInfoContent() { Destroy(); }

  TraceInfoContent& operator=(TraceInfoContent&& other) {
    Destroy();
    MoveFrom(std::move(other));
    return *this;
  }

  TraceInfoType type() const { return type_; }

  fbl::String ToString() const;

 private:
  void Destroy();
  void MoveFrom(TraceInfoContent&& other);

  TraceInfoType type_;
  union {
    MagicNumberInfo magic_number_info_;
  };

  DISALLOW_COPY_AND_ASSIGN_ALLOW_MOVE(TraceInfoContent);
};

// Metadata type specific data.
class MetadataContent final {
 public:
  // Provider info event data.
  struct ProviderInfo {
    ProviderId id;
    fbl::String name;
  };

  // Provider section event data.
  struct ProviderSection {
    ProviderId id;
  };

  // Provider event event data.
  struct ProviderEvent {
    ProviderId id;
    ProviderEventType event;
  };

  // Trace info record data
  struct TraceInfo {
    TraceInfoType type() const { return content.type(); }
    TraceInfoContent content;
  };

  explicit MetadataContent(ProviderInfo provider_info)
      : type_(MetadataType::kProviderInfo), provider_info_(std::move(provider_info)) {}

  explicit MetadataContent(ProviderSection provider_section)
      : type_(MetadataType::kProviderSection), provider_section_(std::move(provider_section)) {}

  explicit MetadataContent(ProviderEvent provider_event)
      : type_(MetadataType::kProviderEvent), provider_event_(std::move(provider_event)) {}

  explicit MetadataContent(TraceInfo trace_info)
      : type_(MetadataType::kTraceInfo), trace_info_(std::move(trace_info)) {}

  const ProviderInfo& GetProviderInfo() const {
    ZX_DEBUG_ASSERT(type_ == MetadataType::kProviderInfo);
    return provider_info_;
  }

  const ProviderSection& GetProviderSection() const {
    ZX_DEBUG_ASSERT(type_ == MetadataType::kProviderSection);
    return provider_section_;
  }

  const ProviderEvent& GetProviderEvent() const {
    ZX_DEBUG_ASSERT(type_ == MetadataType::kProviderEvent);
    return provider_event_;
  }

  MetadataContent(MetadataContent&& other) : type_(other.type_) { MoveFrom(std::move(other)); }

  ~MetadataContent() { Destroy(); }

  MetadataContent& operator=(MetadataContent&& other) {
    Destroy();
    MoveFrom(std::move(other));
    return *this;
  }

  MetadataType type() const { return type_; }

  fbl::String ToString() const;

 private:
  void Destroy();
  void MoveFrom(MetadataContent&& other);

  MetadataType type_;
  union {
    ProviderInfo provider_info_;
    ProviderSection provider_section_;
    ProviderEvent provider_event_;
    TraceInfo trace_info_;
  };

  DISALLOW_COPY_AND_ASSIGN_ALLOW_MOVE(MetadataContent);
};

// Event type specific data.
class EventData final {
 public:
  // Instant event data.
  struct Instant {
    EventScope scope;
  };

  // Counter event data.
  struct Counter {
    trace_counter_id_t id;
  };

  // Duration begin event data.
  struct DurationBegin {};

  // Duration end event data.
  struct DurationEnd {};

  // Duration complete event data.
  struct DurationComplete {
    trace_ticks_t end_time;
  };

  // Async begin event data.
  struct AsyncBegin {
    trace_async_id_t id;
  };

  // Async instant event data.
  struct AsyncInstant {
    trace_async_id_t id;
  };

  // Async end event data.
  struct AsyncEnd {
    trace_async_id_t id;
  };

  // Flow begin event data.
  struct FlowBegin {
    trace_flow_id_t id;
  };

  // Flow step event data.
  struct FlowStep {
    trace_flow_id_t id;
  };

  // Flow end event data.
  struct FlowEnd {
    trace_flow_id_t id;
  };

  explicit EventData(Instant instant) : type_(EventType::kInstant), instant_(std::move(instant)) {}

  explicit EventData(Counter counter) : type_(EventType::kCounter), counter_(std::move(counter)) {}

  explicit EventData(DurationBegin duration_begin)
      : type_(EventType::kDurationBegin), duration_begin_(std::move(duration_begin)) {}

  explicit EventData(DurationEnd duration_end)
      : type_(EventType::kDurationEnd), duration_end_(std::move(duration_end)) {}

  explicit EventData(DurationComplete duration_complete)
      : type_(EventType::kDurationComplete), duration_complete_(std::move(duration_complete)) {}

  explicit EventData(AsyncBegin async_begin)
      : type_(EventType::kAsyncBegin), async_begin_(std::move(async_begin)) {}

  explicit EventData(AsyncInstant async_instant)
      : type_(EventType::kAsyncInstant), async_instant_(std::move(async_instant)) {}

  explicit EventData(AsyncEnd async_end)
      : type_(EventType::kAsyncEnd), async_end_(std::move(async_end)) {}

  explicit EventData(FlowBegin flow_begin)
      : type_(EventType::kFlowBegin), flow_begin_(std::move(flow_begin)) {}

  explicit EventData(FlowStep flow_step)
      : type_(EventType::kFlowStep), flow_step_(std::move(flow_step)) {}

  explicit EventData(FlowEnd flow_end)
      : type_(EventType::kFlowEnd), flow_end_(std::move(flow_end)) {}

  EventData(EventData&& other) { MoveFrom(std::move(other)); }

  ~EventData() { Destroy(); }

  EventData& operator=(EventData&& other) {
    Destroy();
    MoveFrom(std::move(other));
    return *this;
  }

  const Instant& GetInstant() const {
    ZX_DEBUG_ASSERT(type_ == EventType::kInstant);
    return instant_;
  }

  const Counter& GetCounter() const {
    ZX_DEBUG_ASSERT(type_ == EventType::kCounter);
    return counter_;
  }

  const DurationBegin& GetDurationBegin() const {
    ZX_DEBUG_ASSERT(type_ == EventType::kDurationBegin);
    return duration_begin_;
  }

  const DurationEnd& GetDurationEnd() const {
    ZX_DEBUG_ASSERT(type_ == EventType::kDurationEnd);
    return duration_end_;
  }

  const DurationComplete& GetDurationComplete() const {
    ZX_DEBUG_ASSERT(type_ == EventType::kDurationComplete);
    return duration_complete_;
  }

  const AsyncBegin& GetAsyncBegin() const {
    ZX_DEBUG_ASSERT(type_ == EventType::kAsyncBegin);
    return async_begin_;
  }

  const AsyncInstant& GetAsyncInstant() const {
    ZX_DEBUG_ASSERT(type_ == EventType::kAsyncInstant);
    return async_instant_;
  }

  const AsyncEnd& GetAsyncEnd() const {
    ZX_DEBUG_ASSERT(type_ == EventType::kAsyncEnd);
    return async_end_;
  }

  const FlowBegin& GetFlowBegin() const {
    ZX_DEBUG_ASSERT(type_ == EventType::kFlowBegin);
    return flow_begin_;
  }

  const FlowStep& GetFlowStep() const {
    ZX_DEBUG_ASSERT(type_ == EventType::kFlowStep);
    return flow_step_;
  }

  const FlowEnd& GetFlowEnd() const {
    ZX_DEBUG_ASSERT(type_ == EventType::kFlowEnd);
    return flow_end_;
  }

  EventType type() const { return type_; }

  fbl::String ToString() const;

 private:
  void Destroy();
  void MoveFrom(EventData&& other);

  EventType type_;
  union {
    Instant instant_;
    Counter counter_;
    DurationBegin duration_begin_;
    DurationEnd duration_end_;
    DurationComplete duration_complete_;
    AsyncBegin async_begin_;
    AsyncInstant async_instant_;
    AsyncEnd async_end_;
    FlowBegin flow_begin_;
    FlowStep flow_step_;
    FlowEnd flow_end_;
  };

  DISALLOW_COPY_AND_ASSIGN_ALLOW_MOVE(EventData);
};

// Large record specific data
class LargeRecordData final {
 public:
  struct BlobEvent {
    fbl::String category;
    fbl::String name;
    trace_ticks_t timestamp;
    ProcessThread process_thread;
    std::vector<Argument> arguments;

    const void* blob;
    uint64_t blob_size;
  };

  struct BlobAttachment {
    fbl::String category;
    fbl::String name;

    const void* blob;
    uint64_t blob_size;
  };

  // Large blob record data.
  // The blob data pointer is actually just a pointer into the trace
  // reader's buffer. As such, the record consumer should not attempt
  // to free it. The record consumer must finish processing the
  // blob data within the callback, as the pointer may not be valid
  // after the completion of that callback.
  using Blob = cpp17::variant<BlobEvent, BlobAttachment>;

  explicit LargeRecordData(Blob blob) : type_(LargeRecordType::kBlob), blob_(std::move(blob)) {}

  const Blob& GetBlob() const {
    ZX_DEBUG_ASSERT(type_ == LargeRecordType::kBlob);
    return blob_;
  }

  LargeRecordData(LargeRecordData&& other) : type_(other.type_) { MoveFrom(std::move(other)); }

  ~LargeRecordData() { Destroy(); }

  LargeRecordData& operator=(LargeRecordData&& other) {
    Destroy();
    MoveFrom(std::move(other));
    return *this;
  }

  LargeRecordType type() const { return type_; }

  fbl::String ToString() const;

 private:
  void Destroy();
  void MoveFrom(LargeRecordData&& other);

  LargeRecordType type_;
  union {
    Blob blob_;
  };

  DISALLOW_COPY_AND_ASSIGN_ALLOW_MOVE(LargeRecordData);
};

// A decoded record.
class Record final {
 public:
  // Metadata record data.
  struct Metadata {
    MetadataType type() const { return content.type(); }
    MetadataContent content;
  };

  // Initialization record data.
  struct Initialization {
    trace_ticks_t ticks_per_second;
  };

  // String record data.
  struct String {
    trace_string_index_t index;
    fbl::String string;
  };

  // Thread record data.
  struct Thread {
    trace_thread_index_t index;
    ProcessThread process_thread;
  };

  // Event record data.
  struct Event {
    EventType type() const { return data.type(); }
    trace_ticks_t timestamp;
    ProcessThread process_thread;
    fbl::String category;
    fbl::String name;
    std::vector<Argument> arguments;
    EventData data;
  };

  // Blob record data.
  // Since blobs can be rather large we avoid unnecessary copying of them.
  // This then means that the consumer must process the blob's payload
  // before the next record is read.
  struct Blob {
    trace_blob_type_t type;
    fbl::String name;
    const void* blob;
    size_t blob_size;
  };

  // Kernel Object record data.
  struct KernelObject {
    zx_koid_t koid;
    zx_obj_type_t object_type;
    fbl::String name;
    std::vector<Argument> arguments;
  };

  // Scheduler Event record data.
  struct SchedulerEvent {
    struct LegacyContextSwitch {
      trace_ticks_t timestamp;
      trace_cpu_number_t cpu_number;
      ThreadState outgoing_thread_state;
      ProcessThread outgoing_thread;
      ProcessThread incoming_thread;
      trace_thread_priority_t outgoing_thread_priority;
      trace_thread_priority_t incoming_thread_priority;
    };
    struct ContextSwitch {
      trace_ticks_t timestamp;
      trace_cpu_number_t cpu_number;
      ThreadState outgoing_thread_state;
      zx_koid_t outgoing_tid;
      zx_koid_t incoming_tid;
      std::vector<Argument> arguments;

      const Argument* FindArgument(const fbl::String& name) const {
        return Argument::Find(name, arguments);
      }
    };
    struct ThreadWakeup {
      trace_ticks_t timestamp;
      trace_cpu_number_t cpu_number;
      zx_koid_t incoming_tid;
      std::vector<Argument> arguments;

      const Argument* FindArgument(const fbl::String& name) const {
        return Argument::Find(name, arguments);
      }
    };

    explicit SchedulerEvent(LegacyContextSwitch record)
        : event_type{SchedulerEventType::kLegacyContextSwitch}, event{std::move(record)} {}
    explicit SchedulerEvent(ContextSwitch record)
        : event_type{SchedulerEventType::kContextSwitch}, event{std::move(record)} {}
    explicit SchedulerEvent(ThreadWakeup record)
        : event_type{SchedulerEventType::kThreadWakeup}, event{std::move(record)} {}

    SchedulerEvent(const SchedulerEvent&) = default;
    SchedulerEvent& operator=(const SchedulerEvent&) = default;
    SchedulerEvent(SchedulerEvent&&) = default;
    SchedulerEvent& operator=(SchedulerEvent&&) = default;

    SchedulerEventType type() const { return event_type; }

    const LegacyContextSwitch& legacy_context_switch() const {
      ZX_DEBUG_ASSERT(event_type == SchedulerEventType::kLegacyContextSwitch);
      return cpp17::get<LegacyContextSwitch>(event);
    }
    const ContextSwitch& context_switch() const {
      ZX_DEBUG_ASSERT(event_type == SchedulerEventType::kContextSwitch);
      return cpp17::get<ContextSwitch>(event);
    }
    const ThreadWakeup& thread_wakeup() const {
      ZX_DEBUG_ASSERT(event_type == SchedulerEventType::kThreadWakeup);
      return cpp17::get<ThreadWakeup>(event);
    }

    SchedulerEventType event_type;
    cpp17::variant<LegacyContextSwitch, ContextSwitch, ThreadWakeup> event;
  };

  // Log record data.
  struct Log {
    trace_ticks_t timestamp;
    ProcessThread process_thread;
    fbl::String message;
  };

  // Large record data.
  using Large = LargeRecordData;

  explicit Record(Metadata record) : type_(RecordType::kMetadata), metadata_(std::move(record)) {}

  explicit Record(Initialization record)
      : type_(RecordType::kInitialization), initialization_(std::move(record)) {}

  explicit Record(String record) : type_(RecordType::kString) {
    new (&string_) String(std::move(record));
  }

  explicit Record(Thread record) : type_(RecordType::kThread) {
    new (&thread_) Thread(std::move(record));
  }

  explicit Record(Event record) : type_(RecordType::kEvent) {
    new (&event_) Event(std::move(record));
  }

  explicit Record(Blob record) : type_(RecordType::kBlob) { new (&blob_) Blob(std::move(record)); }

  explicit Record(KernelObject record) : type_(RecordType::kKernelObject) {
    new (&kernel_object_) KernelObject(std::move(record));
  }

  explicit Record(SchedulerEvent record) : type_(RecordType::kScheduler) {
    new (&scheduler_event_) SchedulerEvent(std::move(record));
  }

  explicit Record(Log record) : type_(RecordType::kLog) { new (&log_) Log(std::move(record)); }

  explicit Record(Large record) : type_(RecordType::kLargeRecord) {
    new (&large_) Large(std::move(record));
  }

  Record(Record&& other) { MoveFrom(std::move(other)); }

  ~Record() { Destroy(); }

  Record& operator=(Record&& other) {
    Destroy();
    MoveFrom(std::move(other));
    return *this;
  }

  const Metadata& GetMetadata() const {
    ZX_DEBUG_ASSERT(type_ == RecordType::kMetadata);
    return metadata_;
  }

  const Initialization& GetInitialization() const {
    ZX_DEBUG_ASSERT(type_ == RecordType::kInitialization);
    return initialization_;
  }

  const String& GetString() const {
    ZX_DEBUG_ASSERT(type_ == RecordType::kString);
    return string_;
  }

  const Thread& GetThread() const {
    ZX_DEBUG_ASSERT(type_ == RecordType::kThread);
    return thread_;
  }

  const Event& GetEvent() const {
    ZX_DEBUG_ASSERT(type_ == RecordType::kEvent);
    return event_;
  }

  const Blob& GetBlob() const {
    ZX_DEBUG_ASSERT(type_ == RecordType::kBlob);
    return blob_;
  }

  const KernelObject& GetKernelObject() const {
    ZX_DEBUG_ASSERT(type_ == RecordType::kKernelObject);
    return kernel_object_;
  }

  const SchedulerEvent& GetSchedulerEvent() const {
    ZX_DEBUG_ASSERT(type_ == RecordType::kScheduler);
    return scheduler_event_;
  }

  const Log& GetLog() const {
    ZX_DEBUG_ASSERT(type_ == RecordType::kLog);
    return log_;
  }

  const Large& GetLargeRecord() const {
    ZX_DEBUG_ASSERT(type_ == RecordType::kLargeRecord);
    return large_;
  }

  RecordType type() const { return type_; }

  fbl::String ToString() const;

  std::optional<fbl::String> GetName() const;

 private:
  void Destroy();
  void MoveFrom(Record&& other);

  RecordType type_;
  union {
    Metadata metadata_;
    Initialization initialization_;
    String string_;
    Thread thread_;
    Event event_;
    Blob blob_;
    KernelObject kernel_object_;
    SchedulerEvent scheduler_event_;
    Log log_;
    Large large_;
  };

  DISALLOW_COPY_AND_ASSIGN_ALLOW_MOVE(Record);
};

}  // namespace trace

#endif  // TRACE_READER_RECORDS_H_
