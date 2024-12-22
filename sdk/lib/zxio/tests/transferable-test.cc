// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.pty/cpp/wire_test_base.h>
#include <fidl/fuchsia.unknown/cpp/wire_test_base.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/zxio/zxio.h>

#include <zxtest/zxtest.h>

class TransferableServer : public fidl::testing::WireTestBase<fuchsia_hardware_pty::Device> {
 public:
  TransferableServer(async_dispatcher_t* dispatcher) : dispatcher_(dispatcher) {}

  void NotImplemented_(const std::string& name, fidl::CompleterBase& completer) final {
    ADD_FAILURE() << "unexpected message received: " << name;
    completer.Close(ZX_ERR_NOT_SUPPORTED);
  }

  void Close(CloseCompleter::Sync& completer) final {
    completer.ReplySuccess();
    // After the reply, we should close the connection.
    completer.Close(ZX_OK);
  }

  void Clone(CloneRequestView request, CloneCompleter::Sync& completer) final {
    fidl::BindServer(dispatcher_,
                     fidl::ServerEnd<fuchsia_hardware_pty::Device>(request->request.TakeChannel()),
                     this);
  }

 private:
  async_dispatcher_t* dispatcher_;
};

TEST(Transferable, Clone) {
  auto [device_client, device_server] = fidl::Endpoints<fuchsia_hardware_pty::Device>::Create();

  async::Loop device_control_loop(&kAsyncLoopConfigNoAttachToCurrentThread);
  TransferableServer server(device_control_loop.dispatcher());
  fidl::BindServer(device_control_loop.dispatcher(), std::move(device_server), &server);
  device_control_loop.StartThread("device_control_thread");

  zxio_storage_t storage;
  ASSERT_OK(zxio_create_with_type(&storage, ZXIO_OBJECT_TYPE_TRANSFERABLE,
                                  device_client.TakeChannel().release(), &storage));
  zxio_t* io = &storage.io;

  zx::channel clone;
  EXPECT_OK(zxio_clone(io, clone.reset_and_get_address()));

  fidl::ClientEnd<fuchsia_unknown::Closeable> clone_client(std::move(clone));

  const fidl::WireResult response = fidl::WireCall(clone_client)->Close();
  ASSERT_OK(response.status());

  EXPECT_OK(zxio_close(io, true));
}

TEST(Transferable, FlagsGetDefault) {
  auto [device_client, device_server] = fidl::Endpoints<fuchsia_hardware_pty::Device>::Create();

  zxio_storage_t storage;
  ASSERT_OK(zxio_create_with_type(&storage, ZXIO_OBJECT_TYPE_TRANSFERABLE,
                                  device_client.TakeChannel().release(), &storage));
  zxio_t* io = &storage.io;
  // By default, transferable supports IO (Read + Write).
  uint32_t raw_flags{};
  ASSERT_OK(zxio_flags_get(io, &raw_flags));
  fuchsia_io::wire::OpenFlags flags{raw_flags};
  EXPECT_TRUE(flags & fuchsia_io::wire::OpenFlags::kRightReadable);
  EXPECT_TRUE(flags & fuchsia_io::wire::OpenFlags::kRightWritable);
}

TEST(Transferable, FlagsSet) {
  auto [device_client, device_server] = fidl::Endpoints<fuchsia_hardware_pty::Device>::Create();

  zxio_storage_t storage;
  ASSERT_OK(zxio_create_with_type(&storage, ZXIO_OBJECT_TYPE_TRANSFERABLE,
                                  device_client.TakeChannel().release(), &storage));
  zxio_t* io = &storage.io;
  fuchsia_io::wire::OpenFlags flags =
      fuchsia_io::wire::OpenFlags::kRightReadable | fuchsia_io::wire::OpenFlags::kRightWritable;
  ASSERT_OK(zxio_flags_set(io, static_cast<uint32_t>(flags)));
}
