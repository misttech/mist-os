// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.tracing.controller/cpp/fidl.h>
#include <fidl/fuchsia.tracing/cpp/fidl.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/socket.h>
#include <stdlib.h>

#include <gtest/gtest.h>
#include <trace-test-utils/read_records.h>

TEST(PerfettoBridgeIntegrationTest, Init) {
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);
  zx::result client_end = component::Connect<fuchsia_tracing_controller::Controller>();
  ASSERT_TRUE(client_end.is_ok());
  const fidl::SyncClient client{std::move(*client_end)};

  // Wait for perfetto bridge to attach
  for (unsigned retries = 0; retries < 5; retries++) {
    fidl::Result<fuchsia_tracing_controller::Controller::GetProviders> providers =
        client->GetProviders();
    ASSERT_TRUE(providers.is_ok());
    if (providers->providers().size() == 1) {
      break;
    }
    sleep(1);
  }
  fidl::Result<fuchsia_tracing_controller::Controller::GetProviders> providers =
      client->GetProviders();
  ASSERT_TRUE(providers.is_ok());
  ASSERT_EQ(providers->providers().size(), size_t{1});

  zx::socket in_socket;
  zx::socket outgoing_socket;

  ASSERT_EQ(zx::socket::create(0u, &in_socket, &outgoing_socket), ZX_OK);
  const fuchsia_tracing_controller::TraceConfig config{
      {.buffer_size_megabytes_hint = uint32_t{4},
       .buffering_mode = fuchsia_tracing::BufferingMode::kOneshot}};
  auto init_response = client->InitializeTracing({config, std::move(outgoing_socket)});
  ASSERT_TRUE(init_response.is_ok());

  client->StartTracing({});
  loop.Run(zx::deadline_after(zx::sec(1)));
  client->StopTracing({{{.write_results = true}}});

  uint8_t buffer[1024];
  size_t actual;
  ASSERT_EQ(in_socket.read(0, buffer, 1024, &actual), ZX_OK);
  ASSERT_GT(actual, size_t{0});
  ASSERT_LT(actual, size_t{1024});
  FX_LOGS(INFO) << "Socket read " << actual << " bytes of trace data.";

  bool saw_perfetto_blob = false;
  trace::TraceReader::RecordConsumer handle_perfetto_blob =
      [&saw_perfetto_blob](trace::Record record) {
        if (record.type() == trace::RecordType::kBlob &&
            record.GetBlob().type == TRACE_BLOB_TYPE_PERFETTO) {
          saw_perfetto_blob = true;
        }
      };
  trace::TraceReader reader(std::move(handle_perfetto_blob), [](fbl::String) {});
  trace::Chunk data{reinterpret_cast<uint64_t*>(buffer), actual >> 3};
  reader.ReadRecords(data);
  EXPECT_TRUE(saw_perfetto_blob);

  loop.RunUntilIdle();
}

// TODO(fxb/120485): Add a test to cover calls to Controller::GetKnownCategories once
// perfetto_producer.cc has a working backend. This would test the plumbing all the
// way from perfetto to ffx trace.
// TEST(PerfettoBridgeIntegrationTest, GetKnownCategories)
