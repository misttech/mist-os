// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/fit/defer.h>
#include <lib/trace-engine/context.h>
#include <lib/trace-engine/handler.h>
#include <lib/trace-engine/instrumentation.h>

#include <gtest/gtest.h>

namespace {
const unsigned char bytes_disabled[5] = {'b', 'y', 't', 'e', 's'};
const unsigned char bytes_enabled[11] = {'e', 'n', 'a', 'b', 'l', 'e', 'd', '_', 'c', 'a', 't'};
const char* str_disabled = "string";
const char* str_enabled = "enabled_cat";

bool is_category_enabled(trace_handler_t* handler, const char* category) {
  const std::string cat = category;
  return cat == "enabled_cat";
}

const trace_handler_ops ops{
    .is_category_enabled = is_category_enabled,
    .trace_started = [](trace_handler_t*) {},
    .trace_stopped = [](trace_handler_t*, zx_status_t) {},
    .trace_terminated = [](trace_handler_t*) {},
    .notify_buffer_full = [](trace_handler_t*, uint32_t, uint64_t) {},
    .send_alert = [](trace_handler_t*, const char*) {},
};
}  // namespace

// Verify registering byte strings get us the same ids.
TEST(TraceEngineTest, RegisterByteString) {
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);
  char buffer[4096];
  trace_handler_t handler{&ops};
  zx_status_t init = trace_engine_initialize(loop.dispatcher(), &handler,
                                             TRACE_BUFFERING_MODE_ONESHOT, buffer, sizeof(buffer));
  ASSERT_EQ(init, ZX_OK);
  loop.RunUntilIdle();

  zx_status_t start = trace_engine_start(TRACE_START_CLEAR_ENTIRE_BUFFER);
  ASSERT_EQ(start, ZX_OK);
  loop.RunUntilIdle();

  trace_context_t* context = trace_acquire_context();
  ASSERT_TRUE(context);
  trace_string_ref_t ref;
  trace_context_register_bytestring(context, bytes_disabled, sizeof(bytes_disabled), &ref);
  trace_string_ref_t ref2;
  trace_context_register_bytestring(context, bytes_disabled, sizeof(bytes_disabled), &ref2);
  trace_string_ref_t ref3;
  trace_context_register_bytestring(context, bytes_enabled, sizeof(bytes_enabled), &ref3);
  trace_string_ref_t ref4;
  trace_context_register_string_literal(context, str_enabled, &ref4);
  // The same bytestring should get the same encoded_value
  ASSERT_EQ(ref.encoded_value, ref2.encoded_value);
  ASSERT_NE(ref.encoded_value, ref3.encoded_value);
  ASSERT_NE(ref2.encoded_value, ref3.encoded_value);

  // But a byte string and a string literal are not the same -- one has a trailing '\0'.
  ASSERT_NE(ref3.encoded_value, ref4.encoded_value);
  trace_release_context(context);

  trace_engine_stop(ZX_OK);
  trace_engine_terminate();
  loop.RunUntilIdle();
}

// Verify registering strings get us the same ids.
TEST(TraceEngineTest, RegisterString) {
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);
  char buffer[4096];
  trace_handler_t handler{&ops};
  zx_status_t init = trace_engine_initialize(loop.dispatcher(), &handler,
                                             TRACE_BUFFERING_MODE_ONESHOT, buffer, sizeof(buffer));
  ASSERT_EQ(init, ZX_OK);
  loop.RunUntilIdle();

  zx_status_t start = trace_engine_start(TRACE_START_CLEAR_ENTIRE_BUFFER);
  ASSERT_EQ(start, ZX_OK);
  loop.RunUntilIdle();

  trace_context_t* context = trace_acquire_context();
  ASSERT_TRUE(context);
  trace_string_ref_t ref;
  trace_context_register_string_literal(context, str_disabled, &ref);
  trace_string_ref_t ref2;
  trace_context_register_string_literal(context, str_disabled, &ref2);
  trace_string_ref_t ref3;
  trace_context_register_string_literal(context, str_enabled, &ref3);
  ASSERT_EQ(ref.encoded_value, ref2.encoded_value);
  ASSERT_NE(ref.encoded_value, ref3.encoded_value);
  ASSERT_NE(ref2.encoded_value, ref3.encoded_value);
  trace_release_context(context);

  trace_engine_stop(ZX_OK);
  trace_engine_terminate();
  loop.RunUntilIdle();
}

