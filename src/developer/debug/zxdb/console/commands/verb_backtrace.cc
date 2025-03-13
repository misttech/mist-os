// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/debug/zxdb/console/commands/verb_backtrace.h"

#include "src/developer/debug/zxdb/client/process.h"
#include "src/developer/debug/zxdb/client/target.h"
#include "src/developer/debug/zxdb/console/command.h"
#include "src/developer/debug/zxdb/console/command_utils.h"
#include "src/developer/debug/zxdb/console/format_frame.h"
#include "src/developer/debug/zxdb/console/format_location.h"
#include "src/developer/debug/zxdb/console/format_node_console.h"
#include "src/developer/debug/zxdb/console/verbs.h"

namespace zxdb {

namespace {

constexpr int kForceAllTypes = 1;
constexpr int kRawOutput = 2;
constexpr int kVerboseBacktrace = 3;
constexpr int kForceRefresh = 4;
constexpr int kForceRemoteUnwind = 5;

const char kBacktraceShortHelp[] = "backtrace / bt: Print a backtrace.";
const char kBacktraceUsage[] = "backtrace / bt";
const char kBacktraceHelp[] = R"(
  Prints a backtrace of the thread, including function parameters.

  To see just function names and line numbers, use "frame" or just "f".

Arguments

  -f
  --force
      Force an update to the stack when listing stack frames. This will always
      reevaluate the complete call stack based on the current stop location. Use
      --force-remote-unwind to unwind completely on the target.

  --force-remote-unwind
      Force the unwinding operation to happen from the target backend. Note that
      this may result in different results than the default due to less metadata
      availability on the target compared to the default option. Implies
      --force.

  -r
  --raw
      Expands frames that were collapsed by the "pretty" stack formatter.

  -t
  --types
      Include all type information for function parameters.

  -v
  --verbose
      Include extra stack frame information:
       • Full template lists and function parameter types.
       • Instruction pointer.
       • Stack pointer.
       • Stack frame base pointer.

Examples

  t 2 bt
  thread 2 backtrace

  t * bt
  all threads backtrace
)";

void RunVerbBacktrace(const Command& cmd, fxl::RefPtr<CommandContext> cmd_context) {
  if (Err err = cmd.ValidateNouns({Noun::kProcess, Noun::kThread}, true); err.has_error())
    return cmd_context->ReportError(err);

  if (!cmd.thread() && cmd.GetNounIndex(Noun::kThread) != Command::kWildcard)
    return cmd_context->ReportError(Err("There is no thread to have frames."));

  auto opts = FormatStackOptions::GetFrameOptions(cmd.target(), cmd.HasSwitch(kVerboseBacktrace),
                                                  cmd.HasSwitch(kForceAllTypes), 3);

  if (!cmd.HasSwitch(kRawOutput))
    opts.pretty_stack = cmd_context->GetConsoleContext()->pretty_stack_manager();

  opts.frame.detail = FormatFrameOptions::kParameters;
  if (cmd.HasSwitch(kVerboseBacktrace)) {
    opts.frame.detail = FormatFrameOptions::kVerbose;
  }

  // These are minimal since there is often a lot of data.
  opts.frame.variable.verbosity = ConsoleFormatOptions::Verbosity::kMinimal;
  opts.frame.variable.verbosity = cmd.HasSwitch(kForceAllTypes)
                                      ? ConsoleFormatOptions::Verbosity::kAllTypes
                                      : ConsoleFormatOptions::Verbosity::kMinimal;

  if (cmd.GetNounIndex(Noun::kThread) == Command::kWildcard) {
    FX_DCHECK(cmd.target());
    FX_DCHECK(cmd.target()->GetProcess());

    cmd_context->Output(
        FormatAllThreadStacks(cmd.target()->GetProcess()->GetThreads(), opts, cmd_context));
    return;
  }

  opts.sync_options.force_update = cmd.HasSwitch(kForceRefresh);
  opts.sync_options.remote_unwind = cmd.HasSwitch(kForceRemoteUnwind);
  cmd_context->Output(FormatStack(cmd.thread(), opts));
}

}  // namespace

VerbRecord GetBacktraceVerbRecord() {
  VerbRecord backtrace(&RunVerbBacktrace, {"backtrace", "where", "bt"}, kBacktraceShortHelp,
                       kBacktraceUsage, kBacktraceHelp, CommandGroup::kQuery);
  SwitchRecord force_types(kForceAllTypes, false, "types", 't');
  SwitchRecord raw(kRawOutput, false, "raw", 'r');
  SwitchRecord verbose(kVerboseBacktrace, false, "verbose", 'v');
  SwitchRecord force_refresh(kForceRefresh, false, "force", 'f');
  SwitchRecord force_remote_unwind(kForceRemoteUnwind, false, "force-remote-unwind");
  backtrace.switches = {force_types, raw, verbose, force_refresh};

  return backtrace;
}

}  // namespace zxdb
