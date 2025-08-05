// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <inttypes.h>
#include <string.h>

#include <iomanip>
#include <sstream>
#include <utility>

#include <trace-reader/records.h>

namespace trace {
namespace {
const char* EventScopeToString(EventScope scope) {
  switch (scope) {
    case EventScope::kGlobal:
      return "global";
    case EventScope::kProcess:
      return "process";
    case EventScope::kThread:
      return "thread";
  }
  return "???";
}

const char* ThreadStateToString(ThreadState state) {
  switch (state) {
    case ThreadState::kNew:
      return "new";
    case ThreadState::kRunning:
      return "running";
    case ThreadState::kSuspended:
      return "suspended";
    case ThreadState::kBlocked:
      return "blocked";
    case ThreadState::kDying:
      return "dying";
    case ThreadState::kDead:
      return "dead";
  }
  return "???";
}

const char* ObjectTypeToString(zx_obj_type_t type) {
  switch (type) {
    case ZX_OBJ_TYPE_PROCESS:
      return "process";
    case ZX_OBJ_TYPE_THREAD:
      return "thread";
    case ZX_OBJ_TYPE_VMO:
      return "vmo";
    case ZX_OBJ_TYPE_CHANNEL:
      return "channel";
    case ZX_OBJ_TYPE_EVENT:
      return "event";
    case ZX_OBJ_TYPE_PORT:
      return "port";
    case ZX_OBJ_TYPE_INTERRUPT:
      return "interrupt";
    case ZX_OBJ_TYPE_PCI_DEVICE:
      return "pci-device";
    case ZX_OBJ_TYPE_LOG:
      return "log";
    case ZX_OBJ_TYPE_SOCKET:
      return "socket";
    case ZX_OBJ_TYPE_RESOURCE:
      return "resource";
    case ZX_OBJ_TYPE_EVENTPAIR:
      return "event-pair";
    case ZX_OBJ_TYPE_JOB:
      return "job";
    case ZX_OBJ_TYPE_VMAR:
      return "vmar";
    case ZX_OBJ_TYPE_FIFO:
      return "fifo";
    case ZX_OBJ_TYPE_GUEST:
      return "guest";
    case ZX_OBJ_TYPE_VCPU:
      return "vcpu";
    case ZX_OBJ_TYPE_TIMER:
      return "timer";
    case ZX_OBJ_TYPE_IOMMU:
      return "iommu";
    case ZX_OBJ_TYPE_BTI:
      return "bti";
    case ZX_OBJ_TYPE_PROFILE:
      return "profile";
    case ZX_OBJ_TYPE_PMT:
      return "pmt";
    case ZX_OBJ_TYPE_SUSPEND_TOKEN:
      return "suspend-token";
    case ZX_OBJ_TYPE_PAGER:
      return "pager";
    case ZX_OBJ_TYPE_EXCEPTION:
      return "exception";
    default:
      return "???";
  }
}

template <size_t max_preview_start, size_t max_preview_end>
std::string PreviewBlobData(const void* blob, size_t blob_size) {
  static_assert((max_preview_start + max_preview_end) > 0);

  auto blob_data = (const unsigned char*)blob;
  std::string result;
  result.reserve((3 * (max_preview_start + max_preview_end)) + 128);

  size_t num_leading_bytes;
  size_t num_trailing_bytes;
  if (blob_size <= (max_preview_start + max_preview_end)) {
    num_leading_bytes = blob_size;
    num_trailing_bytes = 0;
  } else {
    num_leading_bytes = max_preview_start;
    num_trailing_bytes = max_preview_end;
  }

  char buf[3];  // Shared buffer into which to format hex data byte by byte.
  result.append("<");
  for (size_t i = 0; i < num_leading_bytes; i++) {
    if (i > 0)
      result.append(" ");
    snprintf(buf, sizeof(buf), "%02x", blob_data[i]);
    result.append(buf);
  }
  if (num_trailing_bytes)
    result.append(" ...");
  for (size_t i = blob_size - num_trailing_bytes; i < blob_size; i++) {
    result.append(" ");
    snprintf(buf, sizeof(buf), "%02x", blob_data[i]);
    result.append(buf);
  }
  result.append(">");

  result.shrink_to_fit();
  return result;
}

std::string FormatArgumentList(const std::vector<trace::Argument>& args) {
  std::string result;
  result.reserve(1024);

  result.append("{");
  for (size_t i = 0; i < args.size(); i++) {
    if (i != 0)
      result.append(", ");
    result.append(args[i].ToString());
  }
  result.append("}");

  result.shrink_to_fit();
  return result;
}
}  // namespace

std::string ProcessThread::ToString() const {
  return (std::stringstream() << process_koid_ << "/" << thread_koid_).str();
}

std::string ArgumentValue::ToString() const {
  switch (type()) {
    case ArgumentType::kNull:
      return "null";
    case ArgumentType::kBool: {
      std::stringstream ss;
      ss << std::boolalpha << "bool(" << std::get<Bool>(value_).value << ")";
      return ss.str();
    }
    case ArgumentType::kInt32:
      return (std::stringstream() << "int32(" << std::get<int32_t>(value_) << ")").str();
    case ArgumentType::kUint32:
      return (std::stringstream() << "uint32(" << std::get<uint32_t>(value_) << ")").str();
    case ArgumentType::kInt64:
      return (std::stringstream() << "int64(" << std::get<int64_t>(value_) << ")").str();
    case ArgumentType::kUint64:
      return (std::stringstream() << "uint64(" << std::get<uint64_t>(value_) << ")").str();
    case ArgumentType::kDouble: {
      std::stringstream ss;
      ss << std::fixed << std::setprecision(6) << "double(" << std::get<double>(value_) << ")";
      return ss.str();
    }
    case ArgumentType::kString:
      return (std::stringstream() << "string(\"" << std::get<std::string>(value_) << "\")").str();
    case ArgumentType::kPointer: {
      auto p = std::get<Pointer>(value_).value;
      return (std::stringstream() << std::hex << "pointer(" << (p ? "0x" : "") << p << ")").str();
    }
    case ArgumentType::kKoid:
      return (std::stringstream() << "koid(" << std::get<Koid>(value_).value << ")").str();
    case ArgumentType::kBlob:
      return (std::stringstream() << "blob(length=" << std::get<std::vector<uint8_t>>(value_).size()
                                  << ")")
          .str();
  }
  ZX_ASSERT(false);
}

std::string Argument::ToString() const {
  return (std::stringstream() << name_ << ": " << value_.ToString()).str();
}

void TraceInfoContent::Destroy() {
  switch (type_) {
    case TraceInfoType::kMagicNumber:
      magic_number_info_.~MagicNumberInfo();
      break;
  }
}

void TraceInfoContent::MoveFrom(TraceInfoContent&& other) {
  type_ = other.type_;
  switch (type_) {
    case TraceInfoType::kMagicNumber:
      new (&magic_number_info_) MagicNumberInfo(std::move(other.magic_number_info_));
      break;
  }
}

std::string TraceInfoContent::ToString() const {
  switch (type_) {
    case TraceInfoType::kMagicNumber: {
      std::stringstream ss;
      ss << "MagicNumberInfo(magic_value: 0x" << std::hex << magic_number_info_.magic_value << ")";
      return ss.str();
    }
  }
  ZX_ASSERT(false);
}

void MetadataContent::Destroy() {
  switch (type_) {
    case MetadataType::kProviderInfo:
      provider_info_.~ProviderInfo();
      break;
    case MetadataType::kProviderSection:
      provider_section_.~ProviderSection();
      break;
    case MetadataType::kProviderEvent:
      provider_event_.~ProviderEvent();
      break;
    case MetadataType::kTraceInfo:
      trace_info_.~TraceInfo();
      break;
  }
}

void MetadataContent::MoveFrom(MetadataContent&& other) {
  type_ = other.type_;
  switch (type_) {
    case MetadataType::kProviderInfo:
      new (&provider_info_) ProviderInfo(std::move(other.provider_info_));
      break;
    case MetadataType::kProviderSection:
      new (&provider_section_) ProviderSection(std::move(other.provider_section_));
      break;
    case MetadataType::kProviderEvent:
      new (&provider_event_) ProviderEvent(std::move(other.provider_event_));
      break;
    case MetadataType::kTraceInfo:
      new (&trace_info_) TraceInfo(std::move(other.trace_info_));
      break;
  }
}

std::string MetadataContent::ToString() const {
  std::stringstream ss;
  switch (type_) {
    case MetadataType::kProviderInfo:
      ss << "ProviderInfo(id: " << provider_info_.id << ", name: \"" << provider_info_.name
         << "\")";
      return ss.str();
    case MetadataType::kProviderSection:
      ss << "ProviderSection(id: " << provider_section_.id << ")";
      return ss.str();
    case MetadataType::kProviderEvent: {
      ss << "ProviderEvent(id: " << provider_event_.id << ", ";
      ProviderEventType type = provider_event_.event;
      switch (type) {
        case ProviderEventType::kBufferOverflow:
          ss << "buffer overflow";
          break;
        default: {
          ss << "unknown(" << static_cast<unsigned>(type) << ")";
          break;
        }
      }
      ss << ")";
      return ss.str();
    }
    case MetadataType::kTraceInfo: {
      ss << "TraceInfo(content: " << trace_info_.content.ToString() << ")";
      return ss.str();
    }
  }
  ZX_ASSERT(false);
}

void EventData::Destroy() {
  switch (type_) {
    case EventType::kInstant:
      instant_.~Instant();
      break;
    case EventType::kCounter:
      counter_.~Counter();
      break;
    case EventType::kDurationBegin:
      duration_begin_.~DurationBegin();
      break;
    case EventType::kDurationEnd:
      duration_end_.~DurationEnd();
      break;
    case EventType::kDurationComplete:
      duration_complete_.~DurationComplete();
      break;
    case EventType::kAsyncBegin:
      async_begin_.~AsyncBegin();
      break;
    case EventType::kAsyncInstant:
      async_instant_.~AsyncInstant();
      break;
    case EventType::kAsyncEnd:
      async_end_.~AsyncEnd();
      break;
    case EventType::kFlowBegin:
      flow_begin_.~FlowBegin();
      break;
    case EventType::kFlowStep:
      flow_step_.~FlowStep();
      break;
    case EventType::kFlowEnd:
      flow_end_.~FlowEnd();
      break;
  }
}

void EventData::MoveFrom(EventData&& other) {
  type_ = other.type_;
  switch (type_) {
    case EventType::kInstant:
      new (&instant_) Instant(std::move(other.instant_));
      break;
    case EventType::kCounter:
      new (&counter_) Counter(std::move(other.counter_));
      break;
    case EventType::kDurationBegin:
      new (&duration_begin_) DurationBegin(std::move(other.duration_begin_));
      break;
    case EventType::kDurationEnd:
      new (&duration_end_) DurationEnd(std::move(other.duration_end_));
      break;
    case EventType::kDurationComplete:
      new (&duration_complete_) DurationComplete(std::move(other.duration_complete_));
      break;
    case EventType::kAsyncBegin:
      new (&async_begin_) AsyncBegin(std::move(other.async_begin_));
      break;
    case EventType::kAsyncInstant:
      new (&async_instant_) AsyncInstant(std::move(other.async_instant_));
      break;
    case EventType::kAsyncEnd:
      new (&async_end_) AsyncEnd(std::move(other.async_end_));
      break;
    case EventType::kFlowBegin:
      new (&flow_begin_) FlowBegin(std::move(other.flow_begin_));
      break;
    case EventType::kFlowStep:
      new (&flow_step_) FlowStep(std::move(other.flow_step_));
      break;
    case EventType::kFlowEnd:
      new (&flow_end_) FlowEnd(std::move(other.flow_end_));
      break;
  }
}

std::string EventData::ToString() const {
  switch (type_) {
    case EventType::kInstant: {
      std::stringstream ss;
      ss << "Instant(scope: " << EventScopeToString(instant_.scope) << ")";
      return ss.str();
    }
    case EventType::kCounter:
      return (std::stringstream() << "Counter(id: " << counter_.id << ")").str();
    case EventType::kDurationBegin:
      return "DurationBegin";
    case EventType::kDurationEnd:
      return "DurationEnd";
    case EventType::kDurationComplete: {
      std::stringstream ss;
      ss << "DurationComplete(end_ts: " << duration_complete_.end_time << ")";
      return ss.str();
    }
    case EventType::kAsyncBegin:
      return (std::stringstream() << "AsyncBegin(id: " << async_begin_.id << ")").str();
    case EventType::kAsyncInstant:
      return (std::stringstream() << "AsyncInstant(id: " << async_instant_.id << ")").str();
    case EventType::kAsyncEnd:
      return (std::stringstream() << "AsyncEnd(id: " << async_end_.id << ")").str();
    case EventType::kFlowBegin:
      return (std::stringstream() << "FlowBegin(id: " << flow_begin_.id << ")").str();
    case EventType::kFlowStep:
      return (std::stringstream() << "FlowStep(id: " << flow_step_.id << ")").str();
    case EventType::kFlowEnd:
      return (std::stringstream() << "FlowEnd(id: " << flow_end_.id << ")").str();
  }
  ZX_ASSERT(false);
}

void LargeRecordData::Destroy() {
  switch (type_) {
    case LargeRecordType::kBlob:
      blob_.~Blob();
      break;
  }
}

void LargeRecordData::MoveFrom(trace::LargeRecordData&& other) {
  switch (type_) {
    case LargeRecordType::kBlob:
      new (&blob_) Blob(std::move(other.blob_));
      break;
  }
}

std::string LargeRecordData::ToString() const {
  std::stringstream ss;
  switch (type_) {
    case LargeRecordType::kBlob:
      if (std::holds_alternative<BlobEvent>(blob_)) {
        const auto& data = std::get<BlobEvent>(blob_);
        ss << "Blob(format: blob_event, category: \"" << data.category << "\""
           << ", name: \"" << data.name << "\""
           << ", ts: " << data.timestamp << ", pt: " << data.process_thread.ToString() << ", "
           << FormatArgumentList(data.arguments) << ", size: " << data.blob_size
           << ", preview: " << PreviewBlobData<8, 8>(data.blob, data.blob_size) << ")";
        return ss.str();
      } else if (std::holds_alternative<BlobAttachment>(blob_)) {
        const auto& data = std::get<BlobAttachment>(blob_);
        ss << "Blob(format: blob_attachment, category: \"" << data.category << "\""
           << ", name: \"" << data.name << "\""
           << ", size: " << data.blob_size
           << ", preview: " << PreviewBlobData<8, 8>(data.blob, data.blob_size) << ")";
        return ss.str();
      }
      break;
  }
  ZX_ASSERT(false);
}

void Record::Destroy() {
  switch (type_) {
    case RecordType::kMetadata:
      metadata_.~Metadata();
      break;
    case RecordType::kInitialization:
      initialization_.~Initialization();
      break;
    case RecordType::kString:
      string_.~String();
      break;
    case RecordType::kThread:
      thread_.~Thread();
      break;
    case RecordType::kEvent:
      event_.~Event();
      break;
    case RecordType::kBlob:
      blob_.~Blob();
      break;
    case RecordType::kKernelObject:
      kernel_object_.~KernelObject();
      break;
    case RecordType::kScheduler:
      scheduler_event_.~SchedulerEvent();
      break;
    case RecordType::kLog:
      log_.~Log();
      break;
    case RecordType::kLargeRecord:
      large_.~Large();
      break;
  }
}

void Record::MoveFrom(Record&& other) {
  type_ = other.type_;
  switch (type_) {
    case RecordType::kMetadata:
      new (&metadata_) Metadata(std::move(other.metadata_));
      break;
    case RecordType::kInitialization:
      new (&initialization_) Initialization(std::move(other.initialization_));
      break;
    case RecordType::kString:
      new (&string_) String(std::move(other.string_));
      break;
    case RecordType::kThread:
      new (&thread_) Thread(std::move(other.thread_));
      break;
    case RecordType::kEvent:
      new (&event_) Event(std::move(other.event_));
      break;
    case RecordType::kBlob:
      new (&blob_) Blob(std::move(other.blob_));
      break;
    case RecordType::kKernelObject:
      new (&kernel_object_) KernelObject(std::move(other.kernel_object_));
      break;
    case RecordType::kScheduler:
      new (&scheduler_event_) SchedulerEvent(std::move(other.scheduler_event_));
      break;
    case RecordType::kLog:
      new (&log_) Log(std::move(other.log_));
      break;
    case RecordType::kLargeRecord:
      new (&large_) Large(std::move(other.large_));
      break;
  }
}

std::string Record::ToString() const {
  std::stringstream ss;
  switch (type_) {
    case RecordType::kMetadata:
      ss << "Metadata(content: " << metadata_.content.ToString() << ")";
      return ss.str();
    case RecordType::kInitialization:
      ss << "Initialization(ticks_per_second: " << initialization_.ticks_per_second << ")";
      return ss.str();
    case RecordType::kString:
      ss << "String(index: " << string_.index << ", \"" << string_.string << "\")";
      return ss.str();
    case RecordType::kThread:
      ss << "Thread(index: " << thread_.index << ", " << thread_.process_thread.ToString() << ")";
      return ss.str();
    case RecordType::kEvent:
      ss << "Event(ts: " << event_.timestamp << ", pt: " << event_.process_thread.ToString()
         << ", category: \"" << event_.category << "\", name: \"" << event_.name << "\", "
         << event_.data.ToString() << ", " << FormatArgumentList(event_.arguments) << ")";
      return ss.str();
    case RecordType::kBlob:
      ss << "Blob(name: " << blob_.name << ", size: " << blob_.blob_size
         << ", preview: " << PreviewBlobData<8, 8>(blob_.blob, blob_.blob_size) << ")";
      return ss.str();
    case RecordType::kKernelObject:
      ss << "KernelObject(koid: " << kernel_object_.koid
         << ", type: " << ObjectTypeToString(kernel_object_.object_type) << ", name: \""
         << kernel_object_.name << "\", " << FormatArgumentList(kernel_object_.arguments) << ")";
      return ss.str();
    case RecordType::kScheduler:
      if (scheduler_event_.type() == SchedulerEventType::kLegacyContextSwitch) {
        auto& context_switch = scheduler_event_.legacy_context_switch();
        ss << "ContextSwitch(ts: " << context_switch.timestamp
           << ", cpu: " << context_switch.cpu_number
           << ", os: " << ThreadStateToString(context_switch.outgoing_thread_state)
           << ", opt: " << context_switch.outgoing_thread.ToString()
           << ", ipt: " << context_switch.incoming_thread.ToString()
           << ", oprio: " << context_switch.outgoing_thread_priority
           << ", iprio: " << context_switch.incoming_thread_priority << ")";
      } else if (scheduler_event_.type() == SchedulerEventType::kContextSwitch) {
        auto& context_switch = scheduler_event_.context_switch();
        ss << "ContextSwitch(ts: " << context_switch.timestamp
           << ", cpu: " << context_switch.cpu_number
           << ", os: " << ThreadStateToString(context_switch.outgoing_thread_state)
           << ", ot: " << context_switch.outgoing_tid << ", it: " << context_switch.incoming_tid
           << ", " << FormatArgumentList(context_switch.arguments) << ")";
      } else if (scheduler_event_.type() == SchedulerEventType::kThreadWakeup) {
        auto& thread_wakeup = scheduler_event_.thread_wakeup();
        ss << "ThreadWakeup(ts: " << thread_wakeup.timestamp
           << ", cpu: " << thread_wakeup.cpu_number << ", it: " << thread_wakeup.incoming_tid
           << ", " << FormatArgumentList(thread_wakeup.arguments) << ")";
      } else {
        ss << "UnknownSchedulerEvent(type: " << static_cast<int>(scheduler_event_.type()) << ")";
      }
      return ss.str();
    case RecordType::kLog:
      ss << "Log(ts: " << log_.timestamp << ", pt: " << log_.process_thread.ToString() << ", \""
         << log_.message << "\")";
      return ss.str();
    case RecordType::kLargeRecord:
      ss << "LargeRecord(" << large_.ToString() << ")";
      return ss.str();
  }
  ZX_ASSERT(false);
}

std::optional<std::string> Record::GetName() const {
  switch (type_) {
    // Do not have a namefield
    case RecordType::kMetadata:
    case RecordType::kInitialization:
    case RecordType::kString:
    case RecordType::kThread:
    case RecordType::kScheduler:
    case RecordType::kLog:
    case RecordType::kLargeRecord:
      return std::nullopt;
    case RecordType::kEvent:
      return {event_.name};
    case RecordType::kBlob:
      return {blob_.name};
    case RecordType::kKernelObject:
      return {kernel_object_.name};
  }
}

}  // namespace trace
