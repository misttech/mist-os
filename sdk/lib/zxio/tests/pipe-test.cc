// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.io/cpp/wire_types.h>
#include <lib/zx/socket.h>
#include <lib/zxio/cpp/inception.h>
#include <lib/zxio/zxio.h>
#include <zircon/rights.h>

#include <memory>

#include <zxtest/zxtest.h>

namespace {

TEST(Pipe, Create) {
  zx::socket socket0, socket1;
  ASSERT_OK(zx::socket::create(0u, &socket0, &socket1));

  zxio_storage_t storage;
  ASSERT_OK(zxio_create(socket0.release(), &storage));
  zxio_t* io = &storage.io;

  zxio_node_attributes_t attr = {.has = {.object_type = true}};
  ASSERT_OK(zxio_attr_get(io, &attr));
  EXPECT_EQ(ZXIO_OBJECT_TYPE_PIPE, attr.object_type);
  ASSERT_STATUS(ZX_ERR_NOT_SUPPORTED, zxio_attr_set(io, &attr));

  ASSERT_OK(zxio_close(io, /*should_wait=*/true));
}

TEST(Pipe, CreateWithAllocator) {
  zx::socket socket0, socket1;
  ASSERT_OK(zx::socket::create(0u, &socket0, &socket1));

  auto zxio_allocator = [](zxio_object_type_t type, zxio_storage_t** out_storage,
                           void** out_context) {
    EXPECT_EQ(type, ZXIO_OBJECT_TYPE_PIPE);
    *out_storage = new zxio_storage_t;
    *out_context = *out_storage;
    return ZX_OK;
  };
  void* context = nullptr;
  ASSERT_OK(zxio_create_with_allocator(std::move(socket0), zxio_allocator, &context));
  std::unique_ptr<zxio_storage_t> storage(static_cast<zxio_storage_t*>(context));
  zxio_t* io = &storage->io;

  ASSERT_OK(zxio_close(io, /*should_wait=*/true));
}

TEST(Pipe, FlagsGetDefault) {
  zx::socket socket0, socket1;
  ASSERT_OK(zx::socket::create(0u, &socket0, &socket1));

  zxio_storage_t storage;
  ASSERT_OK(zxio_create(socket0.release(), &storage));
  zxio_t* io = &storage.io;

  // By default, socket supports IO (Read + Write).
  uint64_t raw_flags{};
  ASSERT_OK(zxio_flags_get2(io, &raw_flags));
  fuchsia_io::wire::Flags flags{raw_flags};
  EXPECT_TRUE(flags & fuchsia_io::wire::Flags::kPermRead);
  EXPECT_TRUE(flags & fuchsia_io::wire::Flags::kPermWrite);
}

TEST(Pipe, FlagsGetReadOnly) {
  zx::socket socket0, socket1;
  ASSERT_OK(zx::socket::create(0u, &socket0, &socket1));
  zx::socket duplicate_readonly_socket;
  ASSERT_OK(socket0.duplicate(ZX_RIGHTS_BASIC | ZX_RIGHT_READ, &duplicate_readonly_socket));

  zxio_storage_t storage;
  ASSERT_OK(zxio_create(duplicate_readonly_socket.release(), &storage));
  zxio_t* io = &storage.io;

  uint64_t raw_flags{};
  ASSERT_OK(zxio_flags_get2(io, &raw_flags));
  fuchsia_io::wire::Flags flags{raw_flags};
  EXPECT_TRUE(flags & fuchsia_io::wire::Flags::kPermRead);
  EXPECT_FALSE(flags & fuchsia_io::wire::Flags::kPermWrite);
}

TEST(Pipe, FlagsGetNoIO) {
  zx::socket socket0, socket1;
  ASSERT_OK(zx::socket::create(0u, &socket0, &socket1));
  zx::socket duplicate_socket;
  ASSERT_OK(socket0.duplicate(ZX_RIGHTS_BASIC, &duplicate_socket));

  zxio_storage_t storage;
  ASSERT_OK(zxio_create(duplicate_socket.release(), &storage));
  zxio_t* io = &storage.io;

  uint64_t raw_flags{};
  ASSERT_OK(zxio_flags_get2(io, &raw_flags));
  fuchsia_io::wire::Flags flags{raw_flags};
  EXPECT_FALSE(flags & fuchsia_io::wire::Flags::kPermRead);
  EXPECT_FALSE(flags & fuchsia_io::wire::Flags::kPermWrite);
}

TEST(Pipe, FlagsSetWithValidInputFlags) {
  zx::socket socket0, socket1;
  ASSERT_OK(zx::socket::create(0u, &socket0, &socket1));

  zxio_storage_t storage;
  ASSERT_OK(zxio_create(socket0.release(), &storage));
  zxio_t* io = &storage.io;

  fuchsia_io::wire::Flags flags =
      fuchsia_io::wire::Flags::kPermRead | fuchsia_io::wire::Flags::kPermWrite;
  ASSERT_OK(zxio_flags_set2(io, uint64_t{flags}));
}

TEST(Pipe, FlagsSetWithInvalidInputFlagsIsError) {
  zx::socket socket0, socket1;
  ASSERT_OK(zx::socket::create(0u, &socket0, &socket1));
  zx::socket duplicate_socket;
  ASSERT_OK(socket0.duplicate(ZX_RIGHTS_BASIC | ZX_RIGHT_WRITE, &duplicate_socket));

  zxio_storage_t storage;
  ASSERT_OK(zxio_create(duplicate_socket.release(), &storage));
  zxio_t* io = &storage.io;

  fuchsia_io::wire::Flags flags =
      fuchsia_io::wire::Flags::kPermRead | fuchsia_io::wire::Flags::kPermWrite;
  EXPECT_STATUS(zxio_flags_set2(io, uint64_t{flags}), ZX_ERR_NOT_SUPPORTED);
}

TEST(Pipe, DeprecatedFlagsGetDefault) {
  zx::socket socket0, socket1;
  ASSERT_OK(zx::socket::create(0u, &socket0, &socket1));

  zxio_storage_t storage;
  ASSERT_OK(zxio_create(socket0.release(), &storage));
  zxio_t* io = &storage.io;

  // By default, socket supports IO (Read + Write).
  uint32_t raw_flags{};
  ASSERT_OK(zxio_flags_get(io, &raw_flags));
  fuchsia_io::wire::OpenFlags flags{raw_flags};
  EXPECT_TRUE(flags & fuchsia_io::wire::OpenFlags::kRightReadable);
  EXPECT_TRUE(flags & fuchsia_io::wire::OpenFlags::kRightWritable);
}

TEST(Pipe, DeprecatedFlagsGetReadOnly) {
  zx::socket socket0, socket1;
  ASSERT_OK(zx::socket::create(0u, &socket0, &socket1));
  zx::socket duplicate_readonly_socket;
  ASSERT_OK(socket0.duplicate(ZX_RIGHTS_BASIC | ZX_RIGHT_READ, &duplicate_readonly_socket));

  zxio_storage_t storage;
  ASSERT_OK(zxio_create(duplicate_readonly_socket.release(), &storage));
  zxio_t* io = &storage.io;

  uint32_t raw_flags{};
  ASSERT_OK(zxio_flags_get(io, &raw_flags));
  fuchsia_io::wire::OpenFlags flags{raw_flags};
  EXPECT_TRUE(flags & fuchsia_io::wire::OpenFlags::kRightReadable);
  EXPECT_FALSE(flags & fuchsia_io::wire::OpenFlags::kRightWritable);
}

TEST(Pipe, DeprecatedFlagsGetNoIO) {
  zx::socket socket0, socket1;
  ASSERT_OK(zx::socket::create(0u, &socket0, &socket1));
  zx::socket duplicate_socket;
  ASSERT_OK(socket0.duplicate(ZX_RIGHTS_BASIC, &duplicate_socket));

  zxio_storage_t storage;
  ASSERT_OK(zxio_create(duplicate_socket.release(), &storage));
  zxio_t* io = &storage.io;

  uint32_t raw_flags{};
  ASSERT_OK(zxio_flags_get(io, &raw_flags));
  fuchsia_io::wire::OpenFlags flags{raw_flags};
  EXPECT_FALSE(flags & fuchsia_io::wire::OpenFlags::kRightReadable);
  EXPECT_FALSE(flags & fuchsia_io::wire::OpenFlags::kRightWritable);
}

TEST(Pipe, DeprecatedFlagsSetWithValidInputFlags) {
  zx::socket socket0, socket1;
  ASSERT_OK(zx::socket::create(0u, &socket0, &socket1));

  zxio_storage_t storage;
  ASSERT_OK(zxio_create(socket0.release(), &storage));
  zxio_t* io = &storage.io;

  fuchsia_io::wire::OpenFlags flags =
      fuchsia_io::wire::OpenFlags::kRightReadable | fuchsia_io::wire::OpenFlags::kRightWritable;
  ASSERT_OK(zxio_flags_set(io, static_cast<uint32_t>(flags)));
}

TEST(Pipe, DeprecatedFlagsSetWithInvalidInputFlagsIsError) {
  zx::socket socket0, socket1;
  ASSERT_OK(zx::socket::create(0u, &socket0, &socket1));
  zx::socket duplicate_socket;
  ASSERT_OK(socket0.duplicate(ZX_RIGHTS_BASIC | ZX_RIGHT_WRITE, &duplicate_socket));

  zxio_storage_t storage;
  ASSERT_OK(zxio_create(duplicate_socket.release(), &storage));
  zxio_t* io = &storage.io;

  fuchsia_io::wire::OpenFlags flags =
      fuchsia_io::wire::OpenFlags::kRightReadable | fuchsia_io::wire::OpenFlags::kRightWritable;
  EXPECT_STATUS(zxio_flags_set(io, static_cast<uint32_t>(flags)), ZX_ERR_NOT_SUPPORTED);
}

TEST(Pipe, Basic) {
  zx::socket socket0, socket1;
  ASSERT_OK(zx::socket::create(0u, &socket0, &socket1));

  zxio_storage_t storage;
  ASSERT_OK(zxio_create(socket0.release(), &storage));
  zxio_t* io = &storage.io;

  const uint32_t data = 0x41424344;

  size_t actual = 0u;
  ASSERT_OK(socket1.write(0u, &data, sizeof(data), &actual));
  EXPECT_EQ(actual, sizeof(data));

  uint32_t buffer = 0u;
  ASSERT_OK(zxio_read(io, &buffer, sizeof(buffer), 0u, &actual));
  EXPECT_EQ(actual, sizeof(buffer));
  EXPECT_EQ(buffer, data);

  ASSERT_OK(zxio_close(io, /*should_wait=*/true));
}

TEST(Pipe, GetReadBufferAvailable) {
  zx::socket socket0, socket1;
  ASSERT_OK(zx::socket::create(0u, &socket0, &socket1));

  zxio_storage_t storage;
  ASSERT_OK(zxio_create(socket0.release(), &storage));
  zxio_t* io = &storage.io;

  size_t available = 0;
  ASSERT_OK(zxio_get_read_buffer_available(io, &available));
  EXPECT_EQ(0u, available);

  const uint32_t data = 0x41424344;

  size_t actual = 0u;
  ASSERT_OK(socket1.write(0u, &data, sizeof(data), &actual));
  EXPECT_EQ(actual, sizeof(data));

  ASSERT_OK(zxio_get_read_buffer_available(io, &available));
  EXPECT_EQ(sizeof(data), available);

  uint32_t buffer = 0u;
  ASSERT_OK(zxio_read(io, &buffer, sizeof(buffer), 0u, &actual));
  EXPECT_EQ(actual, sizeof(buffer));

  ASSERT_OK(zxio_get_read_buffer_available(io, &available));
  EXPECT_EQ(0u, available);

  ASSERT_OK(zxio_close(io, /*should_wait=*/true));
}

// Test that after shutting a pipe endpoint down for reading that reading from
// that endpoint and writing to the peer endpoint fail.
TEST(Pipe, ShutdownRead) {
  zx::socket socket0, socket1;
  ASSERT_OK(zx::socket::create(0u, &socket0, &socket1));

  zxio_storage_t storage;
  ASSERT_OK(zxio_create(socket0.release(), &storage));
  zxio_t* io = &storage.io;

  const uint32_t data = 0x41424344;

  // Write some data before shutting down reading on the peer. Should succeed.
  size_t actual = 0u;
  EXPECT_OK(socket1.write(0u, &data, sizeof(data), &actual));
  EXPECT_EQ(actual, 4u);
  actual = 0u;

  int16_t out_code;
  EXPECT_OK(zxio_shutdown(io, ZXIO_SHUTDOWN_OPTIONS_READ, &out_code));
  EXPECT_EQ(out_code, 0);

  // We shouldn't be able to write any more data into the peer.
  EXPECT_STATUS(socket1.write(0u, &data, sizeof(data), &actual), ZX_ERR_BAD_STATE);
  EXPECT_EQ(actual, 0u);
  actual = 0u;

  char buf[4] = {};
  // We should be able to read data written into the pipe before reading was
  // disabled.
  EXPECT_OK(zxio_read(io, buf, sizeof(buf), 0u, &actual));
  EXPECT_EQ(actual, 4u);
  actual = 0u;

  EXPECT_STATUS(zxio_read(io, buf, sizeof(buf), 0u, &actual), ZX_ERR_BAD_STATE);
  EXPECT_EQ(actual, 0u);
  actual = 0u;

  ASSERT_OK(zxio_close(io, /*should_wait=*/true));
}

// Test that after shutting a pipe endpoint down for writing that writing to
// that endpoint and reading from the peer endpoint fail.
TEST(Pipe, ShutdownWrite) {
  zx::socket socket0, socket1;
  ASSERT_OK(zx::socket::create(0u, &socket0, &socket1));

  zxio_storage_t storage;
  ASSERT_OK(zxio_create(socket0.release(), &storage));
  zxio_t* io = &storage.io;

  int16_t out_code;
  EXPECT_OK(zxio_shutdown(io, ZXIO_SHUTDOWN_OPTIONS_WRITE, &out_code));
  EXPECT_EQ(out_code, 0);

  size_t actual = 0u;

  char buf[4] = {};
  EXPECT_STATUS(socket1.read(0u, &buf, sizeof(buf), &actual), ZX_ERR_BAD_STATE);
  EXPECT_EQ(actual, 0u);
  actual = 0u;

  const uint32_t data = 0x41424344;

  EXPECT_STATUS(zxio_write(io, &data, sizeof(data), 0u, &actual), ZX_ERR_BAD_STATE);
  EXPECT_EQ(actual, 0u);

  ASSERT_OK(zxio_close(io, /*should_wait=*/true));
}

// Test that after shutting a pipe endpoint down for reading and writing that
// reading or writing to either endpoint fails.
TEST(Pipe, ShutdownReadWrite) {
  zx::socket socket0, socket1;
  ASSERT_OK(zx::socket::create(0u, &socket0, &socket1));

  zxio_storage_t storage;
  ASSERT_OK(zxio_create(socket0.release(), &storage));
  zxio_t* io = &storage.io;

  const uint32_t data = 0x41424344;

  // Write some data before shutting down the peer. Should succeed.
  size_t actual = 0u;
  EXPECT_OK(socket1.write(0u, &data, sizeof(data), &actual));
  EXPECT_EQ(actual, 4u);
  actual = 0u;

  int16_t out_code;
  EXPECT_OK(zxio_shutdown(io, ZXIO_SHUTDOWN_OPTIONS_READ | ZXIO_SHUTDOWN_OPTIONS_WRITE, &out_code));
  EXPECT_EQ(out_code, 0);

  char buf[4] = {};
  EXPECT_STATUS(socket1.read(0u, &buf, sizeof(buf), &actual), ZX_ERR_BAD_STATE);
  EXPECT_EQ(actual, 0u);
  actual = 0u;

  EXPECT_STATUS(socket1.write(0u, &data, sizeof(data), &actual), ZX_ERR_BAD_STATE);
  EXPECT_EQ(actual, 0u);
  actual = 0u;

  // We should be able to read data written into the pipe before reading was
  // disabled.
  EXPECT_OK(zxio_read(io, buf, sizeof(buf), 0u, &actual));
  EXPECT_EQ(actual, 4u);
  actual = 0u;

  EXPECT_STATUS(zxio_read(io, buf, sizeof(buf), 0u, &actual), ZX_ERR_BAD_STATE);
  EXPECT_EQ(actual, 0u);
  actual = 0u;

  EXPECT_STATUS(zxio_write(io, &data, sizeof(data), 0u, &actual), ZX_ERR_BAD_STATE);
  EXPECT_EQ(actual, 0u);

  ASSERT_OK(zxio_close(io, /*should_wait=*/true));
}

}  // namespace
