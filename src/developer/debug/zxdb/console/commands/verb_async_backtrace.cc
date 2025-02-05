// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/debug/zxdb/console/commands/verb_async_backtrace.h"

#include <algorithm>
#include <cstdint>
#include <string>
#include <string_view>
#include <utility>

#include "src/developer/debug/shared/string_util.h"
#include "src/developer/debug/zxdb/client/frame.h"
#include "src/developer/debug/zxdb/client/target.h"
#include "src/developer/debug/zxdb/client/thread.h"
#include "src/developer/debug/zxdb/common/file_util.h"
#include "src/developer/debug/zxdb/common/join_callbacks.h"
#include "src/developer/debug/zxdb/common/string_util.h"
#include "src/developer/debug/zxdb/console/async_output_buffer.h"
#include "src/developer/debug/zxdb/console/command.h"
#include "src/developer/debug/zxdb/console/command_utils.h"
#include "src/developer/debug/zxdb/console/format_location.h"
#include "src/developer/debug/zxdb/console/format_name.h"
#include "src/developer/debug/zxdb/console/format_node_console.h"
#include "src/developer/debug/zxdb/console/output_buffer.h"
#include "src/developer/debug/zxdb/console/string_util.h"
#include "src/developer/debug/zxdb/console/verbs.h"
#include "src/developer/debug/zxdb/expr/expr.h"
#include "src/developer/debug/zxdb/expr/expr_value.h"
#include "src/developer/debug/zxdb/expr/expr_value_source.h"
#include "src/developer/debug/zxdb/expr/find_name.h"
#include "src/developer/debug/zxdb/expr/found_member.h"
#include "src/developer/debug/zxdb/expr/resolve_base.h"
#include "src/developer/debug/zxdb/expr/resolve_collection.h"
#include "src/developer/debug/zxdb/expr/resolve_ptr_ref.h"
#include "src/developer/debug/zxdb/expr/resolve_variant.h"
#include "src/developer/debug/zxdb/symbols/collection.h"
#include "src/developer/debug/zxdb/symbols/data_member.h"
#include "src/developer/debug/zxdb/symbols/identifier.h"
#include "src/developer/debug/zxdb/symbols/modified_type.h"
#include "src/developer/debug/zxdb/symbols/process_symbols.h"
#include "src/developer/debug/zxdb/symbols/symbol.h"
#include "src/developer/debug/zxdb/symbols/template_parameter.h"
#include "src/lib/fxl/memory/ref_ptr.h"

namespace zxdb {

namespace {

// NOTE: we only support async Rust on Fuchsia for now. Async backtraces in other programming
// languages with different designs will require different approaches.
//
// An asynchronous backtrace is fundamentally different from a synchronous backtrace in that
//
//   * An asynchronous backtrace is a tree of futures rather than a list of frames, as one future
//     could await multiple other futures.
//   * To show a synchronous backtrace, the unwinder walks the stack and uncovers register values
//     of previous frames. Then the debugger symbolizes those frames and could display variables by
//     reading the stack. To show an asynchronous backtrace, the debugger finds the executor, list
//     active tasks, and displays futures recursively. Since Rust doesn't keep the stack or register
//     values for non-running futures, the information is limited to the state stored in the memory.
//   * Since any data types in Rust can be futures and the implementation of their poll methods can
//     be arbitrary, it's impossible for us to find all futures and detect every dependency between
//     them. Instead, we only focus on the async functions or async blocks that are usually the more
//     interesting part during debugging.

constexpr int kVerbose = 1;
constexpr int kMoreVerbose = 2;

const char kAsyncBacktraceShortHelp[] = "async-backtrace / abt: Display all async tasks.";
const char kAsyncBacktraceUsage[] = "async-backtrace";
const char kAsyncBacktraceHelp[] = R"(
  Alias: "abt"

  Print a tree of async tasks from the main future in the current thread.

Arguments

  -v
  --verbose
      Include extra information

  --more-verbose
      Include more extra information

Examples

