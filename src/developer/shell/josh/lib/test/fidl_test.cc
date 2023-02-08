// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "fidl/test/frobinator/cpp/fidl.h"

#include <fidl/test.fidlcodec.examples/cpp/fidl.h>
#include <fidl/test.fidlcodec.examples/cpp/wire_test_base.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/zx/channel.h>
#include <zircon/syscalls.h>

#include <cstdio>
#include <memory>
#include <string>

#include <gtest/gtest.h>

#include "js_testing_utils.h"
#include "lib/fidl/cpp/binding.h"
#include "lib/fidl/cpp/wire/client.h"
#include "src/developer/shell/josh/lib/runtime.h"
#include "src/developer/shell/josh/lib/zx.h"
#include "src/lib/fidl_codec/fidl_codec_test.h"
#include "src/lib/fidl_codec/library_loader_test_data.h"
#include "testing/fidl/async_loop_for_test.h"
#include "testing/fidl/frobinator_impl.h"

namespace shell {

class FidlTest : public JsTest {
 public:
  static std::string GetFidlJsonByName(const std::string& name) {
    fidl_codec_test::FidlcodecExamples examples;
    const std::string fidl_to_find = name;
    std::string result_fidl;
    for (const auto& element : examples.map()) {
      if (0 == element.first.compare(element.first.length() - fidl_to_find.length(),
                                     fidl_to_find.length(), fidl_to_find)) {
        result_fidl = element.second;
      }
    }
    return result_fidl;
  }

  static std::string GetJsHeader() {
    return "if (globalThis.outHandle == undefined) { throw \"outHandle undefined\"; }\n";
  }

 protected:
  void SetUp() override {
    // JS basic init
    JsTest::SetUp();
    loop_ = std::make_unique<fidl::test::AsyncLoopForTest>();

    InitBuiltins("", "");

    ASSERT_EQ(zx_channel_create(0, &out0_, &out1_), ZX_OK);
    JSContext* ctx = ctx_->Get();
    JSValue js_handle = zx::HandleCreate(ctx, out0_, ZX_OBJ_TYPE_CHANNEL);
    JS_DefinePropertyValueStr(ctx, JS_GetGlobalObject(ctx), "outHandle", js_handle,
                              JS_PROP_CONFIGURABLE);
  }

  void TearDown() override { loop_.reset(nullptr); }

  zx_handle_t out0_;
  zx_handle_t out1_;
  std::unique_ptr<fidl::test::AsyncLoopForTest> loop_;
};

// TODO: Migrate Frobinator tests to use natural FIDL bindings.
class FrobinatorTest : public FidlTest {
 public:
  static std::string GetJsHeader() {
    // Get Fidl Frob library IR
    std::string load =
        "fidl.loadLibraryIr(`" + FidlTest::GetFidlJsonByName("frobinator.fidl.json") + "`);\n";
    // Verify if 'outHandle' has been exported properly
    std::string verify = FidlTest::GetJsHeader();

    return load + verify;
  }

 protected:
  void SetUp() override {
    FidlTest::SetUp();

    // Bind Frobinator
    frob_impl_ = std::make_unique<fidl::test::FrobinatorImpl>();
    binding_ = std::make_unique<fidl::Binding<fidl::test::frobinator::Frobinator>>(
        frob_impl_.get(), ::zx::channel(out1_));
    binding_->set_error_handler(
        [](zx_status_t status) { FAIL() << "Frobinator call failed with status " << status; });
  }

  void TearDown() override {
    binding_.reset(nullptr);
    frob_impl_.reset(nullptr);
    FidlTest::TearDown();
  }

