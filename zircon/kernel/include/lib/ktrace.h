// Copyright 2016 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_INCLUDE_LIB_KTRACE_H_
#define ZIRCON_KERNEL_INCLUDE_LIB_KTRACE_H_

#include <lib/fxt/interned_category.h>
#include <lib/fxt/serializer.h>
#include <lib/user_copy/user_ptr.h>
#include <lib/zircon-internal/ktrace.h>
#include <platform.h>
#include <zircon/compiler.h>
#include <zircon/types.h>

#include <kernel/thread.h>
#include <ktl/atomic.h>

namespace ktrace_thunks {

bool category_enabled(const fxt::InternedCategory& category);

ssize_t read_user(user_out_ptr<void> ptr, uint32_t off, size_t len);

template <fxt::RefType name_type, fxt::ArgumentType... arg_types, fxt::RefType... arg_name_types,
          fxt::RefType... arg_val_types>
void fxt_kernel_object(zx_koid_t koid, zx_obj_type_t obj_type,
                       const fxt::StringRef<name_type>& name_arg,
                       const fxt::Argument<arg_types, arg_name_types, arg_val_types>&... args);

template <fxt::RefType thread_type, fxt::RefType name_type, fxt::ArgumentType... arg_types,
          fxt::RefType... arg_name_types, fxt::RefType... arg_val_types>
void fxt_instant(const fxt::InternedCategory& category, uint64_t timestamp,
                 const fxt::ThreadRef<thread_type>& thread_ref,
                 const fxt::StringRef<name_type>& name_ref,
                 const fxt::Argument<arg_types, arg_name_types, arg_val_types>&... args);

template <fxt::RefType thread_type, fxt::RefType name_type, fxt::ArgumentType... arg_types,
          fxt::RefType... arg_name_types, fxt::RefType... arg_val_types>
void fxt_duration_begin(const fxt::InternedCategory& category, uint64_t timestamp,
                        const fxt::ThreadRef<thread_type>& thread_ref,
                        const fxt::StringRef<name_type>& name_ref,
                        const fxt::Argument<arg_types, arg_name_types, arg_val_types>&... args);

template <fxt::RefType thread_type, fxt::RefType name_type, fxt::ArgumentType... arg_types,
          fxt::RefType... arg_name_types, fxt::RefType... arg_val_types>
void fxt_duration_end(const fxt::InternedCategory& category, uint64_t timestamp,
                      const fxt::ThreadRef<thread_type>& thread_ref,
                      const fxt::StringRef<name_type>& name_ref,
                      const fxt::Argument<arg_types, arg_name_types, arg_val_types>&... args);

template <fxt::RefType thread_type, fxt::RefType name_type, fxt::ArgumentType... arg_types,
          fxt::RefType... arg_name_types, fxt::RefType... arg_val_types>
void fxt_duration_complete(const fxt::InternedCategory& category, uint64_t start_time,
                           const fxt::ThreadRef<thread_type>& thread_ref,
                           const fxt::StringRef<name_type>& name_ref, uint64_t end_time,
                           const fxt::Argument<arg_types, arg_name_types, arg_val_types>&... args);

template <fxt::RefType thread_type, fxt::RefType name_type, fxt::ArgumentType... arg_types,
          fxt::RefType... arg_name_types, fxt::RefType... arg_val_types>
void fxt_counter(const fxt::InternedCategory& category, uint64_t timestamp,
                 const fxt::ThreadRef<thread_type>& thread_ref,
                 const fxt::StringRef<name_type>& name_ref, uint64_t counter_id,
                 const fxt::Argument<arg_types, arg_name_types, arg_val_types>&... args);

template <fxt::RefType thread_type, fxt::RefType name_type, fxt::ArgumentType... arg_types,
          fxt::RefType... arg_name_types, fxt::RefType... arg_val_types>
void fxt_flow_begin(const fxt::InternedCategory& category, uint64_t timestamp,
                    const fxt::ThreadRef<thread_type>& thread_ref,
                    const fxt::StringRef<name_type>& name_ref, uint64_t flow_id,
                    const fxt::Argument<arg_types, arg_name_types, arg_val_types>&... args);

template <fxt::RefType thread_type, fxt::RefType name_type, fxt::ArgumentType... arg_types,
          fxt::RefType... arg_name_types, fxt::RefType... arg_val_types>
void fxt_flow_step(const fxt::InternedCategory& category, uint64_t timestamp,
                   const fxt::ThreadRef<thread_type>& thread_ref,
                   const fxt::StringRef<name_type>& name_ref, uint64_t flow_id,
                   const fxt::Argument<arg_types, arg_name_types, arg_val_types>&... args);

template <fxt::RefType thread_type, fxt::RefType name_type, fxt::ArgumentType... arg_types,
          fxt::RefType... arg_name_types, fxt::RefType... arg_val_types>
void fxt_flow_end(const fxt::InternedCategory& category, uint64_t timestamp,
                  const fxt::ThreadRef<thread_type>& thread_ref,
                  const fxt::StringRef<name_type>& name_ref, uint64_t flow_id,
                  const fxt::Argument<arg_types, arg_name_types, arg_val_types>&... args);

template <fxt::RefType outgoing_type, fxt::RefType incoming_type>
void fxt_context_switch(uint64_t timestamp, uint8_t cpu_number,
                        zx_thread_state_t outgoing_thread_state,
                        const fxt::ThreadRef<outgoing_type>& outgoing_thread,
                        const fxt::ThreadRef<incoming_type>& incoming_thread,
                        uint8_t outgoing_thread_priority, uint8_t incoming_thread_priority);

void fxt_string_record(uint16_t index, const char* string, size_t string_length);

}  // namespace ktrace_thunks