// We should successfully check for both bytestring and c-string categories
TEST(TraceEngineTest, EnabledCategories) {
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);
  char buffer[4096];
  trace_handler_t handler{&ops};
  zx_status_t init = trace_engine_initialize(loop.dispatcher(), &handler,
                                             TRACE_BUFFERING_MODE_ONESHOT, buffer, sizeof(buffer));
  ASSERT_EQ(init, ZX_OK);
  loop.RunUntilIdle();

  zx_status_t start = trace_engine_start(TRACE_START_CLEAR_ENTIRE_BUFFER);
  ASSERT_EQ(start, ZX_OK);
  loop.RunUntilIdle();

  ASSERT_FALSE(trace_is_category_bytestring_enabled(bytes_disabled, sizeof(bytes_disabled)));
  ASSERT_TRUE(trace_is_category_bytestring_enabled(bytes_enabled, sizeof(bytes_enabled)));
  ASSERT_FALSE(trace_is_category_enabled(str_disabled));
  ASSERT_TRUE(trace_is_category_enabled(str_enabled));

  trace_context_t* context = trace_acquire_context();
  ASSERT_TRUE(context);

  ASSERT_FALSE(trace_context_is_category_bytestring_enabled(context, bytes_disabled,
                                                            sizeof(bytes_disabled)));
  ASSERT_TRUE(
      trace_context_is_category_bytestring_enabled(context, bytes_enabled, sizeof(bytes_enabled)));

  ASSERT_FALSE(trace_context_is_category_enabled(context, str_disabled));
  ASSERT_TRUE(trace_context_is_category_enabled(context, str_enabled));
  trace_release_context(context);

  trace_engine_stop(ZX_OK);
  trace_engine_terminate();
  loop.RunUntilIdle();
}

// We should successfully check for both bytestring and c-string categories
TEST(TraceEngineTest, CategoryRegistration) {
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);
  char buffer[4096];
  trace_handler_t handler{&ops};
  zx_status_t init = trace_engine_initialize(loop.dispatcher(), &handler,
                                             TRACE_BUFFERING_MODE_ONESHOT, buffer, sizeof(buffer));
  ASSERT_EQ(init, ZX_OK);
  loop.RunUntilIdle();

  zx_status_t start = trace_engine_start(TRACE_START_CLEAR_ENTIRE_BUFFER);
  ASSERT_EQ(start, ZX_OK);
  loop.RunUntilIdle();

  trace_context_t* context = trace_acquire_context();
  ASSERT_TRUE(context);

  trace_string_ref_t ref;
  // This category isn't enabled
  ASSERT_FALSE(trace_context_register_category_bytestring(context, bytes_disabled,
                                                          sizeof(bytes_disabled), &ref));

  // This category should be enabled
  trace_string_ref_t ref2;
  ASSERT_TRUE(trace_context_register_category_bytestring(context, bytes_enabled,
                                                         sizeof(bytes_enabled), &ref2));
  trace_string_ref_t ref3;
  ASSERT_TRUE(trace_context_register_category_bytestring(context, bytes_enabled,
                                                         sizeof(bytes_enabled), &ref3));
  // And we should get the same id if we re-register
  ASSERT_EQ(ref2.encoded_value, ref3.encoded_value);

  trace_string_ref_t ref4;
  ASSERT_FALSE(trace_context_register_category_literal(context, str_disabled, &ref4));
  trace_string_ref_t ref5;
  ASSERT_TRUE(trace_context_register_category_literal(context, str_enabled, &ref5));
  trace_string_ref_t ref6;
  ASSERT_TRUE(trace_context_register_category_literal(context, str_enabled, &ref6));

  // And we should get the same id if we re-register
  ASSERT_EQ(ref5.encoded_value, ref6.encoded_value);

  // Byte strings and c strings are cached separately though
  ASSERT_NE(ref2.encoded_value, ref5.encoded_value);
  trace_release_context(context);

  trace_engine_stop(ZX_OK);
  trace_engine_terminate();
  loop.RunUntilIdle();
}

