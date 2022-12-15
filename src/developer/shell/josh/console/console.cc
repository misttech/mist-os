// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "console.h"

#include <filesystem>
#include <optional>
#include <string>
#include <vector>

#include "src/developer/shell/josh/console/command_line_options.h"
#include "src/developer/shell/josh/console/li.h"
#include "src/developer/shell/josh/lib/runtime.h"
#include "third_party/quickjs/quickjs-libc.h"
#include "third_party/quickjs/quickjs.h"

namespace shell {

extern "C" const uint8_t qjsc_repl[];
extern "C" const uint32_t qjsc_repl_size;

extern "C" const uint8_t qjsc_repl_init[];
extern "C" const uint32_t qjsc_repl_init_size;

void promise_rejection_tracker(JSContext *ctx, JSValueConst promise, JSValueConst reason,
                               int is_handled, void *opaque) {
  if (opaque) {
    auto rt = reinterpret_cast<Runtime *>(opaque);
    if (!is_handled) {
      rt->HandlePromiseRejection(ctx, promise, reason);
    }
  }
}

int ConsoleMain(int argc, const char **argv) {
  CommandLineOptions options;
  std::vector<std::string> params;

  cmdline::Status status = ParseCommandLine(argc, argv, &options, &params);
  if (status.has_error()) {
    fprintf(stderr, "%s\n", status.error_message().c_str());
    return 1;
  }

  Runtime rt;
  if (rt.Get() == nullptr) {
    fprintf(stderr, "Cannot allocate JS runtime");
    return 1;
  }

  Context ctx(&rt);
  if (ctx.Get() == nullptr) {
    fprintf(stderr, "Cannot allocate JS context");
    return 1;
  }

  // Assign promise rejection tracker to handle async errors
  JS_SetHostPromiseRejectionTracker(rt.Get(), promise_rejection_tracker, static_cast<void *>(&rt));

  if (!ctx.InitStd()) {
    ctx.DumpError();
    return 1;
  }

  if (!ctx.InitBuiltins(options.fidl_ir_path, options.boot_js_lib_path)) {
    ctx.DumpError();
    return 1;
  }

  if (!options.startup_js_lib_path.empty() && !ctx.InitStartups(options.startup_js_lib_path)) {
    ctx.DumpError();
    return 1;
  }

  JSContext *ctx_ptr = ctx.Get();

  // Pass in args after double dash ('--') as the args for the JS script, no need to
  // copy params as js_std_add_helpers will do so.
  //
  // Examples of args after double dash taken as JS script args (inside `[]`)
  //    josh --foo A B C => []
  //    josh --foo -- A B C => [A, B, C]
  //
  //    josh --foo A -- B C => [B, C]
  //    josh --foo -- A -- B C => [A, --, B, C]
  //
  //    josh --foo A B C -- => []
  //    josh --foo -- A B C -- => [A, B, C, --]
  int i = 0;
  for (; i < argc; i++) {
    if (strcmp("--", argv[i]) == 0) {
      i++;
      break;
    }
  }
  int js_argv_size = argc - i;
  // Make sure we never point to somewhere outside of the array
  const char **js_argv = (i < argc ? &argv[i] : nullptr);
  js_std_add_helpers(ctx_ptr, js_argv_size, const_cast<char **>(js_argv));

  // Finish loading everything before moving on to user's tasks
  js_std_loop(ctx_ptr);

  if (!options.command_string && !options.run_script_path) {
    // Running console mode
    if (!options.line_editor) {
      // Use the qjs repl for the time being.
      js_std_eval_binary(ctx_ptr, qjsc_repl, qjsc_repl_size, 0);
    } else {
      if (li::LiModuleInit(ctx_ptr, "li_internal") == nullptr) {
        ctx.DumpError();
        return 1;
      }
      js_std_eval_binary(ctx_ptr, qjsc_repl_init, qjsc_repl_init_size, 0);
    }

    js_std_loop(ctx_ptr);
    return 0;
  }

  // Running command string or script file
  std::string command_string;
  if (options.run_script_path) {
    std::filesystem::path script_path(*options.run_script_path);
    if (!std::filesystem::exists(script_path)) {
      fprintf(stderr, "FATAL: the script %s does not exist!\n", script_path.string().c_str());
      return 1;
    }
    command_string.append("std.loadScript(\"" + script_path.string() + "\");");
  } else {
    command_string = *options.command_string;
  }

  JSValue result = JS_Eval(ctx_ptr, command_string.c_str(), command_string.length(), "batch", 0);
  if (JS_IsException(result)) {
    ctx.DumpError();
    return 1;
  }

  js_std_loop(ctx_ptr);
  return rt.HasUnhandledError() ? 1 : 0;
}

}  // namespace shell