constexpr fxt::Koid kNoProcess{0u};

// TODO(fxbug.dev/118873): Move ktrace interfaces into ktrace namespace.
namespace ktrace {

// Maintains the mapping from CPU numbers to pre-allocated KOIDs.
class CpuContextMap {
 public:
  // Returns the pre-allocated KOID for the given CPU.
  static zx_koid_t GetCpuKoid(cpu_num_t cpu_num) { return cpu_koid_base_ + cpu_num; }

  // Returns a ThreadRef for the given CPU.
  static fxt::ThreadRef<fxt::RefType::kInline> GetCpuRef(cpu_num_t cpu_num) {
    return {kNoProcess, fxt::Koid{GetCpuKoid(cpu_num)}};
  }

  // Returns a ThreadRef for the current CPU.
  static fxt::ThreadRef<fxt::RefType::kInline> GetCurrentCpuRef() {
    return GetCpuRef(arch_curr_cpu_num());
  }

  // Initializes the CPU KOID base value.
  static void Init();

 private:
  // The KOID of CPU 0. Valid CPU KOIDs are in the range [base, base + max_cpus).
  static zx_koid_t cpu_koid_base_;
};

}  // namespace ktrace

// Specifies whether the trace applies to the current thread or cpu.
enum class TraceContext {
  Thread,
  Cpu,
  // TODO(eieio): Support process?
};

inline fxt::ThreadRef<fxt::RefType::kInline> ThreadRefFromContext(TraceContext context) {
  switch (context) {
    case TraceContext::Thread:
      return Thread::Current::Get()->fxt_ref();
    case TraceContext::Cpu:
      return ktrace::CpuContextMap::GetCurrentCpuRef();
    default:
      return {kNoProcess, fxt::Koid{0}};
  }
}

// Argument type that specifies whether a trace function is enabled or disabled.
template <bool enabled>
struct TraceEnabled {};

// Type that specifies whether tracing is enabled or disabled for the local
// compilation unit.
template <bool enabled>
constexpr auto LocalTrace = TraceEnabled<enabled>{};

// Constants that specify unconditional enabled or disabled tracing.
constexpr auto TraceAlways = TraceEnabled<true>{};
constexpr auto TraceNever = TraceEnabled<false>{};

static inline uint64_t ktrace_timestamp() { return current_ticks(); }