TEST(TraceEngineTest, AcquireForCategory) {
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);
  char buffer[4096];
  trace_handler_t handler{&ops};
  zx_status_t init = trace_engine_initialize(loop.dispatcher(), &handler,
                                             TRACE_BUFFERING_MODE_ONESHOT, buffer, sizeof(buffer));
  ASSERT_EQ(init, ZX_OK);
  loop.RunUntilIdle();

  zx_status_t start = trace_engine_start(TRACE_START_CLEAR_ENTIRE_BUFFER);
  ASSERT_EQ(start, ZX_OK);
  loop.RunUntilIdle();

  // This category isn't enabled
  trace_string_ref_t ref1;
  trace_context_t* context1 =
      trace_acquire_context_for_category_bytestring(bytes_disabled, sizeof(bytes_disabled), &ref1);
  ASSERT_FALSE(context1);

  // This category isn't enabled
  trace_string_ref_t ref2;
  trace_context_t* context2 =
      trace_acquire_context_for_category_bytestring(bytes_enabled, sizeof(bytes_enabled), &ref2);
  ASSERT_TRUE(context2);
  trace_release_context(context2);

  // This category isn't enabled
  trace_string_ref_t ref3;
  trace_context_t* context3 = trace_acquire_context_for_category(str_disabled, &ref3);
  ASSERT_FALSE(context3);

  // This category isn't enabled
  trace_string_ref_t ref4;
  trace_context_t* context4 = trace_acquire_context_for_category(str_enabled, &ref4);
  ASSERT_TRUE(context4);
  trace_release_context(context4);

  trace_engine_stop(ZX_OK);
  trace_engine_terminate();
  loop.RunUntilIdle();
}

