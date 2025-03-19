// Copyright 2016 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_INCLUDE_LIB_KTRACE_H_
#define ZIRCON_KERNEL_INCLUDE_LIB_KTRACE_H_

#include <lib/fxt/interned_category.h>
#include <lib/fxt/serializer.h>
#include <lib/fxt/trace_base.h>
#include <lib/ktrace/ktrace_internal.h>
#include <lib/user_copy/user_ptr.h>
#include <lib/zircon-internal/ktrace.h>
#include <platform.h>
#include <zircon/compiler.h>
#include <zircon/types.h>

#include <kernel/thread.h>
#include <ktl/atomic.h>
#include <ktl/tuple.h>

//
// # Kernel tracing instrumentation and state management interfaces
//
// The general tracing API is comprised of macros appropriate for most use cases. Direct access to
// the lower level interfaces is available for specialized use cases.
//

//
// # General kernel tracing API
//
// The following macros comprise the general kernel tracing interface. The interface supports most
// FXT record types and provides options for compile-time and runtime predicates to manage the
// presence of instrumentation in different builds.
//

//
// ## Variable arguments
//
// Most FXT records support a variable number of arguments as string/value pairs. The macros in this
// interface expect variable arguments to be parenthesized tuples of the form:
//
//   KTRACE_*(<required args>, ("literal", value, ...), ("literal", value, ...), ...);
//
// These tuples are transformed into instances of fxt::Argument as follows:
//
//   ("key", 10)        -> fxt::MakeArgument("key"_intern, 10)
//   ("key", buf, size) -> fxt::MakeArgument("key"_intern, buf, size)
//
// ## Argument evaluation
//
// Conditions and arguments to KTRACE_* macros are evaluated in the following order and scope. When
// a condition is not exposed by a particular macro it is hard-coded to true unless otherwise
// specified.
//
// 1. constexpr_enabled: Evaluated once with all remaining argument and condition evaluations
//    contained in the true branch.
// 2. runtime_condition: Evaluated once with all remaining argument and condition evaluations
//    contained in the true branch.
// 3. Category predicate: Evaluated once with all remaining argument evaluations contained in the
//    true branch.
// 4. Required and variable arguments: Evaluated as function arguments using standard C++
//    implementation defined evaluation order when all preceding conditions are met. Argument
//    expressions do not need to be globally idempotent (e.g. they can generate ids), however,
//    inter-argument order dependencies must be handled with the same care as in any ordinary
//    function invocation.
//

//
// ## KERNEL_OBJECT
//
// Supports exporting information about kernel objects. For example, the names of threads and
// processes.
//