// Bring the fxt literal operators into the global scope.
using fxt::operator""_category;
using fxt::operator""_intern;

// Indicate that the current time should be recorded when writing a trace record.
//
// Used for ktrace calls which accept a custom timestamp as a parameter.
inline constexpr uint64_t kRecordCurrentTimestamp = 0xffffffff'ffffffff;

// Utility macro to convert string literals passed to local tracing macros into
// fxt::InternedString literals.
//
// Example:
//
// #define LOCAL_KTRACE_ENABLE 0
//
// #define LOCAL_KTRACE(string, args...)
//     ktrace_probe(LocalTrace<LOCAL_KTRACE_ENABLE>, TraceContext::Cpu,
//                  KTRACE_INTERN_STRING(string), ##args)
//
#define KTRACE_INTERN_STRING_CAT(a, b) a##b
#define KTRACE_INTERN_STRING(string) KTRACE_INTERN_STRING_CAT(string, _intern)

inline bool ktrace_category_enabled(const fxt::InternedCategory& category) {
  return ktrace_thunks::category_enabled(category);
}

template <bool enabled>
inline void ktrace_probe(TraceEnabled<enabled>, TraceContext context,
                         const fxt::InternedString& label) {
  if constexpr (enabled) {
    if (ktrace_thunks::category_enabled("kernel:probe"_category)) {
      ktrace_thunks::fxt_instant("kernel:probe"_category, current_ticks(),
                                 ThreadRefFromContext(context), fxt::StringRef{label});
    }
  }
}

template <bool enabled>
inline void ktrace_probe(TraceEnabled<enabled>, TraceContext context,
                         const fxt::InternedString& label, uint32_t a, uint32_t b) {
  if constexpr (enabled) {
    if (ktrace_thunks::category_enabled("kernel:probe"_category)) {
      ktrace_thunks::fxt_instant("kernel:probe"_category, current_ticks(),
                                 ThreadRefFromContext(context), fxt::StringRef(label),
                                 fxt::Argument{"arg0"_intern, a}, fxt::Argument{"arg1"_intern, b});
    }
  }
}

template <bool enabled>
inline void ktrace_probe(TraceEnabled<enabled>, TraceContext context,
                         const fxt::InternedString& label, uint64_t a) {
  if constexpr (enabled) {
    if (ktrace_thunks::category_enabled("kernel:probe"_category)) {
      ktrace_thunks::fxt_instant("kernel:probe"_category, current_ticks(),
                                 ThreadRefFromContext(context), fxt::StringRef{label},
                                 fxt::Argument{"arg0"_intern, a});
    }
  }
}

template <bool enabled>
inline void ktrace_probe(TraceEnabled<enabled>, TraceContext context,
                         const fxt::InternedString& label, uint64_t a, uint64_t b) {
  if constexpr (enabled) {
    if (ktrace_thunks::category_enabled("kernel:probe"_category)) {
      ktrace_thunks::fxt_instant("kernel:probe"_category, current_ticks(),
                                 ThreadRefFromContext(context), fxt::StringRef{label},
                                 fxt::Argument{"arg0"_intern, a}, fxt::Argument{"arg1"_intern, b});
    }
  }
}

template <bool enabled>
inline void ktrace_begin_duration(TraceEnabled<enabled>, const fxt::InternedCategory& category,
                                  TraceContext context, const fxt::InternedString& label) {
  if constexpr (enabled) {
    if (ktrace_thunks::category_enabled(category)) {
      ktrace_thunks::fxt_duration_begin(category, current_ticks(), ThreadRefFromContext(context),
                                        fxt::StringRef{label});
    }
  }
}

template <bool enabled>
inline void ktrace_end_duration(TraceEnabled<enabled>, const fxt::InternedCategory& category,
                                TraceContext context, const fxt::InternedString& label) {
  if constexpr (enabled) {
    if (ktrace_thunks::category_enabled(category)) {
      ktrace_thunks::fxt_duration_end(category, current_ticks(), ThreadRefFromContext(context),
                                      fxt::StringRef{label});
    }
  }
}

