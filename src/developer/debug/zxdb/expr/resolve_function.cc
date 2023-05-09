// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/debug/zxdb/expr/resolve_function.h"

#include "src/developer/debug/shared/logging/logging.h"
#include "src/developer/debug/zxdb/common/err_or.h"
#include "src/developer/debug/zxdb/expr/eval_context.h"
#include "src/developer/debug/zxdb/symbols/function.h"
#include "src/developer/debug/zxdb/symbols/input_location.h"
#include "src/developer/debug/zxdb/symbols/process_symbols.h"

namespace zxdb {

ErrOr<fxl::RefPtr<Function>> ResolveFunction(const fxl::RefPtr<EvalContext>& eval_context,
                                             const ParsedIdentifier& fn_name,
                                             const std::vector<ExprValue>& params) {
  std::vector<Location> locs =
      eval_context->GetProcessSymbols()->ResolveInputLocation(InputLocation(ToIdentifier(fn_name)));

  if (locs.empty()) {
    return Err("Could not find function: " + fn_name.GetDebugName());
  } else if (locs.size() > 1) {
    // TODO(https://fxbug.dev/5457): Handle overloads.
    return Err("Function resolved to multiple locations.");
  }

  // Resolve the symbol.
  const Function* fn = locs[0].symbol().Get()->As<Function>();

  // None of the locations actually resolved to the function or the function was inlined, return an
  // error.
  if (!fn) {
    return Err("Error casting symbol " + fn_name.GetDebugName() + " to function.");
  } else if (fn->is_inline()) {
    return Err(fn->GetFullName() + " has been inlined, and cannot be called from the debugger.");
  }

  // TODO(https://fxbug.dev/5457): support primitive type arguments.
  // |fn->parameters()| holds what the symbol thinks the argument types should be.
  // |params| has the list of parameters that were actually given to the expression.
  // Right now if either of these are not empty, don't even bother calling the function since we
  // only support void(void) type functions.
  if (!fn->parameters().empty()) {
    return Err(
        "Functions with arguments are not supported yet. Track progress in https://fxbug.dev/5457 :)");
  } else if (!params.empty()) {
    return Err("This function takes no arguments but parameters were given in the expression.");
  }

  return RefPtrTo(fn);
}

}  // namespace zxdb