// Writes a kernel object record when the given trace category is enabled.
//
// Arguments:
// - category: String literal filter category for the object record.
// - koid: Kernel object id of the object. Expects type zx_koid_t.
// - obj_type: The type the object. Expects type zx_obj_type_t.
// - name: The name of the object. Must be convertible to fxt::StringRef.
// - ...: List of parenthesized argument tuples.
//
#define KTRACE_KERNEL_OBJECT(category, koid, obj_type, name, ...)                            \
  FXT_KERNEL_OBJECT(true, KTrace::CategoryEnabled, KTrace::EmitKernelObject, category, koid, \
                    obj_type, name, ##__VA_ARGS__)

// Writes a kernel object record unconditionally. Useful for generating the initial set of object
// info records before tracing is enabled.
//
// Arguments:
// - koid: Kernel object id of the object. Expects type zx_koid_t.
// - obj_type: The type the object. Expects type zx_obj_type_t.
// - name: The name of the object. Must be convertible to fxt::StringRef.
// - ...: List of parenthesized argument tuples.
//
#define KTRACE_KERNEL_OBJECT_ALWAYS(koid, obj_type, name, ...) \
  FXT_KERNEL_OBJECT_ALWAYS(true, KTrace::EmitKernelObject, koid, obj_type, name, ##__VA_ARGS__)

//
// ## SCOPE
//
// Supports tracing the duration of lexical scopes, such as functions, conditional bodies, and
// nested scopes using duration complete events.
//
// The duration of a scope is determined by the lifetime of the RAII helper type ktrace::Scope. The
// category, label, and arguments for the complete event are captured by delegates created by
// KTRACE_BEGIN_SCOPE* and KTRACE_END_SCOPE* macros.
//
// Examples:
//
//  // Basic scope with arguments.
//  void Function(int value) {
//    ktrace::Scope trace = KTRACE_BEGIN_SCOPE("category", ("value", value));
//    // ...
//  }
//
//  // Early completion of the scope.
//  void Function(int value) {
//    ktrace::Scope trace = KTRACE_BEGIN_SCOPE("category", ("value", value));
//    // ...
//    scope.End();
//    // ...
//  }
//
//  // Completion of the scope with additional arguments.
//  void Function(int value) {
//    ktrace::Scope trace = KTRACE_BEGIN_SCOPE("category", ("value", value));
//    const int result = GetResult();
//    scope = KTRACE_END_SCOPE(("result", result));
//  }
//
//  // Runtime predicate to enable the scope only in certain conditions.
//  void Function(int value) {
//    ktrace::Scope trace = KTRACE_BEGIN_SCOPE_COND(Predicate(value), "category", ("value", value));
//    const int result = GetResult();
//    scope = KTRACE_END_SCOPE(("result", result));
//  }
//

// Creates a delegate to capture the given arguments at the beginning of a scope when the given
// category is enabled. The returned value should be used to construct a ktrace::Scope to track the
// lifetime of the scope and emit the complete trace event. The complete event is associated with
// the current thread.
//
// Arguments:
// - category: Filter category for the event. Expects a string literal.
// - label: Label for the event. Expects a string literal.
// - ...: List of parenthesized argument tuples.
//
#define KTRACE_BEGIN_SCOPE(category, label, ...)                                                \
  FXT_BEGIN_SCOPE(true, true, KTrace::CategoryEnabled, KTrace::Timestamp, KTrace::EmitComplete, \
                  category, FXT_INTERN_STRING(label), TraceContext::Thread, ##__VA_ARGS__)

// Similar to KTRACE_BEGIN_SCOPE, but associates the event with the current CPU instead of the
// current thread.
#define KTRACE_CPU_BEGIN_SCOPE(category, label, ...)                                            \
  FXT_BEGIN_SCOPE(true, true, KTrace::CategoryEnabled, KTrace::Timestamp, KTrace::EmitComplete, \
                  category, FXT_INTERN_STRING(label), TraceContext::Cpu, ##__VA_ARGS__)

// Similar to KTRACE_BEGIN_SCOPE, but checks the given runtime_condition, in addition to the given
// category, to determine whether to emit the event.
#define KTRACE_BEGIN_SCOPE_COND(runtime_condition, category, label, ...)                          \
  FXT_BEGIN_SCOPE(true, runtime_condition, KTrace::CategoryEnabled, KTrace::Timestamp,            \
                  KTrace::EmitComplete, category, FXT_INTERN_STRING(label), TraceContext::Thread, \
                  ##__VA_ARGS__)

// Similar to KTRACE_BEGIN_SCOPE_COND, but associates the event with the current CPU instead of the
// current thread.
#define KTRACE_CPU_BEGIN_SCOPE_COND(runtime_condition, category, label, ...)                   \
  FXT_BEGIN_SCOPE(true, runtime_condition, KTrace::CategoryEnabled, KTrace::Timestamp,         \
                  KTrace::EmitComplete, category, FXT_INTERN_STRING(label), TraceContext::Cpu, \
                  ##__VA_ARGS__)

// Similar to KTRACE_BEGIN_SCOPE, but checks the given constexpr_condition to determine whether the
// event is enabled at compile time.
#define KTRACE_BEGIN_SCOPE_ENABLE(constexpr_enabled, category, label, ...)                        \
  FXT_BEGIN_SCOPE(constexpr_enabled, true, KTrace::CategoryEnabled, KTrace::Timestamp,            \
                  KTrace::EmitComplete, category, FXT_INTERN_STRING(label), TraceContext::Thread, \
                  ##__VA_ARGS__)

// Similar to KTRACE_BEGIN_SCOPE_ENABLE, but associates the event with the current CPU instead of
// the current thread.
#define KTRACE_CPU_BEGIN_SCOPE_ENABLE(constexpr_enabled, category, label, ...)                 \
  FXT_BEGIN_SCOPE(constexpr_enabled, true, KTrace::CategoryEnabled, KTrace::Timestamp,         \
                  KTrace::EmitComplete, category, FXT_INTERN_STRING(label), TraceContext::Cpu, \
                  ##__VA_ARGS__)

// Similar to KTRACE_BEGIN_SCOPE, but checks the given constexpr_condition to determine whether the
// event is enabled at compile time, and checks the given runtime_condition, in addition to the
// given category, to determine whether to emit the event.
#define KTRACE_BEGIN_SCOPE_ENABLE_COND(constexpr_enabled, runtime_enabled, category, label, ...)  \
  FXT_BEGIN_SCOPE(constexpr_enabled, runtime_enabled, KTrace::CategoryEnabled, KTrace::Timestamp, \
                  KTrace::EmitComplete, category, FXT_INTERN_STRING(label), TraceContext::Thread, \
                  ##__VA_ARGS__)

// Similar to KTRACE_BEGIN_SCOPE_ENABLE_COND, but associates the event with the current CPU instead
// of the current thread.
#define KTRACE_CPU_BEGIN_SCOPE_ENABLE_COND(constexpr_enabled, runtime_enabled, category, label,   \
                                           ...)                                                   \
  FXT_BEGIN_SCOPE(constexpr_enabled, runtime_enabled, KTrace::CategoryEnabled, KTrace::Timestamp, \
                  KTrace::EmitComplete, category, FXT_INTERN_STRING(label), TraceContext::Cpu,    \
                  ##__VA_ARGS__)

// Creates a delegate to capture the given arguments at the end of an active scope. The returned
// value should be assigned to the scope to complete with additional arguments.
#define KTRACE_END_SCOPE(...) FXT_END_SCOPE(__VA_ARGS__)

// Utility to access values with Clang static analysis annotations in a scoped argument list.

// Due to the way arguments are captured by lambdas when using KTRACE_BEGIN_SCOPE and
// KTRACE_END_SCOPE, it is necessary to hoist static assertions or lock guards into the argument
// capture expressions using KTRACE_ANNOTATED_VALUE.
//
// Examples:
//
// - Lock is already held and it is safe to read the guarded value:
//
//  ktrace::Scope scope =
//      KTRACE_BEGIN_SCOPE("category", "label",
//          ("guarded_value", KTRACE_ANNOTATED_VALUE(AssertHeld(lock), guarded_value)));
//
// - Lock is not held and must be acquired to read the guarded value:
//
//  ktrace::Scope scope =
//      KTRACE_BEGIN_SCOPE("category", "label",
//          ("guarded_value", KTRACE_ANNOTATED_VALUE(Guard guard{lock}, guarded_value)));
//
#define KTRACE_ANNOTATED_VALUE(acquire_or_assert_expression, value_expression) \
  FXT_ANNOTATED_VALUE(acquire_or_assert_expression, value_expression)

//
// ## INSTANT
//

// Writes an instant event associated with the current thread when the given category is enabled.
//
// Arguments:
// - category: Filter category for the event. Expects a string literal.
// - label: Label for the event. Expects a string literal.
// - ...: List of parenthesized argument tuples.
//
#define KTRACE_INSTANT(category, label, ...)                                            \
  FXT_EVENT_COMMON(true, KTrace::CategoryEnabled, KTrace::EmitInstant, category,        \
                   FXT_INTERN_STRING(label), KTrace::Timestamp(), TraceContext::Thread, \
                   ktrace::Unused{}, ##__VA_ARGS__)

// Similar to KTRACE_INSTANT, but associates the event with the current CPU instead of the current
// thread.
#define KTRACE_CPU_INSTANT(category, label, ...)                                     \
  FXT_EVENT_COMMON(true, KTrace::CategoryEnabled, KTrace::EmitInstant, category,     \
                   FXT_INTERN_STRING(label), KTrace::Timestamp(), TraceContext::Cpu, \
                   ktrace::Unused{}, ##__VA_ARGS__)

// Similar to KTRACE_INSTANT, but checks the given constexpr_condition to determine whether the
// event is enabled at compile time.
#define KTRACE_INSTANT_ENABLE(constexpr_enabled, category, label, ...)                        \
  FXT_EVENT_COMMON(constexpr_enabled, KTrace::CategoryEnabled, KTrace::EmitInstant, category, \
                   FXT_INTERN_STRING(label), KTrace::Timestamp(), TraceContext::Thread,       \
                   ktrace::Unused{}, ##__VA_ARGS__)

// Similar to KTRACE_INSTANT_ENABLE, but associates the event with the current CPU instead of the
// current thread.
#define KTRACE_CPU_INSTANT_ENABLE(constexpr_enabled, category, label, ...)                    \
  FXT_EVENT_COMMON(constexpr_enabled, KTrace::CategoryEnabled, KTrace::EmitInstant, category, \
                   FXT_INTERN_STRING(label), KTrace::Timestamp(), TraceContext::Cpu,          \
                   ktrace::Unused{}, ##__VA_ARGS__)

//
// ## DURATION_BEGIN
//

// Writes a duration begin event associated with the current thread when the given category is
// enabled.
//
// Arguments:
// - category: Filter category for the event. Expects a string literal.
// - label: Label for the event. Expects a string literal.
// - ...: List of parenthesized argument tuples.
//
#define KTRACE_DURATION_BEGIN(category, label, ...)                                     \
  FXT_EVENT_COMMON(true, KTrace::CategoryEnabled, KTrace::EmitDurationBegin, category,  \
                   FXT_INTERN_STRING(label), KTrace::Timestamp(), TraceContext::Thread, \
                   ktrace::Unused{}, ##__VA_ARGS__)

// Similar to KTRACE_DURATION_BEGIN, but associates the event with the current CPU instead of the
// current thread.
#define KTRACE_CPU_DURATION_BEGIN(category, label, ...)                                \
  FXT_EVENT_COMMON(true, KTrace::CategoryEnabled, KTrace::EmitDurationBegin, category, \
                   FXT_INTERN_STRING(label), KTrace::Timestamp(), TraceContext::Cpu,   \
                   ktrace::Unused{}, ##__VA_ARGS__)

// Similar to KTRACE_DURATION_BEGIN, but checks the given constexpr_condition to determine whether
// the event is enabled at compile time.
#define KTRACE_DURATION_BEGIN_ENABLE(constexpr_enabled, category, label, ...)                     \
  FXT_EVENT_COMMON(constexpr_enabled, KTrace::CategoryEnabled, KTrace::EmitDurationBegin,         \
                   category, FXT_INTERN_STRING(label), KTrace::Timestamp(), TraceContext::Thread, \
                   ktrace::Unused{}, ##__VA_ARGS__)

// Similar to KTRACE_DURATION_BEGIN_ENABLE, but associates the event with the current CPU instead of
// the current thread.
#define KTRACE_CPU_DURATION_BEGIN_ENABLE(constexpr_enabled, category, label, ...)              \
  FXT_EVENT_COMMON(constexpr_enabled, KTrace::CategoryEnabled, KTrace::EmitDurationBegin,      \
                   category, FXT_INTERN_STRING(label), KTrace::Timestamp(), TraceContext::Cpu, \
                   ktrace::Unused{}, ##__VA_ARGS__)

// Similar to KTRACE_DURATION_BEGIN, but accepts a value convertible to fxt::StringRef for the
// label. Useful to tracing durations where the label comes from a table (e.g. syscall, vmm).
#define KTRACE_DURATION_BEGIN_LABEL_REF(category, label_ref, ...)                                 \
  FXT_EVENT_COMMON(true, KTrace::CategoryEnabled, KTrace::EmitDurationBegin, category, label_ref, \
                   KTrace::Timestamp(), TraceContext::Thread, ktrace::Unused{}, ##__VA_ARGS__)

// Similar to KTRACE_DURATION_BEGIN, but accepts an expression to use for the event timestamp.
#define KTRACE_DURATION_BEGIN_TIMESTAMP(category, label, timestamp, ...)                        \
  FXT_EVENT_COMMON(true, KTrace::CategoryEnabled, KTrace::EmitDurationBegin, category,          \
                   FXT_INTERN_STRING(label), timestamp, TraceContext::Thread, ktrace::Unused{}, \
                   ##__VA_ARGS__)

//
// ## DURATION_END
//

// Writes a duration end event associated with the current thread when the given category is
// enabled.
//
// Arguments:
// - category: Filter category for the event. Expects a string literal.
// - label: Label for the event. Expects a string literal.
// - ...: List of parenthesized argument tuples.
//
#define KTRACE_DURATION_END(category, label, ...)                                       \
  FXT_EVENT_COMMON(true, KTrace::CategoryEnabled, KTrace::EmitDurationEnd, category,    \
                   FXT_INTERN_STRING(label), KTrace::Timestamp(), TraceContext::Thread, \
                   ktrace::Unused{}, ##__VA_ARGS__)

// Similar to KTRACE_DURATION_END, but associates the event with the current CPU instead of the
// current thread.
#define KTRACE_CPU_DURATION_END(category, label, ...)                                \
  FXT_EVENT_COMMON(true, KTrace::CategoryEnabled, KTrace::EmitDurationEnd, category, \
                   FXT_INTERN_STRING(label), KTrace::Timestamp(), TraceContext::Cpu, \
                   ktrace::Unused{}, ##__VA_ARGS__)

// Similar to KTRACE_DURATION_END, but checks the given constexpr_condition to determine whether the
// event is enabled at compile time.
#define KTRACE_DURATION_END_ENABLE(constexpr_enabled, category, label, ...)                       \
  FXT_EVENT_COMMON(constexpr_enabled, KTrace::CategoryEnabled, KTrace::EmitDurationEnd, category, \
                   FXT_INTERN_STRING(label), KTrace::Timestamp(), TraceContext::Thread,           \
                   ktrace::Unused{}, ##__VA_ARGS__)

// Similar to KTRACE_DURATION_END_ENABLE, but associates the event with the current CPU instead of
// the current thread.
#define KTRACE_CPU_DURATION_END_ENABLE(constexpr_enabled, category, label, ...)                   \
  FXT_EVENT_COMMON(constexpr_enabled, KTrace::CategoryEnabled, KTrace::EmitDurationEnd, category, \
                   FXT_INTERN_STRING(label), KTrace::Timestamp(), TraceContext::Cpu,              \
                   ktrace::Unused{}, ##__VA_ARGS__)

// Similar to KTRACE_DURATION_END, but accepts a value convertible to fxt::StringRef for the label.
// Useful to tracing durations where the label comes from a table (e.g. syscall, vmm).
#define KTRACE_DURATION_END_LABEL_REF(category, label, ...)                                 \
  FXT_EVENT_COMMON(true, KTrace::CategoryEnabled, KTrace::EmitDurationEnd, category, label, \
                   KTrace::Timestamp(), TraceContext::Thread, ktrace::Unused{}, ##__VA_ARGS__)

// Similar to KTRACE_DURATION_END, but accepts an expression to use for the event timestamp.
#define KTRACE_DURATION_END_TIMESTAMP(category, label, timestamp, ...)                          \
  FXT_EVENT_COMMON(true, KTrace::CategoryEnabled, KTrace::EmitDurationEnd, category,            \
                   FXT_INTERN_STRING(label), timestamp, TraceContext::Thread, ktrace::Unused{}, \
                   ##__VA_ARGS__)

//
// ## COMPLETE
//

// Writes a duration complete event associated with the current thread when the given category is
// enabled.
//
// Arguments:
// - category: Filter category for the event. Expects a string literal.
// - label: Label for the event. Expects a string literal.
// - start_timestamp: The starting timestamp for the event. Must be convertible to uint64_t.
// - ...: List of parenthesized argument tuples.
//
#define KTRACE_COMPLETE(category, label, start_timestamp, ...)                     \
  FXT_EVENT_COMMON(true, KTrace::CategoryEnabled, KTrace::EmitComplete, category,  \
                   FXT_INTERN_STRING(label), start_timestamp, KTrace::Timestamp(), \
                   TraceContext::Thread, ##__VA_ARGS__)

// Similar to KTRACE_COMPLETE, but associates the event with the current CPU instead of the current
// thread.
#define KTRACE_CPU_COMPLETE(category, label, start_timestamp, ...)                 \
  FXT_EVENT_COMMON(true, KTrace::CategoryEnabled, KTrace::EmitComplete, category,  \
                   FXT_INTERN_STRING(label), start_timestamp, KTrace::Timestamp(), \
                   TraceContext::Cpu, ##__VA_ARGS__)

// Similar to KTRACE_COMPLETE, but checks the given constexpr_condition to determine whether the
// event is enabled at compile time.
#define KTRACE_COMPLETE_ENABLE(constexpr_enabled, category, label, start_timestamp, ...)       \
  FXT_EVENT_COMMON(constexpr_enabled, KTrace::CategoryEnabled, KTrace::EmitComplete, category, \
                   FXT_INTERN_STRING(label), start_timestamp, KTrace::Timestamp(),             \
                   TraceContext::Thread, ##__VA_ARGS__)

// Similar to KTRACE_COMPLETE_ENABLE, but associates the event with the current CPU instead of the
// current thread.
#define KTRACE_CPU_COMPLETE_ENABLE(constexpr_enabled, category, label, start_timestamp, ...)   \
  FXT_EVENT_COMMON(constexpr_enabled, KTrace::CategoryEnabled, KTrace::EmitComplete, category, \
                   FXT_INTERN_STRING(label), start_timestamp, KTrace::Timestamp(),             \
                   TraceContext::Cpu, ##__VA_ARGS__)

//
// ## COUNTER
//

// Writes a counter event associated with the current thread when the given category is enabled.
//
// Each argument is rendered as a separate value series named "<label>:<arg name>:<counter_id>".
//
// Arguments:
// - category: Filter category for the event. Expects a string literal.
// - label: Label for the event. Expects a string literal.
// - counter_id: Correlation id for the event. Must be convertible to uint64_t.
// - ...: List of parenthesized argument tuples.
//
#define KTRACE_COUNTER(category, label, counter_id, ...)                                \
  FXT_EVENT_COMMON(true, KTrace::CategoryEnabled, KTrace::EmitCounter, category,        \
                   FXT_INTERN_STRING(label), KTrace::Timestamp(), TraceContext::Thread, \
                   counter_id, ##__VA_ARGS__)

// Similar to KTRACE_COUNTER, but associates the event with the current CPU instead of the current
// thread.
#define KTRACE_CPU_COUNTER(category, label, counter_id, ...)                                     \
  FXT_EVENT_COMMON(true, KTrace::CategoryEnabled, KTrace::EmitCounter, category,                 \
                   FXT_INTERN_STRING(label), KTrace::Timestamp(), TraceContext::Cpu, counter_id, \
                   ##__VA_ARGS__)

// Similar to KTRACE_COUNTER, but checks the given constexpr_condition to determine whether the
// event is enabled at compile time.
#define KTRACE_COUNTER_ENABLE(constexpr_enabled, category, label, counter_id, ...)            \
  FXT_EVENT_COMMON(constexpr_enabled, KTrace::CategoryEnabled, KTrace::EmitCounter, category, \
                   FXT_INTERN_STRING(label), KTrace::Timestamp(), TraceContext::Thread,       \
                   counter_id, ##__VA_ARGS__)

// Similar to KTRACE_COUNTER_ENABLE, but associates the event with the current CPU instead of the
// current thread.
#define KTRACE_CPU_COUNTER_ENABLE(constexpr_enabled, category, label, counter_id, ...)           \
  FXT_EVENT_COMMON(constexpr_enabled, KTrace::CategoryEnabled, KTrace::EmitCounter, category,    \
                   FXT_INTERN_STRING(label), KTrace::Timestamp(), TraceContext::Cpu, counter_id, \
                   ##__VA_ARGS__)

//
// ## FLOW_BEGIN
//

// Writes a flow begin event associated with the current thread when the given category is enabled.
//
// Arguments:
// - category: Filter category for the event. Expects a string literal.
// - label: Label for the event. Expects a string literal.
// - flow_id: Flow id for the event. Must be convertible to uint64_t.
// - ...: List of parenthesized argument tuples.
//
#define KTRACE_FLOW_BEGIN(category, label, flow_id, ...)                                         \
  FXT_EVENT_COMMON(true, KTrace::CategoryEnabled, KTrace::EmitFlowBegin, category,               \
                   FXT_INTERN_STRING(label), KTrace::Timestamp(), TraceContext::Thread, flow_id, \
                   ##__VA_ARGS__)

// Similar to KTRACE_FLOW_BEGIN, but associates the event with the current CPU instead of the
// current thread.
#define KTRACE_CPU_FLOW_BEGIN(category, label, flow_id, ...)                                  \
  FXT_EVENT_COMMON(true, KTrace::CategoryEnabled, KTrace::EmitFlowBegin, category,            \
                   FXT_INTERN_STRING(label), KTrace::Timestamp(), TraceContext::Cpu, flow_id, \
                   ##__VA_ARGS__)

// Similar to KTRACE_FLOW_BEGIN, but checks the given constexpr_condition to determine whether the
// event is enabled at compile time.
#define KTRACE_FLOW_BEGIN_ENABLE(constexpr_enabled, category, label, flow_id, ...)               \
  FXT_EVENT_COMMON(constexpr_enabled, KTrace::CategoryEnabled, KTrace::EmitFlowBegin, category,  \
                   FXT_INTERN_STRING(label), KTrace::Timestamp(), TraceContext::Thread, flow_id, \
                   ##__VA_ARGS__)

// Similar to KTRACE_FLOW_BEGIN_ENABLE, but associates the event with the current CPU instead of the
// current thread.
#define KTRACE_CPU_FLOW_BEGIN_ENABLE(constexpr_enabled, category, label, flow_id, ...)          \
  FXT_EVENT_COMMON(constexpr_enabled, KTrace::CategoryEnabled, KTrace::EmitFlowBegin, category, \
                   FXT_INTERN_STRING(label), KTrace::Timestamp(), TraceContext::Cpu, flow_id,   \
                   ##__VA_ARGS__)

// Similar to KTRACE_FLOW_BEGIN, but accepts an expression to use for the event timestamp.
#define KTRACE_FLOW_BEGIN_TIMESTAMP(category, label, timestamp, flow_id, ...)          \
  FXT_EVENT_COMMON(true, KTrace::CategoryEnabled, KTrace::EmitFlowBegin, category,     \
                   FXT_INTERN_STRING(label), timestamp, TraceContext::Thread, flow_id, \
                   ##__VA_ARGS__)

//
// ## FLOW_STEP
//

// Writes a flow step event associated with the current thread when the given category is enabled.
//
// Arguments:
// - category: Filter category for the event. Expects a string literal.
// - label: Label for the event. Expects a string literal.
// - flow_id: Flow id for the event. Must be convertible to uint64_t.
// - ...: List of parenthesized argument tuples.
//
#define KTRACE_FLOW_STEP(category, label, flow_id, ...)                                          \
  FXT_EVENT_COMMON(true, KTrace::CategoryEnabled, KTrace::EmitFlowStep, category,                \
                   FXT_INTERN_STRING(label), KTrace::Timestamp(), TraceContext::Thread, flow_id, \
                   ##__VA_ARGS__)

// Similar to KTRACE_FLOW_STEP, but associates the event with the current CPU instead of the current
// thread.
#define KTRACE_CPU_FLOW_STEP(category, label, flow_id, ...)                                   \
  FXT_EVENT_COMMON(true, KTrace::CategoryEnabled, KTrace::EmitFlowStep, category,             \
                   FXT_INTERN_STRING(label), KTrace::Timestamp(), TraceContext::Cpu, flow_id, \
                   ##__VA_ARGS__)

// Similar to KTRACE_FLOW_STEP, but checks the given constexpr_condition to determine whether the
// event is enabled at compile time.
#define KTRACE_FLOW_STEP_ENABLE(constexpr_enabled, category, label, flow_id, ...)                \
  FXT_EVENT_COMMON(constexpr_enabled, KTrace::CategoryEnabled, KTrace::EmitFlowStep, category,   \
                   FXT_INTERN_STRING(label), KTrace::Timestamp(), TraceContext::Thread, flow_id, \
                   ##__VA_ARGS__)

// Similar to KTRACE_FLOW_STEP_ENABLE, but associates the event with the current CPU instead of the
// current thread.
#define KTRACE_CPU_FLOW_STEP_ENABLE(constexpr_enabled, category, label, flow_id, ...)          \
  FXT_EVENT_COMMON(constexpr_enabled, KTrace::CategoryEnabled, KTrace::EmitFlowStep, category, \
                   FXT_INTERN_STRING(label), KTrace::Timestamp(), TraceContext::Cpu, flow_id,  \
                   ##__VA_ARGS__)

// Similar to KTRACE_FLOW_STEP, but accepts an expression to use for the event timestamp.
#define KTRACE_FLOW_STEP_TIMESTAMP(category, label, timestamp, flow_id, ...)           \
  FXT_EVENT_COMMON(true, KTrace::CategoryEnabled, KTrace::EmitFlowStep, category,      \
                   FXT_INTERN_STRING(label), timestamp, TraceContext::Thread, flow_id, \
                   ##__VA_ARGS__)

//
// ## FLOW_END
//

// Writes a flow end event associated with the current thread when the given category is enabled.
//
// Arguments:
// - category: Filter category for the event. Expects a string literal.
// - label: Label for the event. Expects a string literal.
// - flow_id: Flow id for the event. Must be convertible to uint64_t.
// - ...: List of parenthesized argument tuples.
//
#define KTRACE_FLOW_END(category, label, flow_id, ...)                                           \
  FXT_EVENT_COMMON(true, KTrace::CategoryEnabled, KTrace::EmitFlowEnd, category,                 \
                   FXT_INTERN_STRING(label), KTrace::Timestamp(), TraceContext::Thread, flow_id, \
                   ##__VA_ARGS__)

// Similar to KTRACE_FLOW_END, but associates the event with the current CPU instead of the current
// thread.
#define KTRACE_CPU_FLOW_END(category, label, flow_id, ...)                                    \
  FXT_EVENT_COMMON(true, KTrace::CategoryEnabled, KTrace::EmitFlowEnd, category,              \
                   FXT_INTERN_STRING(label), KTrace::Timestamp(), TraceContext::Cpu, flow_id, \
                   ##__VA_ARGS__)

// Similar to KTRACE_FLOW_END, but checks the given constexpr_condition to determine whether the
// event is enabled at compile time.
#define KTRACE_FLOW_END_ENABLE(constexpr_enabled, category, label, flow_id, ...)                 \
  FXT_EVENT_COMMON(constexpr_enabled, KTrace::CategoryEnabled, KTrace::EmitFlowEnd, category,    \
                   FXT_INTERN_STRING(label), KTrace::Timestamp(), TraceContext::Thread, flow_id, \
                   ##__VA_ARGS__)

// Similar to KTRACE_FLOW_END_ENABLE, but associates the event with the current CPU instead of the
// current thread.
#define KTRACE_CPU_FLOW_END_ENABLE(constexpr_enabled, category, label, flow_id, ...)          \
  FXT_EVENT_COMMON(constexpr_enabled, KTrace::CategoryEnabled, KTrace::EmitFlowEnd, category, \
                   FXT_INTERN_STRING(label), KTrace::Timestamp(), TraceContext::Cpu, flow_id, \
                   ##__VA_ARGS__)

// Similar to KTRACE_FLOW_END, but accepts an expression to use for the event timestamp.
#define KTRACE_FLOW_END_TIMESTAMP(category, label, timestamp, flow_id, ...)            \
  FXT_EVENT_COMMON(true, KTrace::CategoryEnabled, KTrace::EmitFlowEnd, category,       \
                   FXT_INTERN_STRING(label), timestamp, TraceContext::Thread, flow_id, \
                   ##__VA_ARGS__)

//
// ## CONTEXT_SWITCH
//

// Writes a context switch record for the given threads.
#define KTRACE_CONTEXT_SWITCH(category, cpu, outgoing_state, outgoing_thread_ref,         \
                              incoming_thread_ref, ...)                                   \
  do {                                                                                    \
    if (unlikely(KTrace::CategoryEnabled(FXT_INTERN_CATEGORY(category)))) {               \
      fxt::WriteContextSwitchRecord(&KTrace::GetInstance(), KTrace::Timestamp(),          \
                                    static_cast<uint16_t>(cpu), outgoing_state,           \
                                    outgoing_thread_ref, incoming_thread_ref,             \
                                    FXT_MAP_LIST_ARGS(FXT_MAKE_ARGUMENT, ##__VA_ARGS__)); \
    }                                                                                     \
  } while (false)

//
// ## THREAD_WAKEUP
//

// Writes a thread wakeup record for the given thread.
#define KTRACE_THREAD_WAKEUP(category, cpu, thread_ref, ...)                             \
  do {                                                                                   \
    if (unlikely(KTrace::CategoryEnabled(FXT_INTERN_CATEGORY(category)))) {              \
      fxt::WriteThreadWakeupRecord(&KTrace::GetInstance(), KTrace::Timestamp(),          \
                                   static_cast<uint16_t>(cpu), thread_ref,               \
                                   FXT_MAP_LIST_ARGS(FXT_MAKE_ARGUMENT, ##__VA_ARGS__)); \
    }                                                                                    \
  } while (false)

//
// # Kernel tracing state and low level API
//

constexpr fxt::Koid kNoProcess{0u};

// TODO(https://fxbug.dev/42069955): Move ktrace interfaces into ktrace namespace.
namespace ktrace {

using fxt::Koid;
using fxt::Pointer;
using fxt::Scope;

// Sentinel type for unused arguments.
struct Unused {};

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

class KTrace {
 public:
  static internal::KTraceState& GetInstance() { return state_; }

  // Control is responsible for probing, starting, stopping, or rewinding the ktrace buffer.
  //
  // The meaning of the options changes based on the action. If the action is to start tracing,
  // then the options field functions as the group mask. The ptr argument is only used when
  // probing.
  static zx_status_t Control(uint32_t action, uint32_t options, void* ptr);

  // ReadUser reads len bytes from the ktrace buffer starting at offset off into the given user
  // buffer.
  //
  // On success, this function returns the number of bytes that were read into the buffer.
  // On failure, a zx_status_t error code is returned.
  static zx::result<size_t> ReadUser(user_out_ptr<void> ptr, uint32_t off, size_t len) {
    const ssize_t ret = GetInstance().ReadUser(ptr, off, len);
    if (ret < 0) {
      return zx::error(static_cast<zx_status_t>(ret));
    }
    return zx::ok(ret);
  }

  // Returns the timestamp that should be used to annotate ktrace records.
  static zx_instant_boot_ticks_t Timestamp() { return current_boot_ticks(); }

  static bool CategoryEnabled(const fxt::InternedCategory& category) {
    return GetInstance().IsCategoryEnabled(category);
  }

  // The Emit* functions provide an API by which the ktrace macros above can write a trace record
  // to the ktrace buffer.
  template <fxt::RefType name_type, typename... Ts>
  static void EmitKernelObject(zx_koid_t koid, zx_obj_type_t obj_type,
                               const fxt::StringRef<name_type>& name,
                               const ktl::tuple<Ts...>& args) {
    ktl::apply(
        [&](const Ts&... unpacked_args) {
          fxt::WriteKernelObjectRecord(&GetInstance(), fxt::Koid(koid), obj_type, name,
                                       unpacked_args...);
        },
        args);
  }

  template <fxt::RefType name_type, typename... Ts>
  static void EmitComplete(const fxt::InternedCategory& category,
                           const fxt::StringRef<name_type>& label, uint64_t start_time,
                           uint64_t end_time, TraceContext context, const ktl::tuple<Ts...>& args) {
    ktl::apply(
        [&](const Ts&... unpacked_args) {
          fxt::WriteDurationCompleteEventRecord(
              &GetInstance(), start_time, ThreadRefFromContext(context),
              fxt::StringRef{category.label()}, label, end_time, unpacked_args...);
        },
        args);
  }

  template <fxt::RefType name_type, typename... Ts>
  static void EmitInstant(const fxt::InternedCategory& category,
                          const fxt::StringRef<name_type>& label, uint64_t timestamp,
                          TraceContext context, ktrace::Unused, const ktl::tuple<Ts...>& args) {
    ktl::apply(
        [&](const Ts&... unpacked_args) {
          fxt::WriteInstantEventRecord(&GetInstance(), timestamp, ThreadRefFromContext(context),
                                       fxt::StringRef{category.label()}, label, unpacked_args...);
        },
        args);
  }

  template <fxt::RefType name_type, typename... Ts>
  static void EmitDurationBegin(const fxt::InternedCategory& category,
                                const fxt::StringRef<name_type>& label, uint64_t timestamp,
                                TraceContext context, ktrace::Unused,
                                const ktl::tuple<Ts...>& args) {
    ktl::apply(
        [&](const Ts&... unpacked_args) {
          fxt::WriteDurationBeginEventRecord(
              &GetInstance(), timestamp, ThreadRefFromContext(context),
              fxt::StringRef{category.label()}, label, unpacked_args...);
        },
        args);
  }

  template <fxt::RefType name_type, typename... Ts>
  static void EmitDurationEnd(const fxt::InternedCategory& category,
                              const fxt::StringRef<name_type>& label, uint64_t timestamp,
                              TraceContext context, ktrace::Unused, const ktl::tuple<Ts...> args) {
    ktl::apply(
        [&](const Ts&... unpacked_args) {
          fxt::WriteDurationEndEventRecord(&GetInstance(), timestamp, ThreadRefFromContext(context),
                                           fxt::StringRef{category.label()}, label,
                                           unpacked_args...);
        },
        args);
  }

  template <fxt::RefType name_type, typename... Ts>
  static void EmitCounter(const fxt::InternedCategory& category,
                          const fxt::StringRef<name_type>& label, uint64_t timestamp,
                          TraceContext context, uint64_t counter_id,
                          const ktl::tuple<Ts...>& args) {
    ktl::apply(
        [&](const Ts&... unpacked_args) {
          fxt::WriteCounterEventRecord(&GetInstance(), timestamp, ThreadRefFromContext(context),
                                       fxt::StringRef{category.label()}, label, counter_id,
                                       unpacked_args...);
        },
        args);
  }

  template <fxt::RefType name_type, typename... Ts>
  static void EmitFlowBegin(const fxt::InternedCategory& category,
                            const fxt::StringRef<name_type>& label, uint64_t timestamp,
                            TraceContext context, uint64_t flow_id, const ktl::tuple<Ts...>& args) {
    ktl::apply(
        [&](const Ts&... unpacked_args) {
          fxt::WriteFlowBeginEventRecord(&GetInstance(), timestamp, ThreadRefFromContext(context),
                                         fxt::StringRef{category.label()}, label, flow_id,
                                         unpacked_args...);
        },
        args);
  }

  template <fxt::RefType name_type, typename... Ts>
  static void EmitFlowStep(const fxt::InternedCategory& category,
                           const fxt::StringRef<name_type>& label, uint64_t timestamp,
                           TraceContext context, uint64_t flow_id, const ktl::tuple<Ts...>& args) {
    ktl::apply(
        [&](const Ts&... unpacked_args) {
          fxt::WriteFlowStepEventRecord(&GetInstance(), timestamp, ThreadRefFromContext(context),
                                        fxt::StringRef{category.label()}, label, flow_id,
                                        unpacked_args...);
        },
        args);
  }

  template <fxt::RefType name_type, typename... Ts>
  static void EmitFlowEnd(const fxt::InternedCategory& category,
                          const fxt::StringRef<name_type>& label, uint64_t timestamp,
                          TraceContext context, uint64_t flow_id, const ktl::tuple<Ts...>& args) {
    ktl::apply(
        [&](const Ts&... unpacked_args) {
          fxt::WriteFlowEndEventRecord(&GetInstance(), timestamp, ThreadRefFromContext(context),
                                       fxt::StringRef{category.label()}, label, flow_id,
                                       unpacked_args...);
        },
        args);
  }

 private:
  static internal::KTraceState state_;
};

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

// Bring the fxt literal operators into the global scope.
using fxt::operator""_category;
using fxt::operator""_intern;

template <bool enabled>
inline void ktrace_probe(TraceEnabled<enabled>, TraceContext context,
                         const fxt::InternedString& label, uint64_t a, uint64_t b) {
  if constexpr (enabled) {
    if (KTrace::CategoryEnabled("kernel:probe"_category)) {
      const fxt::StringRef name_ref = fxt::StringRef{label};
      fxt::WriteInstantEventRecord(
          &KTrace::GetInstance(), KTrace::Timestamp(), ThreadRefFromContext(context),
          fxt::StringRef{"kernel:probe"_category.label()}, name_ref,
          fxt::Argument{"arg0"_intern, a}, fxt::Argument{"arg1"_intern, b});
    }
  }
}

void ktrace_report_live_threads();
void ktrace_report_live_processes();

#endif  // ZIRCON_KERNEL_INCLUDE_LIB_KTRACE_H_
