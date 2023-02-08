// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVELOPER_SHELL_JOSH_LIB_TEST_JS_TESTING_UTILS_H_
#define SRC_DEVELOPER_SHELL_JOSH_LIB_TEST_JS_TESTING_UTILS_H_

#include <zircon/status.h>
#include <zircon/types.h>

#include "src/developer/shell/josh/lib/runtime.h"
#include "third_party/quickjs/quickjs.h"

namespace shell {

// A class that supports running a test inside a quickjs context.
class JsTest : public ::testing::Test {
 protected:
  void SetUp() override {
    rt_ = std::make_unique<Runtime>();
    ASSERT_NE(nullptr, rt_.get()) << "Cannot allocate JS runtime";

    ctx_ = std::make_unique<Context>(rt_.get());
    ASSERT_NE(nullptr, ctx_.get()) << "Cannot allocate JS context";
    if (!ctx_->InitStd()) {
      ctx_->DumpError();
      FAIL();
    }
  }

  // Initializes shell-specific modules, including fidl, zx, and fdio.
  // |fidl_path| points to where you look for FIDL JSON IR.
  // |js_lib_path| points to where you look for system JS libs.
  void InitBuiltins(const std::string& fidl_path, const std::string& js_lib_path) {
    if (!ctx_->InitBuiltins(fidl_path, js_lib_path)) {
      ctx_->DumpError();
      FAIL();
    }
  }

  // Initializes js startup modules in startup_js_path.
  // |startup_js_path| points to where you look for startup JS libs.
  void InitStartups(const std::string& startup_js_path) {
    if (!ctx_->InitStartups(startup_js_path)) {
      ctx_->DumpError();
      FAIL();
    }
  }

  std::unique_ptr<Context> ctx_;
  std::unique_ptr<Runtime> rt_;
};

// Evaluates the given string as JS and asserts it doesn't throw an exception
#define ASSERT_EVAL(ctx_, eval_string)                                         \
  do {                                                                         \
    JSContext* ctx = ctx_->Get();                                              \
    std::string command(eval_string);                                          \
    JSValue result = JS_Eval(ctx, command.data(), command.size(), "batch", 0); \
    ::shell::Value val(ctx, result);                                           \
    if (JS_IsException(result)) {                                              \
      js_std_dump_error(ctx);                                                  \
      GTEST_FAIL() << val.ToString();                                          \
    }                                                                          \
  } while (0)

}  // namespace shell

#endif  // SRC_DEVELOPER_SHELL_JOSH_LIB_TEST_JS_TESTING_UTILS_H_