  std::unique_ptr<fidl::test::FrobinatorImpl> frob_impl_;
  std::unique_ptr<fidl::Binding<fidl::test::frobinator::Frobinator>> binding_;
};

TEST_F(FrobinatorTest, FrobinatorFrob) {
  EXPECT_EQ(0u, frob_impl_->frobs.size());

  ASSERT_EVAL(ctx_, FrobinatorTest::GetJsHeader() + R"(
    client = new fidl.ProtocolClient(
        new zx.Channel(globalThis.outHandle), fidling.fidl_test_frobinator.Frobinator);
    client.Frob("one");
  )");
  loop_->RunUntilIdle();
  // This means that the message was received.
  EXPECT_EQ(1u, frob_impl_->frobs.size());
}

TEST_F(FrobinatorTest, FrobinatorSendBasicUnion) {
  EXPECT_EQ(0u, frob_impl_->send_basic_union_received_value_);

  ASSERT_EVAL(ctx_, FrobinatorTest::GetJsHeader() + R"(
    client = new fidl.ProtocolClient(
        new zx.Channel(globalThis.outHandle), fidling.fidl_test_frobinator.Frobinator);
    client.SendBasicUnion({ v: 100 });
  )");
  loop_->RunUntilIdle();
  EXPECT_EQ(100u, frob_impl_->send_basic_union_received_value_);
}

TEST_F(FrobinatorTest, FrobinatorSendBasicTable) {
  EXPECT_EQ(0u, frob_impl_->send_basic_table_received_value_);

  ASSERT_EVAL(ctx_, FrobinatorTest::GetJsHeader() + R"(
    client = new fidl.ProtocolClient(
        new zx.Channel(globalThis.outHandle), fidling.fidl_test_frobinator.Frobinator);
    client.SendBasicTable({ v: 200 });
  )");
  loop_->RunUntilIdle();
  EXPECT_EQ(200u, frob_impl_->send_basic_table_received_value_);
}

TEST_F(FrobinatorTest, FrobinatorSendComplexTables) {
  EXPECT_EQ(0u, frob_impl_->send_complex_tables_received_entry_count_);

  ASSERT_EVAL(ctx_, FrobinatorTest::GetJsHeader() + R"(
    client = new fidl.ProtocolClient(
      new zx.Channel(globalThis.outHandle), fidling.fidl_test_frobinator.Frobinator);
    client.SendComplexTables([
      {
        x: {
          a: [100, 101, 102],
        },
        y: false,
      },
      {
        y: true,
      },
      {
        x: {
          a: [300, 301, 302],
        },
      },
      {
        x: {
          b: 7,
        },
        y: true,
      }
    ]);
  )");
  loop_->RunUntilIdle();
  EXPECT_EQ(4u, frob_impl_->send_complex_tables_received_entry_count_);
  EXPECT_EQ(2u, frob_impl_->send_complex_tables_received_x_a_count_);
  EXPECT_EQ(1u, frob_impl_->send_complex_tables_received_x_b_count_);
  EXPECT_EQ(2u, frob_impl_->send_complex_tables_received_y_true_count_);
  EXPECT_EQ(1u, frob_impl_->send_complex_tables_received_y_false_count_);
}

class PassThroughServer
    : public fidl::testing::WireTestBase<::test_fidlcodec_examples::FidlCodecTestProtocol> {
 public:
  PassThroughServer(async_dispatcher_t* dispatcher,
                    fidl::ServerEnd<test_fidlcodec_examples::FidlCodecTestProtocol> server_end)
      : binding_(fidl::BindServer(dispatcher, std::move(server_end), this)), failed_(false) {}
  virtual void DefaultBitsMessage(
      ::test_fidlcodec_examples::wire::FidlCodecTestProtocolDefaultBitsMessageRequest* request,
      DefaultBitsMessageCompleter::Sync& completer) override {
    default_bits_ = request->v;
  }

  virtual void NotImplemented_(const std::string& name, ::fidl::CompleterBase& completer) override {
    failed_ = true;
  }
  ::test_fidlcodec_examples::wire::DefaultBits default_bits_;
  fidl::ServerBindingRef<test_fidlcodec_examples::FidlCodecTestProtocol> binding_;
  bool failed_;
};

class FidlCodecProtocolTest : public FidlTest {
 public:
  static std::string GetJsHeader() {
    // Get Fidl Frob library IR
    std::string load =
        "fidl.loadLibraryIr(`" + FidlTest::GetFidlJsonByName("fidl_codec/fidl.fidl.json") + "`);\n";
    // Verify if 'outHandle' has been exported properly
    std::string verify = FidlTest::GetJsHeader();

    return load + verify;
  }

 protected:
  void SetUp() override {
    FidlTest::SetUp();

    ::zx::channel o1(out1_);
    fidl::ServerEnd<test_fidlcodec_examples::FidlCodecTestProtocol> server_end(std::move(o1));
    server_end_ = std::move(server_end);
    EXPECT_TRUE(server_end_.is_valid());
  }

  void TearDown() override { FidlTest::TearDown(); }

  fidl::ServerEnd<test_fidlcodec_examples::FidlCodecTestProtocol> server_end_;
};

TEST_F(FidlCodecProtocolTest, SendBits) {
  PassThroughServer server(loop_->dispatcher(), std::move(server_end_));

  ASSERT_EVAL(ctx_, FidlCodecProtocolTest::GetJsHeader() + R"(
    client = new fidl.ProtocolClient(
        new zx.Channel(globalThis.outHandle), fidling.test_fidlcodec_examples.FidlCodecTestProtocol);
    client.DefaultBitsMessage(fidling.test_fidlcodec_examples.DefaultBits.A);
  )");
  loop_->RunUntilIdle();

  ASSERT_TRUE(!server.failed_);
  ASSERT_EQ((uint32_t)::test_fidlcodec_examples::wire::DefaultBits::kA,
            (uint32_t)server.default_bits_);
}

TEST_F(FidlCodecProtocolTest, SendBitsUnion) {
  PassThroughServer server(loop_->dispatcher(), std::move(server_end_));

  ASSERT_EVAL(ctx_, FidlCodecProtocolTest::GetJsHeader() + R"(
    client = new fidl.ProtocolClient(
        new zx.Channel(globalThis.outHandle), fidling.test_fidlcodec_examples.FidlCodecTestProtocol);
    client.DefaultBitsMessage(fidling.test_fidlcodec_examples.DefaultBits.A |
                              fidling.test_fidlcodec_examples.DefaultBits.B );
  )");
  loop_->RunUntilIdle();

  ASSERT_TRUE(!server.failed_);
  ASSERT_EQ((uint32_t)::test_fidlcodec_examples::wire::DefaultBits::kA |
                (uint32_t)::test_fidlcodec_examples::wire::DefaultBits::kB,
            (uint32_t)server.default_bits_);
}

}  // namespace shell
