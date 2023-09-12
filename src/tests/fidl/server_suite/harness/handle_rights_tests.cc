// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/zx/port.h>

#include "src/tests/fidl/server_suite/harness/harness.h"
#include "src/tests/fidl/server_suite/harness/ordinals.h"

namespace server_suite {
namespace {

using namespace ::channel_util;

// The server should tear down when the request is missing a handle.
CLOSED_SERVER_TEST(ClientSendsTooFewHandles) {
  zx::port port;
  ASSERT_OK(zx::port::create(0, &port));

  Bytes bytes = {
      header(kTwoWayTxid, kOrdinalGetSignalableEventRights,
             fidl::MessageDynamicFlags::kStrictMethod),
      handle_present(),
      padding(4),
  };
  ASSERT_OK(client_end().write(bytes));

  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_PEER_CLOSED));
  ASSERT_FALSE(client_end().is_signal_present(ZX_CHANNEL_READABLE));
}

// The server should tear down when it receives the wrong handle type.
CLOSED_SERVER_TEST(ClientSendsWrongHandleType) {
  zx::port port;
  ASSERT_OK(zx::port::create(0, &port));

  Bytes bytes = {
      header(kTwoWayTxid, kOrdinalGetSignalableEventRights,
             fidl::MessageDynamicFlags::kStrictMethod),
      handle_present(),
      padding(4),
  };
  HandleDispositions hd_in = {
      zx_handle_disposition_t{
          .handle = port.release(),
          .type = ZX_OBJ_TYPE_PORT,
          .rights = ZX_RIGHT_SAME_RIGHTS,
      },
  };
  ASSERT_OK(client_end().write(bytes, hd_in));

  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_PEER_CLOSED));
  ASSERT_FALSE(client_end().is_signal_present(ZX_CHANNEL_READABLE));
}

// When a handle with too many rights is sent, the rights should be reduced.
CLOSED_SERVER_TEST(ClientSendsTooManyRights) {
  zx::event event;
  ASSERT_OK(zx::event::create(0, &event));

  // Validate that more rights than just ZX_RIGHT_SIGNAL are present.
  zx_info_handle_basic_t info;
  ASSERT_OK(event.get_info(ZX_INFO_HANDLE_BASIC, &info, sizeof(info), nullptr, nullptr));
  ASSERT_EQ(ZX_DEFAULT_EVENT_RIGHTS, info.rights);
  static_assert(ZX_DEFAULT_EVENT_RIGHTS & ZX_RIGHT_SIGNAL);
  static_assert(ZX_DEFAULT_EVENT_RIGHTS & ~ZX_RIGHT_SIGNAL);

  Bytes bytes_in = {
      header(kTwoWayTxid, kOrdinalGetSignalableEventRights,
             fidl::MessageDynamicFlags::kStrictMethod),
      handle_present(),
      padding(4),
  };
  HandleDispositions hd_in = {
      zx_handle_disposition_t{
          .handle = event.release(),
          .type = ZX_OBJ_TYPE_EVENT,
          .rights = ZX_RIGHT_SAME_RIGHTS,
      },
  };
  ASSERT_OK(client_end().write(bytes_in, hd_in));

  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_READABLE));

  Bytes bytes_out = {
      header(kTwoWayTxid, kOrdinalGetSignalableEventRights,
             fidl::MessageDynamicFlags::kStrictMethod),
      u32(ZX_RIGHT_SIGNAL),
      padding(4),
  };
  ASSERT_OK(client_end().read_and_check(bytes_out));
}

// The server should tear down when it receives a handle with too few rights.
CLOSED_SERVER_TEST(ClientSendsTooFewRights) {
  zx::event event;
  ASSERT_OK(zx::event::create(0, &event));

  zx::event reduced_rights_event;
  ASSERT_OK(event.replace(ZX_RIGHT_TRANSFER, &reduced_rights_event));

  Bytes bytes_in = {
      header(kTwoWayTxid, kOrdinalGetSignalableEventRights,
             fidl::MessageDynamicFlags::kStrictMethod),
      handle_present(),
      padding(4),
  };
  HandleDispositions hd_in = {
      zx_handle_disposition_t{
          .handle = reduced_rights_event.release(),
          .type = ZX_OBJ_TYPE_EVENT,
          .rights = ZX_RIGHT_SAME_RIGHTS,
      },
  };
  ASSERT_OK(client_end().write(bytes_in, hd_in));

  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_PEER_CLOSED));
  ASSERT_FALSE(client_end().is_signal_present(ZX_CHANNEL_READABLE));
}