TEST(TraceEngineTest, AcquireForCategoryCached) {
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);
  char buffer[4096];
  trace_handler_t handler{&ops};
  zx_status_t init = trace_engine_initialize(loop.dispatcher(), &handler,
                                             TRACE_BUFFERING_MODE_ONESHOT, buffer, sizeof(buffer));
  ASSERT_EQ(init, ZX_OK);
  loop.RunUntilIdle();

  zx_status_t start = trace_engine_start(TRACE_START_CLEAR_ENTIRE_BUFFER);
  ASSERT_EQ(start, ZX_OK);
  loop.RunUntilIdle();

  // This category isn't enabled
  trace_string_ref_t ref1;
  static trace_site_t site1{0};
  trace_context_t* context1 = trace_acquire_context_for_category_bytestring_cached(
      bytes_disabled, sizeof(bytes_disabled), &site1, &ref1);
  ASSERT_FALSE(context1);
  ASSERT_EQ(site1.state & 3, trace_site_state_t{1});  // kSiteStateDisabled = 1

  // Calling again should just use the cached value.
  context1 = trace_acquire_context_for_category_bytestring_cached(
      bytes_disabled, sizeof(bytes_disabled), &site1, &ref1);
  ASSERT_FALSE(context1);
  ASSERT_EQ(site1.state & 3, trace_site_state_t{1});  // kSiteStateDisabled = 1

  // This category is enabled
  trace_string_ref_t ref2;
  static trace_site_t site2{0};
  trace_context_t* context2 = trace_acquire_context_for_category_bytestring_cached(
      bytes_enabled, sizeof(bytes_enabled), &site2, &ref2);
  ASSERT_TRUE(context2);
  ASSERT_EQ(site2.state & 3, trace_site_state_t{2});  // kSiteStateEnabled = 2
  trace_release_context(context2);

  context2 = trace_acquire_context_for_category_bytestring_cached(
      bytes_enabled, sizeof(bytes_enabled), &site2, &ref2);
  ASSERT_TRUE(context2);
  ASSERT_EQ(site2.state & 3, trace_site_state_t{2});  // kSiteStateEnabled = 2
  trace_release_context(context2);

  // This category isn't enabled
  trace_string_ref_t ref3;
  static trace_site_t site3{0};
  trace_context_t* context3 =
      trace_acquire_context_for_category_cached(str_disabled, &site3, &ref3);
  ASSERT_FALSE(context3);
  ASSERT_EQ(site3.state & 3, trace_site_state_t{1});  // kSiteStateDisabled = 1

  context3 = trace_acquire_context_for_category_cached(str_disabled, &site3, &ref3);
  ASSERT_FALSE(context3);
  ASSERT_EQ(site3.state & 3, trace_site_state_t{1});  // kSiteStateDisabled = 1

  // This category is enabled
  trace_string_ref_t ref4;
  static trace_site_t site4{0};
  trace_context_t* context4 = trace_acquire_context_for_category_cached(str_enabled, &site4, &ref4);
  ASSERT_TRUE(context4);
  ASSERT_EQ(site4.state & 3, trace_site_state_t{2});  // kSiteStateEnabled = 2
  trace_release_context(context4);

  context4 = trace_acquire_context_for_category_cached(str_enabled, &site4, &ref4);
  ASSERT_TRUE(context4);
  ASSERT_EQ(site4.state & 3, trace_site_state_t{2});  // kSiteStateEnabled = 2
  trace_release_context(context4);

  // Once we stop, we should see all the caches reset.
  trace_engine_stop(ZX_OK);
  EXPECT_EQ(site1.state & 3, trace_site_state_t{0});
  EXPECT_EQ(site2.state & 3, trace_site_state_t{0});
  EXPECT_EQ(site3.state & 3, trace_site_state_t{0});
  EXPECT_EQ(site4.state & 3, trace_site_state_t{0});

  trace_engine_terminate();
  loop.RunUntilIdle();
}

TEST(TraceEngineTest, Alerts) {
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);
  char buffer[4096];
  struct alert_handler : trace_handler_t {
    explicit alert_handler(const trace_handler_ops* ops) : trace_handler_t{ops} {}
    std::vector<std::string> alerts;
  };

  trace_handler_ops alert_ops = ops;
  alert_ops.send_alert = [](trace_handler_t* handler, const char* alert) {
    reinterpret_cast<alert_handler*>(handler)->alerts.emplace_back(alert);
  };

  alert_handler handler{&alert_ops};
  zx_status_t init = trace_engine_initialize(loop.dispatcher(), &handler,
                                             TRACE_BUFFERING_MODE_ONESHOT, buffer, sizeof(buffer));
  ASSERT_EQ(init, ZX_OK);
  loop.RunUntilIdle();

  zx_status_t start = trace_engine_start(TRACE_START_CLEAR_ENTIRE_BUFFER);
  ASSERT_EQ(start, ZX_OK);
  loop.RunUntilIdle();

  trace_context_t* context = trace_acquire_context();
  ASSERT_TRUE(context);
  trace_context_send_alert(context, "test");
  trace_context_send_alert_bytestring(context, bytes_disabled, sizeof(bytes_disabled));
  trace_release_context(context);

  ASSERT_EQ(handler.alerts.size(), size_t{2});
  ASSERT_EQ(handler.alerts[0], std::string{"test"});
  ASSERT_EQ(handler.alerts[1], std::string{"bytes"});

  trace_engine_stop(ZX_OK);
  trace_engine_terminate();
  loop.RunUntilIdle();
}