  abt
  abt -v
  process 2 abt
)";

const std::string kAwaiteeMarker = "└─ ";

struct FormatFutureOptions {
  bool verbose = false;
  ConsoleFormatOptions variable;  // options to format variables
};

fxl::RefPtr<AsyncOutputBuffer> FormatFuture(const ExprValue& future,
                                            const FormatFutureOptions& options,
                                            const fxl::RefPtr<EvalContext>& context, int indent,
                                            const std::string& awaiter_file_name = "");

// Strip the template part of a type. This bypasses the complexity in ParsedIdentifier and should
// be sufficient.
std::string_view StripTemplate(std::string_view type_name) {
  return type_name.substr(0, type_name.find('<'));
}

fxl::RefPtr<AsyncOutputBuffer> FormatMessage(Syntax syntax, const std::string& str) {
  auto out = fxl::MakeRefCounted<AsyncOutputBuffer>();
  out->Complete(syntax, str + '\n');
  return out;
}

fxl::RefPtr<AsyncOutputBuffer> FormatError(std::string msg, const Err& err = Err()) {
  if (err.has_error())
    msg += ": " + err.msg();
  return FormatMessage(Syntax::kWarning, msg);
}

Err MakeError(std::string msg, const Err& err = Err()) {
  if (err.has_error())
    msg += ": " + err.msg();
  return Err(msg);
}

bool IsAsyncFunctionOrBlock(Type* type) {
  if (type->GetIdentifier().components().empty())
    return false;

  // {async_fn_env#0} or {async_block_env#0}
  return debug::StringStartsWith(type->GetIdentifier().components().back().name(), "{async_");
}

fxl::RefPtr<AsyncOutputBuffer> FormatAsyncFunctionOrBlock(const ExprValue& future,
                                                          const FormatFutureOptions& options,
                                                          const fxl::RefPtr<EvalContext>& context,
                                                          int indent) {
  auto out = fxl::MakeRefCounted<AsyncOutputBuffer>();

  // Resolve the current value of the async function.
  fxl::RefPtr<DataMember> member;
  ErrOrValue varient_val = ResolveSingleVariantValue(context, future, &member);
  if (varient_val.has_error())
    return FormatError("Cannot resolve async function", varient_val.err());

  // This should be a struct, e.g., async_rust::main::func::λ::Suspend0.
  ExprValue value = varient_val.take_value();

  // Print the name of the original function, and put the state in a comment.
  Identifier ident = value.type()->GetIdentifier();
  std::string state = ident.components()[ident.components().size() - 1].name();
  ident.components().resize(ident.components().size() - 2);
  out->Append(FormatIdentifier(ident, {}));

  if (!debug::StringStartsWith(state, "Suspend"))
    out->Append(Syntax::kComment, " (" + state + ")");

  std::string filename;
  if (member->decl_line().is_valid()) {
    filename = ExtractLastFileComponent(member->decl_line().file());
    out->Append(" " + GetBullet() + " ");
    out->Append(
        FormatFileLine(member->decl_line(), context->GetProcessSymbols()->target_symbols()));
  }
  out->Append("\n");

  // Iterate data members and find the awaitee.
  std::optional<ExprValue> awaitee;
  std::set<std::string> printed;
  if (const Collection* coll = value.type()->As<Collection>()) {
    for (const auto& lazy_member : coll->data_members()) {
      const DataMember* member = lazy_member.Get()->As<DataMember>();
      // Skip compiler-generated data and static data.
      if (!member || member->artificial() || member->is_external())
        continue;

      std::string name = member->GetAssignedName();
      ErrOrValue val = ResolveNonstaticMember(context, value, FoundMember(coll, member));
      if (val.has_error())
        continue;
      if (name == "__awaitee") {
        awaitee = val.take_value();
      } else if (options.verbose && printed.emplace(name).second) {
        // For some reason Rust could repeat the same field twice.
        out->Append(std::string(indent + 2, ' '));
        out->Append(FormatValueForConsole(val.take_value(), options.variable, context, name));
        out->Append("\n");
      }
    }
  }
  if (awaitee) {
    out->Append(std::string(indent, ' ') + kAwaiteeMarker);
    out->Append(FormatFuture(*awaitee, options, context, indent + 3, filename));
  }
  out->Complete();
  return out;
}

fxl::RefPtr<AsyncOutputBuffer> FormatSelectJoin(const ExprValue& future,
                                                const FormatFutureOptions& options,
                                                const fxl::RefPtr<EvalContext>& context, int indent,
                                                const std::string& select_or_join) {
  auto out = fxl::MakeRefCounted<AsyncOutputBuffer>();
  out->Append(select_or_join + "!\n", TextForegroundColor::kCyan);

  // f should be a lambda.
  ErrOrValue err_or_f = ResolveNonstaticMember(context, future, {"f"});
  if (err_or_f.has_error())
    return FormatError("Cannot read f from PollFn", err_or_f.err());

  ExprValue f = err_or_f.take_value();
  const Collection* f_coll = f.type()->As<Collection>();
  if (!f_coll)
    return FormatError("Wrong type for f in PollFn");

  for (const auto& lazy_member : f_coll->data_members()) {
    const DataMember* member = lazy_member.Get()->As<DataMember>();
    if (!member || member->artificial() || member->is_external())
      continue;
    ErrOrValue member_val = ResolveNonstaticMember(context, f, FoundMember(f_coll, member));
    // Each member_val should be a future.
    if (member_val) {
      out->Append(std::string(indent, ' ') + kAwaiteeMarker);
      out->Append(FormatFuture(member_val.take_value(), options, context, indent + 3));
    }
  }

  out->Complete();
  return out;
}

fxl::RefPtr<AsyncOutputBuffer> FormatFuse(const ExprValue& fuse, const FormatFutureOptions& options,
                                          const fxl::RefPtr<EvalContext>& context, int indent) {
  ErrOrValue inner = ResolveNonstaticMember(context, fuse, {"inner"});
  if (inner.has_error())
    return FormatError("Invalid Fuse (1)", inner.err());
  // |inner| should be an option.
  ErrOrValue some = ResolveSingleVariantValue(context, inner.value());
  if (some.has_error())
    return FormatError("Invalid Fuse (2)", some.err());
  if (some.value().type()->GetAssignedName() == "None")
    return FormatMessage(Syntax::kComment, "(terminated)");
  ErrOrValue future = ResolveNonstaticMember(context, some.value(), {"__0"});
  if (future.has_error())
    return FormatError("Invalid Fuse (3)", future.err());
  return FormatFuture(future.value(), options, context, indent);
}

fxl::RefPtr<AsyncOutputBuffer> FormatThen(const ExprValue& then, const FormatFutureOptions& options,
                                          const fxl::RefPtr<EvalContext>& context, int indent) {
  ErrOrValue val = ResolveNonstaticMember(context, then, {"inner"});
  if (val.has_error())
    return FormatError("Invalid Then (1)", val.err());
  val = ResolveSingleVariantValue(context, val.value());
  if (val.has_error())
    return FormatError("Invalid Then (2)", val.err());
  val = ResolveNonstaticMember(context, val.value(), {"f"});
  if (val.has_error())
    return FormatError("Invalid Then (3)", val.err());
  return FormatFuture(val.value(), options, context, indent);
}

fxl::RefPtr<AsyncOutputBuffer> FormatMap(const ExprValue& map, const FormatFutureOptions& options,
                                         const fxl::RefPtr<EvalContext>& context, int indent) {
  ErrOrValue val = ResolveSingleVariantValue(context, map);
  if (val.has_error())
    return FormatError("Invalid Map (1)", val.err());
  val = ResolveNonstaticMember(context, val.value(), {"future"});
  if (val.has_error())
    return FormatError("Invalid Map (2)", val.err());
  return FormatFuture(val.value(), options, context, indent);
}

fxl::RefPtr<AsyncOutputBuffer> FormatMapDebug(const ExprValue& map,
                                              const FormatFutureOptions& options,
                                              const fxl::RefPtr<EvalContext>& context, int indent) {
  ErrOrValue val = ResolveNonstaticMember(context, map, {"inner"});
  if (val.has_error())
    return FormatError("Invalid Map (0)", val.err());
  return FormatMap(val.value(), options, context, indent);
}

fxl::RefPtr<AsyncOutputBuffer> FormatMaybeDone(const ExprValue& maybe_done,
                                               const FormatFutureOptions& options,
                                               const fxl::RefPtr<EvalContext>& context,
                                               int indent) {
  ErrOrValue some = ResolveSingleVariantValue(context, maybe_done);
  if (some.has_error())
    return FormatError("Invalid MaybeDone (1)", some.err());
  if (some.value().type()->GetAssignedName() == "Future") {
    ErrOrValue future = ResolveNonstaticMember(context, some.value(), {"__0"});
    if (future.has_error())
      return FormatError("Invalid MaybeDone (2)", future.err());
    return FormatFuture(future.value(), options, context, indent);
  }
  return FormatMessage(Syntax::kComment, "(" + some.value().type()->GetAssignedName() + ")");
}

fxl::RefPtr<AsyncOutputBuffer> FormatRemote(const ExprValue& remote,
                                            const FormatFutureOptions& options,
                                            const fxl::RefPtr<EvalContext>& context, int indent) {
  ErrOrValue future = ResolveNonstaticMember(context, remote, {"future", "future", "__0"});
  if (future.has_error())
    return FormatError("Invalid Remote", future.err());
  return FormatFuture(future.value(), options, context, indent);
}

fxl::RefPtr<AsyncOutputBuffer> FormatPin(const ExprValue& pin, const FormatFutureOptions& options,
                                         const fxl::RefPtr<EvalContext>& context, int indent) {
  // Pin changed the member variable name to "__pointer" in
  // https://fuchsia.googlesource.com/third_party/rust/+/346397d081d289db20bc5cbdc23c96e00e60825f
  // So if this fails we should try the old variable.
  ErrOrValue pointer = ResolveNonstaticMember(context, pin, {"__pointer"});
  if (pointer.has_error()) {
    pointer = ResolveNonstaticMember(context, pin, {"pointer"});

    if (pointer.has_error())
      return FormatError("Invalid Pin", pointer.err());
  }

  // Let FormatFuture to resolve pointer.
  return FormatFuture(pointer.value(), options, context, indent);
}

fxl::RefPtr<AsyncOutputBuffer> FormatTaskRunner(const ExprValue& task_runner,
                                                const FormatFutureOptions& options,
                                                const fxl::RefPtr<EvalContext>& context,
                                                int indent) {
  ErrOrValue future = ResolveNonstaticMember(context, task_runner, {"task"});
  if (future.has_error())
    return FormatError("Invalid TaskRunner", future.err());
  return FormatFuture(future.value(), options, context, indent);
}

fxl::RefPtr<AsyncOutputBuffer> FormatScopeJoin(const ExprValue& task_runner,
                                               const FormatFutureOptions& options,
                                               const fxl::RefPtr<EvalContext>& context,
                                               int indent) {
  ErrOrValue arc_inner_ptr =
      ResolveNonstaticMember(context, task_runner, {"scope", "inner", "inner", "ptr", "pointer"});
  if (arc_inner_ptr.has_error())
    return FormatError("Invalid scope::Join", arc_inner_ptr.err());
  auto out = fxl::MakeRefCounted<AsyncOutputBuffer>();
  out->Append("fuchsia_async::scope::Join(", TextForegroundColor::kGray);
  out->Append(to_hex_string(arc_inner_ptr.value().GetAs<TargetPointer>()),
              TextForegroundColor::kGray);
  out->Complete(")\n", TextForegroundColor::kGray);
  return out;
}

fxl::RefPtr<AsyncOutputBuffer> FormatTask(const ExprValue& task_runner,
                                          const FormatFutureOptions& options,
                                          const fxl::RefPtr<EvalContext>& context, int indent) {
  ErrOrValue id = ResolveNonstaticMember(context, task_runner, {"__0", "task_id"});
  uint64_t task_id = 0;
  if (id.has_error() || id.value().PromoteTo64(&task_id).has_error())
    return FormatError("Invalid Task handle", id.err());
  auto out = fxl::MakeRefCounted<AsyncOutputBuffer>();
  out->Complete("fuchsia_async::Task(id = " + std::to_string(task_id) + ")\n",
                TextForegroundColor::kGray);
  return out;
}

fxl::RefPtr<AsyncOutputBuffer> FormatJoinHandle(const ExprValue& task_runner,
                                                const FormatFutureOptions& options,
                                                const fxl::RefPtr<EvalContext>& context,
                                                int indent) {
  ErrOrValue id = ResolveNonstaticMember(context, task_runner, {"task_id"});
  uint64_t task_id = 0;
  if (id.has_error() || id.value().PromoteTo64(&task_id).has_error())
    return FormatError("Invalid JoinHandle", id.err());
  auto out = fxl::MakeRefCounted<AsyncOutputBuffer>();
  out->Complete("fuchsia_async::JoinHandle(id = " + std::to_string(task_id) + ")\n",
                TextForegroundColor::kGray);
  return out;
}

fxl::RefPtr<AsyncOutputBuffer> FormatFuture(const ExprValue& future,
                                            const FormatFutureOptions& options,
                                            const fxl::RefPtr<EvalContext>& context, int indent,
                                            const std::string& awaiter_file_name) {
  std::string_view type = StripTemplate(future.type()->GetFullName());

  // Resolve pointers first. A pointer could be either non-dyn or dyn, raw or boxed.
  //
  // A non-dyn pointer (raw or boxed) is a ModifiedType.
  // A dyn raw pointer is a Collection and has a name "*mut dyn ..." or "*mut (dyn ... + ...)".
  // A dyn boxed pointer has the same layout but with a name "alloc::boxed::Box<(dyn ... + ...)>".
  if (future.type()->As<ModifiedType>() || debug::StringStartsWith(type, "*mut ") ||
      type == "alloc::boxed::Box") {
    auto out = fxl::MakeRefCounted<AsyncOutputBuffer>();
    ResolvePointer(context, future, [=](ErrOrValue val) {
      if (val.has_error()) {
        out->Complete(FormatError("Fail to resolve pointer", val.err()));
      } else {
        out->Complete(FormatFuture(val.value(), options, context, indent));
      }
    });
    return out;
  }

  if (IsAsyncFunctionOrBlock(future.type()))
    return FormatAsyncFunctionOrBlock(future, options, context, indent);

  if (type == "core::pin::Pin")
    return FormatPin(future, options, context, indent);
  if (type == "fuchsia_async::runtime::fuchsia::executor::scope::Join")
    return FormatScopeJoin(future, options, context, indent);
  if (type == "fuchsia_async::runtime::fuchsia::task::Task")
    return FormatTask(future, options, context, indent);
  if (type == "fuchsia_async::runtime::fuchsia::task::JoinHandle")
    return FormatJoinHandle(future, options, context, indent);
  if (type == "futures_util::future::future::fuse::Fuse")
    return FormatFuse(future, options, context, indent);
  if (type == "futures_util::future::maybe_done::MaybeDone")
    return FormatMaybeDone(future, options, context, indent);
  if (type == "futures_util::future::future::Then")
    return FormatThen(future, options, context, indent);
  if (type == "futures_util::future::future::Map")  // only appears in debug mode.
    return FormatMapDebug(future, options, context, indent);
  if (type == "futures_util::future::future::map::Map")
    return FormatMap(future, options, context, indent);
  if (type == "futures_util::future::future::remote_handle::Remote")
    return FormatRemote(future, options, context, indent);
  if (type == "vfs::execution_scope::TaskRunner")
    return FormatTaskRunner(future, options, context, indent);

  // NOTE: `select!` and `join!` macro expand to PollFn. It'll be useful if we could describe it.
  // However, PollFn could encode an arbitrary function so there's a chance we're doing very wrong.
  // To be more accurate, we also check the filename of the awaiter. `select!` will be expanded
  // from select_mod.rs, and `join!` will be expanded from `join_mod.rs`.
  if (type == "futures_util::future::poll_fn::PollFn") {
    if (awaiter_file_name == "select_mod.rs")
      return FormatSelectJoin(future, options, context, indent, "select");
    if (awaiter_file_name == "join_mod.rs")
      return FormatSelectJoin(future, options, context, indent, "join");
  }

  // General formatter.
  auto out = fxl::MakeRefCounted<AsyncOutputBuffer>();
  if (options.verbose) {
    out->Append(FormatValueForConsole(future, options.variable, context));
    out->Complete("\n");
  } else {
    out->Complete(std::string(type) + "\n", TextForegroundColor::kGray);
  }
  return out;
}

// Format (usize, alloc::sync::Arc<fuchsia_async::runtime::fuchsia::executor::common::Task>)
// Instead of return an AsyncOutputBuffer directly, use a callback to also return task_id.
void FormatActiveTasksHashMapTuple(
    const ExprValue& tuple, const FormatFutureOptions& options,
    const fxl::RefPtr<EvalContext>& context, int indent,
    fit::callback<void(uint64_t, fxl::RefPtr<AsyncOutputBuffer>)> cb) {
  ErrOrValue arc_inner_ptr = ResolveNonstaticMember(context, tuple, {"__1", "ptr", "pointer"});
  if (arc_inner_ptr.has_error())
    return cb(0, FormatError("Invalid HashMap tuple (1)", arc_inner_ptr.err()));
  ResolvePointer(
      context, arc_inner_ptr.value(), [=, cb = std::move(cb)](ErrOrValue arc_inner) mutable {
        if (arc_inner.has_error())
          return cb(0, FormatError("Invalid HashMap tuple (2)", arc_inner.err()));
        ErrOrValue id = ResolveNonstaticMember(context, arc_inner.value(), {"data", "id"});
        uint64_t task_id = 0;
        if (id.has_error() || id.value().PromoteTo64(&task_id).has_error())
          return cb(0, FormatError("Invalid HashMap tuple (3)", id.err()));
        auto out = fxl::MakeRefCounted<AsyncOutputBuffer>();
        if (indent >= 3) {
          out->Append(std::string(indent - 3, ' '));
          out->Append(kAwaiteeMarker);
        }
        out->Append("Task(id = " + std::to_string(task_id) + ")\n", TextForegroundColor::kGreen);
        out->Append(std::string(indent, ' '));
        out->Append(kAwaiteeMarker);
        // Arc -> Task -> AtomicFuture
        ErrOrValue atomic_future =
            ResolveNonstaticMember(context, arc_inner.value(), {"data", "future"});
        if (atomic_future.has_error())
          return cb(task_id, FormatError("Invalid HashMap tuple (4)", atomic_future.err()));

        ErrOrValue state = ResolveNonstaticMember(context, atomic_future.value(), {"state"});
        if (state.has_error())
          return cb(task_id, FormatError("Invalid HashMap tuple (5)", state.err()));

        // Read the state of the future.
        uint64_t state_value;
        if (auto result = state.value().PromoteTo64(&state_value); result.has_error())
          return cb(task_id, FormatError("Invalid HashMap tuple (6)", result));

        // See if the future is DONE.
        if (state_value & (1 << 2)) {
          out->Complete("Finished\n");
          return cb(task_id, out);
        }

        // AtomicFuture -> UnsafeCell -> Box<dyn FutureOrResult>
        ErrOrValue value =
            ResolveNonstaticMember(context, atomic_future.value(), {"future", "value"});

        // Now we have `Box<dyn FutureOrResult>`.  To determine the concrete type of the future we
        // need to find the future's drop function.  We can't use FutureOrResult's drop because it
        // doesn't have a drop method (it uses ManuallyDrop), so we have to find the `drop_future`
        // method from the `FutureOrResult` trait.

        // Extract pointer and vtable from the fat pointer.
        ErrOrValue pointer_val = ResolveNonstaticMember(context, value.value(), {"pointer"});
        ErrOrValue vtable_val = ResolveNonstaticMember(context, value.value(), {"vtable"});
        TargetPointer vtable = 0;
        if (pointer_val.has_error() || vtable_val.has_error() ||
            vtable_val.value().PromoteTo64(&vtable).has_error())
          return cb(task_id, FormatMessage(Syntax::kError, "Invalid HashMap tuple (7)"));

        // We want to get to the `drop_future` method which is the first method in the trait.  The
        // vtable should be <drop, size, align, trait methods...>, so it's the fourth pointer.
        context->GetDataProvider()->GetMemoryAsync(
            vtable + 3 * sizeof(TargetPointer), sizeof(TargetPointer),
            [=, cb = std::move(cb), pointer = pointer_val.value()](
                const Err& err, std::vector<uint8_t> data) mutable {
              if (err.has_error() || data.size() != sizeof(TargetPointer))
                return cb(task_id, FormatMessage(Syntax::kError, "Invalid HashMap tuple (8)"));

              // Assume the same endian.
              TargetPointer drop_in_place_addr = *reinterpret_cast<TargetPointer*>(data.data());
              Location loc = context->GetLocationForAddress(drop_in_place_addr);
              if (!loc.symbol())
                return cb(task_id, FormatMessage(Syntax::kError, "Invalid HashMap tuple (9)"));

              const Function* func = loc.symbol().Get()->As<Function>();
              if (!func || func->template_params().empty())
                return cb(task_id, FormatMessage(Syntax::kError, "Invalid HashMap tuple (10)"));

              // Get the templated parameter and create a ModifiedType to that type.
              LazySymbol pointed_to =
                  func->template_params()[0].Get()->As<TemplateParameter>()->type();
              auto derived = fxl::MakeRefCounted<ModifiedType>(DwarfTag::kPointerType, pointed_to);

              out->Complete(FormatFuture(ExprValue(derived, pointer.data(), pointer.source()),
                                         options, context, 3 + indent));

              cb(task_id, out);
            });
      });
}

// Iterate over items in a hashbrown HashMap of any type.
template <typename Val>
void IterateHashMap(const ExprValue& hashmap, const fxl::RefPtr<EvalContext>& context,
                    fit::function<void(const ExprValue&, fit::callback<void(Val)>)> each_cb,
                    fit::callback<void(ErrOr<std::vector<Val>>)> done_cb) {
  if (StripTemplate(hashmap.type()->GetFullName()) != "hashbrown::map::HashMap") {
    return done_cb(MakeError("Expect a HashMap, got " + hashmap.type()->GetFullName()));
  }
  // See |StdHashMapSyntheticProvider| in .../rustlib/etc/lldb_providers.py for the layout.

  // 1. Obtain the type of the tuple (usize, Arc<Task>)
  ErrOrValue raw_table = ResolveNonstaticMember(context, hashmap, {"table"});
  if (raw_table.has_error())
    return done_cb(MakeError("Invalid HashMap (1)", raw_table.err()));
  const Collection* raw_table_coll = raw_table.value().type()->As<Collection>();
  if (!raw_table_coll || raw_table_coll->template_params().empty())
    return done_cb(MakeError("Invalid HashMap (2)"));
  fxl::RefPtr<Type> tuple_type;
  if (auto param = raw_table_coll->template_params()[0].Get()->As<TemplateParameter>())
    tuple_type = RefPtrTo(param->type().Get()->As<Type>());
  if (!tuple_type)
    return done_cb(MakeError("Invalid HashMap (3)"));

  // 2. Resolve bucket_mask and ctrl pointer.
  ErrOrValue bucket_mask_res =
      ResolveNonstaticMember(context, raw_table.value(), {"table", "bucket_mask"});
  if (bucket_mask_res.has_error())
    return done_cb(MakeError("Invalid HashMap (4)", bucket_mask_res.err()));
  uint64_t bucket_mask = 0;
  Err err = bucket_mask_res.value().PromoteTo64(&bucket_mask);
  if (err.has_error())
    return done_cb(MakeError("Invalid HashMap (5)", err));
  if (!bucket_mask) {
    // Empty hashmap.
    return done_cb(std::vector<Val>{});
  }
  ErrOrValue ctrl_res =
      ResolveNonstaticMember(context, raw_table.value(), {"table", "ctrl", "pointer"});
  if (ctrl_res.has_error())
    return done_cb(MakeError("Invalid HashMap (6)", ctrl_res.err()));
  uint64_t ctrl = 0;
  err = ctrl_res.value().PromoteTo64(&ctrl);
  if (err.has_error() || !ctrl)
    return done_cb(MakeError("Invalid HashMap (7)", err));

  // 3. Read the memory. To save some operations we try to fetch the whole hashmap once.
  // The layout of a HashMap looks like
  //   Tn, ..., T2, T1, C1, C2, ..., Cn
  //                    ^ |ctrl| points here
  uint64_t capacity = bucket_mask + 1;
  uint64_t total_buckets_size = tuple_type->byte_size() * capacity;
  context->GetDataProvider()->GetMemoryAsync(
      ctrl - total_buckets_size, total_buckets_size + capacity,
      [=, done_cb = std::move(done_cb), each_cb = std::move(each_cb)](
          const Err& err, std::vector<uint8_t> data) mutable {
        if (err.has_error()) {
          return done_cb(MakeError("Invalid HashMap (8)", err));
        }
        // Items are collected using a JoinCallbacks first so that they can be sorted
        // rather than the orders appearing in the hashmap.
        auto joiner = fxl::MakeRefCounted<JoinCallbacks<Val>>();
        for (size_t idx = 0; idx < capacity; idx++) {
          if ((data[total_buckets_size + idx] & 0x80))  // not present
            continue;
          uint8_t* slot = &data[total_buckets_size - (idx + 1) * tuple_type->byte_size()];
          ExprValue tuple(tuple_type, {slot, slot + tuple_type->byte_size()},
                          ExprValueSource(ctrl - (idx + 1) * tuple_type->byte_size()));
          each_cb(tuple, joiner->AddCallback());
        }
        joiner->Ready(
            [done_cb = std::move(done_cb)](std::vector<Val> val) mutable { done_cb(val); });
      });
}

// Format HashMap<usize, alloc::sync::Arc<fuchsia_async::runtime::fuchsia::executor::common::Task>>
fxl::RefPtr<AsyncOutputBuffer> FormatActiveTasksHashMap(const ErrOrValue& hashmap,
                                                        const FormatFutureOptions& options,
                                                        const fxl::RefPtr<EvalContext>& context,
                                                        int indent) {
  auto out = fxl::MakeRefCounted<AsyncOutputBuffer>();
  using CallbackDataType = std::pair<uint64_t, fxl::RefPtr<AsyncOutputBuffer>>;
  IterateHashMap<CallbackDataType>(
      hashmap.value(), context,
      [=](auto tuple, auto cb) {
        FormatActiveTasksHashMapTuple(tuple, options, context, indent,
                                      [cb = std::move(cb)](auto task_id, auto output) mutable {
                                        cb(std::make_pair(task_id, std::move(output)));
                                      });
      },
      [out](ErrOr<std::vector<CallbackDataType>> value) {
        if (value.has_error()) {
          return out->Complete(FormatError("Failed to iterate all_tasks", value.err()));
        }
        auto tasks = value.take_value();
        std::sort(tasks.begin(), tasks.end(),
                  [](auto& p1, auto& p2) { return p1.first < p2.first; });
        for (auto& p : tasks) {
          out->Append(std::move(p.second));
        }
        out->Complete();
      });
  return out;
}

fxl::RefPtr<AsyncOutputBuffer> FormatScope(const ErrOrValue& scope_state,
                                           const FormatFutureOptions& options,
                                           const fxl::RefPtr<EvalContext>& context, int indent) {
  if (scope_state.has_error()) {
    return FormatError("Cannot locate scope state", scope_state.err());
  }
  ErrOrValue hashmap = ResolveNonstaticMember(context, scope_state.value(), {"all_tasks", "base"});
  if (hashmap.has_error()) {
    return FormatError("Cannot locate all_tasks", hashmap.err());
  }
  auto out = fxl::MakeRefCounted<AsyncOutputBuffer>();
  out->Append(FormatActiveTasksHashMap(hashmap.value(), options, context, indent));

  ErrOrValue children =
      ResolveNonstaticMember(context, scope_state.value(), {"children", "base", "map"});
  if (children.has_error()) {
    return FormatError("Cannot locate scope children", children.err());
  }
  IterateHashMap<fxl::RefPtr<AsyncOutputBuffer>>(
      children.value(), context,
      [=](auto tuple, auto cb) mutable {
        ErrOrValue arc_inner_ptr =
            ResolveNonstaticMember(context, tuple, {"__0", "inner", "ptr", "pointer"});
        if (arc_inner_ptr.has_error())
          return cb(FormatError("Invalid children tuple (1)", arc_inner_ptr.err()));
        ResolvePointer(
            context, arc_inner_ptr.value(), [=, cb = std::move(cb)](ErrOrValue arc_inner) mutable {
              // TODO: Check strong count.
              if (arc_inner.has_error())
                return cb(FormatError("Invalid children tuple (2)", arc_inner.err()));
              // ArcInner<ScopeInner> -> ScopeInner -> Condition<ScopeState> -> Arc<Mutex<_>> ->
              // ArcInner<Mutex<_>>
              ErrOrValue mutex_ptr = ResolveNonstaticMember(
                  context, arc_inner.value(), {"data", "state", "__0", "ptr", "pointer"});
              if (mutex_ptr.has_error())
                return cb(FormatError("Invalid children tuple (3)", mutex_ptr.err()));
              ResolvePointer(
                  context, mutex_ptr.value(), [=, cb = std::move(cb)](ErrOrValue mutex) mutable {
                    if (mutex.has_error())
                      return cb(FormatError("Invalid children tuple (4)", mutex.err()));
                    // ArcInner<Mutex<_>> -> Mutex<_> -> UnsafeCell -> condition::Inner ->
                    // ScopeState
                    ErrOrValue child_state = ResolveNonstaticMember(
                        context, mutex.value(), {"data", "data", "value", "data"});
                    if (child_state.has_error())
                      return cb(FormatError("Invalid children tuple (5)", child_state.err()));

                    auto out = fxl::MakeRefCounted<AsyncOutputBuffer>();
                    if (indent >= 3) {
                      out->Append(kAwaiteeMarker);
                      out->Append(std::string(indent - 3, ' '));
                    }
                    out->Append(Syntax::kStringBold, "Scope");
                    out->Append(Syntax::kNumberDim, "(");
                    out->Append(Syntax::kNumberDim,
                                to_hex_string(arc_inner_ptr.value().GetAs<TargetPointer>()));
                    out->Append(Syntax::kNumberDim, ")\n");
                    out->Append(FormatScope(child_state, options, context, 3 + indent));
                    out->Complete();
                    cb(out);
                  });
            });
      },
      [=](auto values) {
        if (values.has_error()) {
          return out->Complete(FormatError("Failed to iterate children", values.err()));
        }
        for (auto& buf : values.value()) {
          out->Append(buf);
        }
        out->Complete();
      });

  return out;
}

void OnStackReady(Stack& stack, fxl::RefPtr<CommandContext> cmd_context,
                  const FormatFutureOptions& options) {
  // Step 2: locate main_future and the executor.
  if (stack.empty()) {
    cmd_context->ReportError(
        Err("Cannot sync frames. Please ensure the thread is either suspended "
            "or blocked in an exception. Use \"pause\" to suspend it."));
    return;
  }
  for (size_t i = 0; i < stack.size(); i++) {
    if (!stack[i]->GetLocation().has_symbols())
      continue;
    // TODO(https://fxbug.dev/339724188): Traverse nested scopes.
    std::string func_name(StripTemplate(stack[i]->GetLocation().symbol().Get()->GetFullName()));
    std::string expr;
    if (func_name == "fuchsia_async::runtime::fuchsia::executor::local::LocalExecutor::run") {
      expr = "self.ehandle.root_scope.inner->data.state.__0->data.data.value.data";
    } else if (func_name == "fuchsia_async::runtime::fuchsia::executor::send::SendExecutor::run") {
      expr = "self.root_scope.inner->data.state.__0->data.data.value.data";
    } else if (func_name.starts_with("fuchsia_async::runtime::fuchsia::executor::send") &&
               func_name.find("create_worker_threads::{closure") != std::string::npos) {
      // TODO(sadmac): This is depending on a local closure capture. All of this
      // code is delicate in the face of refactors of the fuchsia_async crate
      // but depending on a *local* might be a bit too brittle. Ideally we'd
      // fetch from the EXECUTOR thread-local static. Not sure how to get to
      // that though.
      expr = "root_scope.inner->data.state.__0->data.data.value.data";
    } else {
      continue;
    }

    auto out = fxl::MakeRefCounted<AsyncOutputBuffer>();
    auto context = stack[i]->GetEvalContext();
    EvalExpression(expr, context, false, [out, options, context](const ErrOrValue& value) {
      out->Complete(FormatScope(value, options, context, 0));
    });
    cmd_context->Output(out);
    return;
  }
  cmd_context->ReportError(Err("Cannot locate the async executor on the stack."));
}

void RunVerbAsyncBacktrace(const Command& cmd, fxl::RefPtr<CommandContext> cmd_context) {
  if (Err err = cmd.ValidateNouns({Noun::kProcess, Noun::kThread}); err.has_error())
    return cmd_context->ReportError(err);

  if (!cmd.thread())
    return cmd_context->ReportError(Err("There is no thread to show backtrace."));

  FormatFutureOptions options;
  if (cmd.HasSwitch(kMoreVerbose)) {
    options.verbose = true;
    options.variable.verbosity = ConsoleFormatOptions::Verbosity::kMedium;
    options.variable.wrapping = ConsoleFormatOptions::Wrapping::kSmart;
    options.variable.pointer_expand_depth = 3;
    options.variable.max_depth = 6;
  } else if (cmd.HasSwitch(kVerbose)) {
    options.verbose = true;
    options.variable.verbosity = ConsoleFormatOptions::Verbosity::kMedium;
    options.variable.pointer_expand_depth = 1;
    options.variable.max_depth = 3;
  }

  // Step 1: obtain the (synchronous) stack.
  if (cmd.thread()->GetStack().has_all_frames()) {
    OnStackReady(cmd.thread()->GetStack(), std::move(cmd_context), options);
  } else {
    cmd.thread()->GetStack().SyncFrames(
        false, [thread = cmd.thread(), cmd_context = std::move(cmd_context),
                options](const Err& err) mutable {
          if (err.has_error()) {
            cmd_context->ReportError(err);
          } else {
            OnStackReady(thread->GetStack(), std::move(cmd_context), options);
          }
        });
  }
}

}  // namespace

VerbRecord GetAsyncBacktraceVerbRecord() {
  VerbRecord abt(&RunVerbAsyncBacktrace, {"async-backtrace", "abt"}, kAsyncBacktraceShortHelp,
                 kAsyncBacktraceUsage, kAsyncBacktraceHelp, CommandGroup::kQuery);
  abt.switches.emplace_back(kVerbose, false, "verbose", 'v');
  abt.switches.emplace_back(kMoreVerbose, false, "more-verbose", 0);
  return abt;
}

}  // namespace zxdb