template <bool enabled>
inline void ktrace_begin_duration(TraceEnabled<enabled>, const fxt::InternedCategory& category,
                                  TraceContext context, const fxt::InternedString& label,
                                  uint64_t a, uint64_t b) {
  if constexpr (enabled) {
    if (ktrace_thunks::category_enabled(category)) {
      ktrace_thunks::fxt_duration_begin(category, current_ticks(), ThreadRefFromContext(context),
                                        fxt::StringRef{label}, fxt::Argument{"arg0"_intern, a},
                                        fxt::Argument{"arg1"_intern, b});
    }
  }
}

template <bool enabled>
inline void ktrace_end_duration(TraceEnabled<enabled>, const fxt::InternedCategory& category,
                                TraceContext context, const fxt::InternedString& label, uint64_t a,
                                uint64_t b) {
  if constexpr (enabled) {
    if (ktrace_thunks::category_enabled(category)) {
      ktrace_thunks::fxt_duration_end(category, current_ticks(), ThreadRefFromContext(context),
                                      fxt::StringRef{label}, fxt::Argument{"arg0"_intern, a},
                                      fxt::Argument{"arg1"_intern, b});
    }
  }
}

template <bool enabled>
inline void ktrace_flow_begin(TraceEnabled<enabled>, const fxt::InternedCategory& category,
                              TraceContext context, const fxt::InternedString& label,
                              uint64_t flow_id, uint64_t a = 0) {
  if constexpr (enabled) {
    if (ktrace_thunks::category_enabled(category)) {
      ktrace_thunks::fxt_flow_begin(category, current_ticks(), ThreadRefFromContext(context),
                                    fxt::StringRef{label}, flow_id,
                                    fxt::Argument{"arg0"_intern, a});
    }
  }
}

template <bool enabled>
inline void ktrace_flow_end(TraceEnabled<enabled>, const fxt::InternedCategory& category,
                            TraceContext context, const fxt::InternedString& label,
                            uint64_t flow_id, uint64_t a = 0) {
  if constexpr (enabled) {
    if (ktrace_thunks::category_enabled(category)) {
      ktrace_thunks::fxt_flow_end(category, current_ticks(), ThreadRefFromContext(context),
                                  fxt::StringRef{label}, flow_id, fxt::Argument{"arg0"_intern, a});
    }
  }
}

template <bool enabled>
inline void ktrace_flow_step(TraceEnabled<enabled>, const fxt::InternedCategory& category,
                             TraceContext context, const fxt::InternedString& label,
                             uint64_t flow_id, uint64_t a = 0) {
  if constexpr (enabled) {
    if (ktrace_thunks::category_enabled(category)) {
      ktrace_thunks::fxt_flow_step(category, current_ticks(), ThreadRefFromContext(context),
                                   fxt::StringRef{label}, flow_id, fxt::Argument{"arg0"_intern, a});
    }
  }
}

template <bool enabled>
inline void ktrace_counter(TraceEnabled<enabled>, const fxt::InternedCategory& category,
                           const fxt::InternedString& label, int64_t value,
                           uint64_t counter_id = 0) {
  if constexpr (enabled) {
    if (ktrace_thunks::category_enabled(category)) {
      ktrace_thunks::fxt_counter(category, current_ticks(), ThreadRefFromContext(TraceContext::Cpu),
                                 fxt::StringRef{label}, counter_id,
                                 fxt::Argument{"arg0"_intern, value});
    }
  }
}

inline ssize_t ktrace_read_user(user_out_ptr<void> ptr, uint32_t off, size_t len) {
  return ktrace_thunks::read_user(ptr, off, len);
}

template <fxt::RefType name_type, fxt::ArgumentType... arg_types, fxt::RefType... arg_name_types,
          fxt::RefType... arg_val_types>
inline void fxt_kernel_object(
    zx_koid_t koid, zx_obj_type_t obj_type, const fxt::StringRef<name_type>& name_arg,
    const fxt::Argument<arg_types, arg_name_types, arg_val_types>&... args) {
  ktrace_thunks::fxt_kernel_object(koid, obj_type, name_arg, args...);
}

