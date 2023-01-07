// Copyright 2021 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/ktrace.h>
#include <lib/ktrace/ktrace_internal.h>

#include <ktl/forward.h>

#include "lib/fxt/serializer.h"

// Fwd declaration of the global singleton KTraceState
extern internal::KTraceState KTRACE_STATE;

namespace ktrace_thunks {

bool category_enabled(const fxt::InternedCategory& category) {
  return KTRACE_STATE.IsCategoryEnabled(category);
}

ssize_t read_user(user_out_ptr<void> ptr, uint32_t off, size_t len) {
  return KTRACE_STATE.ReadUser(ptr, off, len);
}

template <fxt::RefType name_type, fxt::ArgumentType... arg_types, fxt::RefType... arg_name_types,
          fxt::RefType... arg_val_types>
void fxt_kernel_object(zx_koid_t koid, zx_obj_type_t obj_type,
                       const fxt::StringRef<name_type>& name_arg,
                       const fxt::Argument<arg_types, arg_name_types, arg_val_types>&... args) {
  fxt::WriteKernelObjectRecord(&KTRACE_STATE, fxt::Koid(koid), obj_type, name_arg, args...);
}

template <fxt::RefType outgoing_type, fxt::RefType incoming_type>
void fxt_context_switch(uint64_t timestamp, uint8_t cpu_number,
                        zx_thread_state_t outgoing_thread_state,
                        const fxt::ThreadRef<outgoing_type>& outgoing_thread,
                        const fxt::ThreadRef<incoming_type>& incoming_thread,
                        uint8_t outgoing_thread_priority, uint8_t incoming_thread_priority) {
  fxt::WriteContextSwitchRecord(&KTRACE_STATE, timestamp, cpu_number, outgoing_thread_state,
                                outgoing_thread, incoming_thread, outgoing_thread_priority,
                                incoming_thread_priority);
}

void fxt_string_record(uint16_t index, const char* string, size_t string_length) {
  fxt::WriteStringRecord(&KTRACE_STATE, index, string, string_length);
}

template <fxt::RefType thread_type, fxt::RefType name_type, fxt::ArgumentType... arg_types,
          fxt::RefType... arg_name_types, fxt::RefType... arg_val_types>
void fxt_instant(const fxt::InternedCategory& category, uint64_t timestamp,
                 const fxt::ThreadRef<thread_type>& thread_ref,
                 const fxt::StringRef<name_type>& name_ref,
                 const fxt::Argument<arg_types, arg_name_types, arg_val_types>&... args) {
  fxt::WriteInstantEventRecord(&KTRACE_STATE, timestamp, thread_ref, fxt::StringRef{category.label},
                               name_ref, args...);
}

template <fxt::RefType thread_type, fxt::RefType name_type, fxt::ArgumentType... arg_types,
          fxt::RefType... arg_name_types, fxt::RefType... arg_val_types>
void fxt_duration_begin(const fxt::InternedCategory& category, uint64_t timestamp,
                        const fxt::ThreadRef<thread_type>& thread_ref,
                        const fxt::StringRef<name_type>& name_ref,
                        const fxt::Argument<arg_types, arg_name_types, arg_val_types>&... args) {
  fxt::WriteDurationBeginEventRecord(&KTRACE_STATE, timestamp, thread_ref,
                                     fxt::StringRef{category.label}, name_ref, args...);
}

template <fxt::RefType thread_type, fxt::RefType name_type, fxt::ArgumentType... arg_types,
          fxt::RefType... arg_name_types, fxt::RefType... arg_val_types>
void fxt_duration_end(const fxt::InternedCategory& category, uint64_t timestamp,
                      const fxt::ThreadRef<thread_type>& thread_ref,
                      const fxt::StringRef<name_type>& name_ref,
                      const fxt::Argument<arg_types, arg_name_types, arg_val_types>&... args) {
  fxt::WriteDurationEndEventRecord(&KTRACE_STATE, timestamp, thread_ref,
                                   fxt::StringRef{category.label}, name_ref, args...);
}

template <fxt::RefType thread_type, fxt::RefType name_type, fxt::ArgumentType... arg_types,
          fxt::RefType... arg_name_types, fxt::RefType... arg_val_types>
void fxt_duration_complete(const fxt::InternedCategory& category, uint64_t start,
                           const fxt::ThreadRef<thread_type>& thread_ref,
                           const fxt::StringRef<name_type>& name_ref, uint64_t end,
                           const fxt::Argument<arg_types, arg_name_types, arg_val_types>&... args) {
  fxt::WriteDurationCompleteEventRecord(&KTRACE_STATE, start, thread_ref,
                                        fxt::StringRef{category.label}, name_ref, end, args...);
}

template <fxt::RefType thread_type, fxt::RefType name_type, fxt::ArgumentType... arg_types,
          fxt::RefType... arg_name_types, fxt::RefType... arg_val_types>
void fxt_counter(const fxt::InternedCategory& category, uint64_t timestamp,
                 const fxt::ThreadRef<thread_type>& thread_ref,
                 const fxt::StringRef<name_type>& name_ref, uint64_t counter_id,
                 const fxt::Argument<arg_types, arg_name_types, arg_val_types>&... args) {
  fxt::WriteCounterEventRecord(&KTRACE_STATE, timestamp, thread_ref, fxt::StringRef{category.label},
                               name_ref, counter_id, args...);
}

template <fxt::RefType thread_type, fxt::RefType name_type, fxt::ArgumentType... arg_types,
          fxt::RefType... arg_name_types, fxt::RefType... arg_val_types>
void fxt_flow_begin(const fxt::InternedCategory& category, uint64_t timestamp,
                    const fxt::ThreadRef<thread_type>& thread_ref,
                    const fxt::StringRef<name_type>& name_ref, uint64_t flow_id,
                    const fxt::Argument<arg_types, arg_name_types, arg_val_types>&... args) {
  fxt::WriteFlowBeginEventRecord(&KTRACE_STATE, timestamp, thread_ref,
                                 fxt::StringRef{category.label}, name_ref, flow_id, args...);
}

template <fxt::RefType thread_type, fxt::RefType name_type, fxt::ArgumentType... arg_types,
          fxt::RefType... arg_name_types, fxt::RefType... arg_val_types>
void fxt_flow_step(const fxt::InternedCategory& category, uint64_t timestamp,
                   const fxt::ThreadRef<thread_type>& thread_ref,
                   const fxt::StringRef<name_type>& name_ref, uint64_t flow_id,
                   const fxt::Argument<arg_types, arg_name_types, arg_val_types>&... args) {
  fxt::WriteFlowStepEventRecord(&KTRACE_STATE, timestamp, thread_ref,
                                fxt::StringRef{category.label}, name_ref, flow_id, args...);
}

template <fxt::RefType thread_type, fxt::RefType name_type, fxt::ArgumentType... arg_types,
          fxt::RefType... arg_name_types, fxt::RefType... arg_val_types>
void fxt_flow_end(const fxt::InternedCategory& category, uint64_t timestamp,
                  const fxt::ThreadRef<thread_type>& thread_ref,
                  const fxt::StringRef<name_type>& name_ref, uint64_t flow_id,
                  const fxt::Argument<arg_types, arg_name_types, arg_val_types>&... args) {
  fxt::WriteFlowEndEventRecord(&KTRACE_STATE, timestamp, thread_ref, fxt::StringRef{category.label},
                               name_ref, flow_id, args...);
}

template void fxt_kernel_object(zx_koid_t koid, zx_obj_type_t obj_type,
                                const fxt::StringRef<fxt::RefType::kInline>& name_arg);
template void fxt_kernel_object(zx_koid_t koid, zx_obj_type_t obj_type,
                                const fxt::StringRef<fxt::RefType::kId>& name_arg);
template void fxt_kernel_object(zx_koid_t koid, zx_obj_type_t obj_type,
                                const fxt::StringRef<fxt::RefType::kInline>& name_arg,
                                const fxt::Argument<fxt::ArgumentType::kKoid, fxt::RefType::kId>&);

template void fxt_instant(const fxt::InternedCategory& category, uint64_t timestamp,
                          const fxt::ThreadRef<fxt::RefType::kInline>& thread_ref,
                          const fxt::StringRef<fxt::RefType::kId>& name_ref);
template void fxt_instant(
    const fxt::InternedCategory& category, uint64_t timestamp,
    const fxt::ThreadRef<fxt::RefType::kInline>& thread_ref,
    const fxt::StringRef<fxt::RefType::kId>& name_ref,
    const fxt::Argument<fxt::ArgumentType::kUint32, fxt::RefType::kId, fxt::RefType::kId>& arg);
template void fxt_instant(
    const fxt::InternedCategory& category, uint64_t timestamp,
    const fxt::ThreadRef<fxt::RefType::kInline>& thread_ref,
    const fxt::StringRef<fxt::RefType::kId>& name_ref,
    const fxt::Argument<fxt::ArgumentType::kUint32, fxt::RefType::kId, fxt::RefType::kId>& arg1,
    const fxt::Argument<fxt::ArgumentType::kUint32, fxt::RefType::kId, fxt::RefType::kId>& arg2);
template void fxt_instant(
    const fxt::InternedCategory& category, uint64_t timestamp,
    const fxt::ThreadRef<fxt::RefType::kInline>& thread_ref,
    const fxt::StringRef<fxt::RefType::kId>& name_ref,
    const fxt::Argument<fxt::ArgumentType::kUint64, fxt::RefType::kId, fxt::RefType::kId>& arg1,
    const fxt::Argument<fxt::ArgumentType::kUint64, fxt::RefType::kId, fxt::RefType::kId>& arg2);
template void fxt_duration_begin(const fxt::InternedCategory& category, uint64_t timestamp,
                                 const fxt::ThreadRef<fxt::RefType::kInline>& thread_ref,
                                 const fxt::StringRef<fxt::RefType::kId>& name_ref);
template void fxt_duration_begin(
    const fxt::InternedCategory& category, uint64_t timestamp,
    const fxt::ThreadRef<fxt::RefType::kInline>& thread_ref,
    const fxt::StringRef<fxt::RefType::kId>& name_ref,
    const fxt::Argument<fxt::ArgumentType::kInt32, fxt::RefType::kId, fxt::RefType::kId>& arg);
template void fxt_duration_begin(
    const fxt::InternedCategory& category, uint64_t timestamp,
    const fxt::ThreadRef<fxt::RefType::kInline>& thread_ref,
    const fxt::StringRef<fxt::RefType::kId>& name_ref,
    const fxt::Argument<fxt::ArgumentType::kUint32, fxt::RefType::kId, fxt::RefType::kId>& arg);
template void fxt_duration_begin(
    const fxt::InternedCategory& category, uint64_t timestamp,
    const fxt::ThreadRef<fxt::RefType::kInline>& thread_ref,
    const fxt::StringRef<fxt::RefType::kId>& name_ref,
    const fxt::Argument<fxt::ArgumentType::kUint64, fxt::RefType::kId, fxt::RefType::kId>& arg);
template void fxt_duration_begin(
    const fxt::InternedCategory& category, uint64_t timestamp,
    const fxt::ThreadRef<fxt::RefType::kInline>& thread_ref,
    const fxt::StringRef<fxt::RefType::kId>& name_ref,
    const fxt::Argument<fxt::ArgumentType::kUint32, fxt::RefType::kId, fxt::RefType::kId>& arg1,
    const fxt::Argument<fxt::ArgumentType::kUint32, fxt::RefType::kId, fxt::RefType::kId>& arg2);
template void fxt_duration_begin(
    const fxt::InternedCategory& category, uint64_t timestamp,
    const fxt::ThreadRef<fxt::RefType::kInline>& thread_ref,
    const fxt::StringRef<fxt::RefType::kId>& name_ref,
    const fxt::Argument<fxt::ArgumentType::kUint64, fxt::RefType::kId, fxt::RefType::kId>& arg1,
    const fxt::Argument<fxt::ArgumentType::kUint32, fxt::RefType::kId, fxt::RefType::kId>& arg2);
template void fxt_duration_begin(
    const fxt::InternedCategory& category, uint64_t timestamp,
    const fxt::ThreadRef<fxt::RefType::kInline>& thread_ref,
    const fxt::StringRef<fxt::RefType::kId>& name_ref,
    const fxt::Argument<fxt::ArgumentType::kUint64, fxt::RefType::kId, fxt::RefType::kId>& arg1,
    const fxt::Argument<fxt::ArgumentType::kUint64, fxt::RefType::kId, fxt::RefType::kId>& arg2);

template void fxt_duration_end(const fxt::InternedCategory& category, uint64_t timestamp,
                               const fxt::ThreadRef<fxt::RefType::kInline>& thread_ref,
                               const fxt::StringRef<fxt::RefType::kId>& name_ref);
template void fxt_duration_end(
    const fxt::InternedCategory& category, uint64_t timestamp,
    const fxt::ThreadRef<fxt::RefType::kInline>& thread_ref,
    const fxt::StringRef<fxt::RefType::kId>& name_ref,
    const fxt::Argument<fxt::ArgumentType::kInt32, fxt::RefType::kId, fxt::RefType::kId>& arg);
template void fxt_duration_end(
    const fxt::InternedCategory& category, uint64_t timestamp,
    const fxt::ThreadRef<fxt::RefType::kInline>& thread_ref,
    const fxt::StringRef<fxt::RefType::kId>& name_ref,
    const fxt::Argument<fxt::ArgumentType::kUint32, fxt::RefType::kId, fxt::RefType::kId>& arg);
template void fxt_duration_end(
    const fxt::InternedCategory& category, uint64_t timestamp,
    const fxt::ThreadRef<fxt::RefType::kInline>& thread_ref,
    const fxt::StringRef<fxt::RefType::kId>& name_ref,
    const fxt::Argument<fxt::ArgumentType::kUint64, fxt::RefType::kId, fxt::RefType::kId>& arg);
template void fxt_duration_end(
    const fxt::InternedCategory& category, uint64_t timestamp,
    const fxt::ThreadRef<fxt::RefType::kInline>& thread_ref,
    const fxt::StringRef<fxt::RefType::kId>& name_ref,
    const fxt::Argument<fxt::ArgumentType::kUint64, fxt::RefType::kId, fxt::RefType::kId>& arg1,
    const fxt::Argument<fxt::ArgumentType::kString, fxt::RefType::kId, fxt::RefType::kId>& arg2);
template void fxt_duration_end(
    const fxt::InternedCategory& category, uint64_t timestamp,
    const fxt::ThreadRef<fxt::RefType::kInline>& thread_ref,
    const fxt::StringRef<fxt::RefType::kId>& name_ref,
    const fxt::Argument<fxt::ArgumentType::kUint64, fxt::RefType::kId, fxt::RefType::kId>& arg1,
    const fxt::Argument<fxt::ArgumentType::kUint32, fxt::RefType::kId, fxt::RefType::kId>& arg2);
template void fxt_duration_end(
    const fxt::InternedCategory& category, uint64_t timestamp,
    const fxt::ThreadRef<fxt::RefType::kInline>& thread_ref,
    const fxt::StringRef<fxt::RefType::kId>& name_ref,
    const fxt::Argument<fxt::ArgumentType::kUint64, fxt::RefType::kId, fxt::RefType::kId>& arg1,
    const fxt::Argument<fxt::ArgumentType::kUint64, fxt::RefType::kId, fxt::RefType::kId>& arg2);
template void fxt_duration_complete(const fxt::InternedCategory& category, uint64_t start_time,
                                    const fxt::ThreadRef<fxt::RefType::kInline>& thread_ref,
                                    const fxt::StringRef<fxt::RefType::kId>& name_ref,
                                    uint64_t end_time);
template void fxt_duration_complete(
    const fxt::InternedCategory& category, uint64_t start_time,
    const fxt::ThreadRef<fxt::RefType::kInline>& thread_ref,
    const fxt::StringRef<fxt::RefType::kId>& name_ref, uint64_t end_time,
    const fxt::Argument<fxt::ArgumentType::kUint64, fxt::RefType::kId, fxt::RefType::kId>& arg1,
    const fxt::Argument<fxt::ArgumentType::kUint32, fxt::RefType::kId, fxt::RefType::kId>& arg2);
template void fxt_duration_complete(
    const fxt::InternedCategory& category, uint64_t start_time,
    const fxt::ThreadRef<fxt::RefType::kInline>& thread_ref,
    const fxt::StringRef<fxt::RefType::kId>& name_ref, uint64_t end_time,
    const fxt::Argument<fxt::ArgumentType::kUint64, fxt::RefType::kId, fxt::RefType::kId>& arg1);
template void fxt_duration_complete(
    const fxt::InternedCategory& category, uint64_t start_time,
    const fxt::ThreadRef<fxt::RefType::kInline>& thread_ref,
    const fxt::StringRef<fxt::RefType::kId>& name_ref, uint64_t end_time,
    const fxt::Argument<fxt::ArgumentType::kUint32, fxt::RefType::kId, fxt::RefType::kId>& arg1,
    const fxt::Argument<fxt::ArgumentType::kUint32, fxt::RefType::kId, fxt::RefType::kId>& arg2,
    const fxt::Argument<fxt::ArgumentType::kUint32, fxt::RefType::kId, fxt::RefType::kId>& arg3,
    const fxt::Argument<fxt::ArgumentType::kUint32, fxt::RefType::kId, fxt::RefType::kId>& arg4);

template void fxt_counter(const fxt::InternedCategory& category, uint64_t timestamp,
                          const fxt::ThreadRef<fxt::RefType::kInline>& thread_ref,
                          const fxt::StringRef<fxt::RefType::kId>& name_ref, uint64_t counter_id);
template void fxt_counter(
    const fxt::InternedCategory& category, uint64_t timestamp,
    const fxt::ThreadRef<fxt::RefType::kInline>& thread_ref,
    const fxt::StringRef<fxt::RefType::kId>& name_ref, uint64_t counter_id,
    const fxt::Argument<fxt::ArgumentType::kInt64, fxt::RefType::kId, fxt::RefType::kId>& arg4);
template void fxt_counter(
    const fxt::InternedCategory& category, uint64_t timestamp,
    const fxt::ThreadRef<fxt::RefType::kInline>& thread_ref,
    const fxt::StringRef<fxt::RefType::kId>& name_ref, uint64_t counter_id,
    const fxt::Argument<fxt::ArgumentType::kUint64, fxt::RefType::kId, fxt::RefType::kId>& arg);

template void fxt_flow_begin(const fxt::InternedCategory& category, uint64_t timestamp,
                             const fxt::ThreadRef<fxt::RefType::kInline>& thread_ref,
                             const fxt::StringRef<fxt::RefType::kId>& name_ref, uint64_t flow_id);
template void fxt_flow_begin(
    const fxt::InternedCategory& category, uint64_t timestamp,
    const fxt::ThreadRef<fxt::RefType::kInline>& thread_ref,
    const fxt::StringRef<fxt::RefType::kId>& name_ref, uint64_t flow_id,
    const fxt::Argument<fxt::ArgumentType::kUint64, fxt::RefType::kId, fxt::RefType::kId>& arg);
template void fxt_flow_step(const fxt::InternedCategory& category, uint64_t timestamp,
                            const fxt::ThreadRef<fxt::RefType::kInline>& thread_ref,
                            const fxt::StringRef<fxt::RefType::kId>& name_ref, uint64_t flow_id);
template void fxt_flow_step(
    const fxt::InternedCategory& category, uint64_t timestamp,
    const fxt::ThreadRef<fxt::RefType::kInline>& thread_ref,
    const fxt::StringRef<fxt::RefType::kId>& name_ref, uint64_t flow_id,
    const fxt::Argument<fxt::ArgumentType::kUint64, fxt::RefType::kId, fxt::RefType::kId>& arg);
template void fxt_flow_end(const fxt::InternedCategory& category, uint64_t timestamp,
                           const fxt::ThreadRef<fxt::RefType::kInline>& thread_ref,
                           const fxt::StringRef<fxt::RefType::kId>& name_ref, uint64_t flow_id);
template void fxt_flow_end(
    const fxt::InternedCategory& category, uint64_t timestamp,
    const fxt::ThreadRef<fxt::RefType::kInline>& thread_ref,
    const fxt::StringRef<fxt::RefType::kId>& name_ref, uint64_t flow_id,
    const fxt::Argument<fxt::ArgumentType::kUint64, fxt::RefType::kId, fxt::RefType::kId>& arg);

template void fxt_context_switch(uint64_t timestamp, uint8_t cpu_number,
                                 zx_thread_state_t outgoing_thread_state,
                                 const fxt::ThreadRef<fxt::RefType::kInline>& outgoing_thread,
                                 const fxt::ThreadRef<fxt::RefType::kInline>& incoming_thread,
                                 uint8_t outgoing_thread_priority,
                                 uint8_t incoming_thread_priority);
}  // namespace ktrace_thunks