// The server should handle ZX_OBJ_TYPE_NONE and ZX_RIGHT_SAME_RIGHTS correctly.
// ZX_OBJ_TYPE_NONE means "any object type is allowed".
// ZX_RIGHT_SAME_RIGHTS means "any rights are allowed".
CLOSED_SERVER_TEST(ClientSendsObjectOverPlainHandle) {
  zx::event event;
  ASSERT_OK(zx::event::create(0, &event));

  Bytes bytes_in = {
      header(kTwoWayTxid, kOrdinalGetHandleRights, fidl::MessageDynamicFlags::kStrictMethod),
      handle_present(),
      padding(4),
  };
  HandleDispositions hd_in = {
      zx_handle_disposition_t{
          .handle = event.release(),
          .type = ZX_OBJ_TYPE_EVENT,
          .rights = ZX_RIGHT_SAME_RIGHTS,
      },
  };
  ASSERT_OK(client_end().write(bytes_in, hd_in));

  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_READABLE));

  Bytes bytes_out = {
      header(kTwoWayTxid, kOrdinalGetHandleRights, fidl::MessageDynamicFlags::kStrictMethod),
      u32(ZX_DEFAULT_EVENT_RIGHTS),
      padding(4),
  };
  ASSERT_OK(client_end().read_and_check(bytes_out));
}

// The server should tear down when it tries to send the wrong handle type.
CLOSED_SERVER_TEST(ServerSendsWrongHandleType) {
  zx::port port;
  ASSERT_OK(zx::port::create(0, &port));

  Bytes bytes = {
      header(kTwoWayTxid, kOrdinalEchoAsTransferableSignalableEvent,
             fidl::MessageDynamicFlags::kStrictMethod),
      handle_present(),
      padding(4),
  };

  ASSERT_OK(client_end().write(bytes));

  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_PEER_CLOSED));
  ASSERT_FALSE(client_end().is_signal_present(ZX_CHANNEL_READABLE));
}

// When the server sends a handle with too many rights, the rights should be reduced.
CLOSED_SERVER_TEST(ServerSendsTooManyRights) {
  zx::event event;
  ASSERT_OK(zx::event::create(0, &event));

  // Validate that more rights than just ZX_RIGHT_SIGNAL are present.
  zx_info_handle_basic_t info;
  ASSERT_OK(event.get_info(ZX_INFO_HANDLE_BASIC, &info, sizeof(info), nullptr, nullptr));
  ASSERT_EQ(ZX_DEFAULT_EVENT_RIGHTS, info.rights);
  static_assert(ZX_DEFAULT_EVENT_RIGHTS & ZX_RIGHT_SIGNAL);
  static_assert(ZX_DEFAULT_EVENT_RIGHTS & ~ZX_RIGHT_SIGNAL);

  Bytes bytes_in = {
      header(kTwoWayTxid, kOrdinalEchoAsTransferableSignalableEvent,
             fidl::MessageDynamicFlags::kStrictMethod),
      handle_present(),
      padding(4),
  };
  HandleDispositions hd_in = {
      zx_handle_disposition_t{
          .handle = event.release(),
          .type = ZX_OBJ_TYPE_EVENT,
          .rights = ZX_RIGHT_SAME_RIGHTS,
      },
  };
  ASSERT_OK(client_end().write(bytes_in, hd_in));

  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_READABLE));

  Bytes bytes_out = {
      header(kTwoWayTxid, kOrdinalEchoAsTransferableSignalableEvent,
             fidl::MessageDynamicFlags::kStrictMethod),
      handle_present(),
      padding(4),
  };
  HandleInfos handles_out = {
      zx_handle_info_t{
          .type = ZX_OBJ_TYPE_EVENT,
          .rights = ZX_RIGHT_SIGNAL | ZX_RIGHT_TRANSFER,
      },
  };
  ASSERT_OK(client_end().read_and_check(bytes_out, handles_out));
}

// The server should tear down when it tries to send a handle with too few rights.
CLOSED_SERVER_TEST(ServerSendsTooFewRights) {
  zx::event event;
  ASSERT_OK(zx::event::create(0, &event));

  zx::event reduced_rights_event;
  ASSERT_OK(event.replace(ZX_RIGHT_TRANSFER, &reduced_rights_event));

  Bytes bytes_in = {
      header(kTwoWayTxid, kOrdinalEchoAsTransferableSignalableEvent,
             fidl::MessageDynamicFlags::kStrictMethod),
      handle_present(),
      padding(4),
  };
  HandleDispositions hd_in = {
      zx_handle_disposition_t{
          .handle = reduced_rights_event.release(),
          .type = ZX_OBJ_TYPE_EVENT,
          .rights = ZX_RIGHT_SAME_RIGHTS,
      },
  };
  ASSERT_OK(client_end().write(bytes_in, hd_in));

  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_PEER_CLOSED));
  ASSERT_FALSE(client_end().is_signal_present(ZX_CHANNEL_READABLE));
}

}  // namespace
}  // namespace server_suite