template <fxt::RefType outgoing_type, fxt::RefType incoming_type>
inline void fxt_context_switch(uint64_t timestamp, uint8_t cpu_num,
                               zx_thread_state_t outgoing_thread_state,
                               const fxt::ThreadRef<outgoing_type>& outgoing_thread,
                               const fxt::ThreadRef<incoming_type>& incoming_thread,
                               uint8_t outgoing_priority, uint8_t incoming_priority) {
  ktrace_thunks::fxt_context_switch(timestamp, cpu_num, outgoing_thread_state, outgoing_thread,
                                    incoming_thread, outgoing_priority, incoming_priority);
}

inline void fxt_string_record(uint16_t index, const char* string, size_t string_length) {
  ktrace_thunks::fxt_string_record(index, string, string_length);
}

template <fxt::RefType thread_type, fxt::RefType name_type, fxt::ArgumentType... arg_types,
          fxt::RefType... arg_name_types, fxt::RefType... arg_val_types>
inline void fxt_instant(const fxt::InternedCategory& category, uint64_t timestamp,
                        const fxt::ThreadRef<thread_type>& thread_ref,
                        const fxt::StringRef<name_type>& name_ref,
                        const fxt::Argument<arg_types, arg_name_types, arg_val_types>&... args) {
  ktrace_thunks::fxt_instant(category, timestamp, thread_ref, name_ref, args...);
}

template <fxt::RefType thread_type, fxt::RefType name_type, fxt::ArgumentType... arg_types,
          fxt::RefType... arg_name_types, fxt::RefType... arg_val_types>
inline void fxt_duration_begin(
    const fxt::InternedCategory& category, uint64_t timestamp,
    const fxt::ThreadRef<thread_type>& thread_ref, const fxt::StringRef<name_type>& name_ref,
    const fxt::Argument<arg_types, arg_name_types, arg_val_types>&... args) {
  ktrace_thunks::fxt_duration_begin(category, timestamp, thread_ref, name_ref, args...);
}

template <fxt::RefType thread_type, fxt::RefType name_type, fxt::ArgumentType... arg_types,
          fxt::RefType... arg_name_types, fxt::RefType... arg_val_types>
inline void fxt_duration_end(
    const fxt::InternedCategory& category, uint64_t timestamp,
    const fxt::ThreadRef<thread_type>& thread_ref, const fxt::StringRef<name_type>& name_ref,
    const fxt::Argument<arg_types, arg_name_types, arg_val_types>&... args) {
  ktrace_thunks::fxt_duration_end(category, timestamp, thread_ref, name_ref, args...);
}

template <fxt::RefType thread_type, fxt::RefType name_type, fxt::ArgumentType... arg_types,
          fxt::RefType... arg_name_types, fxt::RefType... arg_val_types>
inline void fxt_duration_complete(
    const fxt::InternedCategory& category, uint64_t start_time,
    const fxt::ThreadRef<thread_type>& thread_ref, const fxt::StringRef<name_type>& name_ref,
    uint64_t end_time, const fxt::Argument<arg_types, arg_name_types, arg_val_types>&... args) {
  ktrace_thunks::fxt_duration_complete(category, start_time, thread_ref, name_ref, end_time,
                                       args...);
}

template <fxt::RefType thread_type, fxt::RefType name_type, fxt::ArgumentType... arg_types,
          fxt::RefType... arg_name_types, fxt::RefType... arg_val_types>
inline void fxt_counter(const fxt::InternedCategory& category, uint64_t timestamp,
                        const fxt::ThreadRef<thread_type>& thread_ref,
                        const fxt::StringRef<name_type>& name_ref, uint64_t counter_id,
                        const fxt::Argument<arg_types, arg_name_types, arg_val_types>&... args) {
  ktrace_thunks::fxt_counter(category, timestamp, thread_ref, name_ref, counter_id, args...);
}

template <fxt::RefType thread_type, fxt::RefType name_type, fxt::ArgumentType... arg_types,
          fxt::RefType... arg_name_types, fxt::RefType... arg_val_types>
inline void fxt_flow_begin(const fxt::InternedCategory& category, uint64_t timestamp,
                           const fxt::ThreadRef<thread_type>& thread_ref,
                           const fxt::StringRef<name_type>& name_ref, uint64_t flow_id,
                           const fxt::Argument<arg_types, arg_name_types, arg_val_types>&... args) {
  ktrace_thunks::fxt_flow_begin(category, timestamp, thread_ref, name_ref, flow_id, args...);
}

template <fxt::RefType thread_type, fxt::RefType name_type, fxt::ArgumentType... arg_types,
          fxt::RefType... arg_name_types, fxt::RefType... arg_val_types>
inline void fxt_flow_step(const fxt::InternedCategory& category, uint64_t timestamp,
                          const fxt::ThreadRef<thread_type>& thread_ref,
                          const fxt::StringRef<name_type>& name_ref, uint64_t flow_id,
                          const fxt::Argument<arg_types, arg_name_types, arg_val_types>&... args) {
  ktrace_thunks::fxt_flow_step(category, timestamp, thread_ref, name_ref, flow_id, args...);
}

template <fxt::RefType thread_type, fxt::RefType name_type, fxt::ArgumentType... arg_types,
          fxt::RefType... arg_name_types, fxt::RefType... arg_val_types>
inline void fxt_flow_end(const fxt::InternedCategory& category, uint64_t timestamp,
                         const fxt::ThreadRef<thread_type>& thread_ref,
                         const fxt::StringRef<name_type>& name_ref, uint64_t flow_id,
                         const fxt::Argument<arg_types, arg_name_types, arg_val_types>&... args) {
  ktrace_thunks::fxt_flow_end(category, timestamp, thread_ref, name_ref, flow_id, args...);
}

zx_status_t ktrace_control(uint32_t action, uint32_t options, void* ptr);

void ktrace_report_live_threads();
void ktrace_report_live_processes();

// RAII type that emits begin/end duration events covering the lifetime of the
// instance for use in tracing scopes.
// TODO(eieio): Add option to combine begin/end traces as a single complete
// event for better trace buffer efficiency.
template <typename Enabled, const fxt::InternedCategory& category,
          TraceContext = TraceContext::Thread>
class TraceDuration;

template <bool enabled, const fxt::InternedCategory& category, TraceContext context>
class TraceDuration<TraceEnabled<enabled>, category, context> {
 public:
  explicit TraceDuration(const fxt::InternedString& label) : label_{&label} {
    ktrace_begin_duration(TraceEnabled<enabled>{}, category, context, label);
  }
  TraceDuration(const fxt::InternedString& label, uint64_t a, uint64_t b) : label_{&label} {
    ktrace_begin_duration(TraceEnabled<enabled>{}, category, context, label, a, b);
  }

  ~TraceDuration() { End(); }

  TraceDuration(const TraceDuration&) = delete;
  TraceDuration& operator=(const TraceDuration&) = delete;
  TraceDuration(TraceDuration&&) = delete;
  TraceDuration& operator=(TraceDuration&&) = delete;

  // Emits the end trace early, before this instance destructs.
  void End() {
    if (label_) {
      ktrace_end_duration(TraceEnabled<enabled>{}, category, context, *label_);
      label_ = nullptr;
    }
  }
  // Similar to the overload above, taking the given arguments for the end
  // event.
  void End(uint64_t a, uint64_t b) {
    if (label_) {
      ktrace_end_duration(TraceEnabled<enabled>{}, category, context, *label_, a, b);
      label_ = nullptr;
    }
  }

  // Returns a callable to complete this duration trace. This is useful to
  // delegate closing the duration to a callee. The lifetime of the
  // TraceDuration instance must not end before the completer is invoked.
  auto Completer() {
    return [this]() { End(); };
  }

 private:
  const fxt::InternedString* label_;
};

#endif  // ZIRCON_KERNEL_INCLUDE_LIB_KTRACE_H_
