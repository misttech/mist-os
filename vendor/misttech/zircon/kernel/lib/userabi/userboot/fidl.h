// Copyright 2024 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/fidl/txn_header.h>
#include <lib/zx/channel.h>
#include <lib/zx/result.h>
#include <zircon/fidl.h>

// TODO(https://fxbug.dev/42072759): Replace copy & pasted FIDL C bindings with new C++ bindings
// when that's allowed.
constexpr std::string_view kUserbootProtocolName = "fuchsia.boot.Userboot";
constexpr std::string_view kSvcStashProtocolName = "fuchsia.boot.SvcStash";

struct fuchsia_boot_SvcStashStoreRequestMessage {
  FIDL_ALIGNDECL
  fidl_message_header_t hdr;
  zx_handle_t svc_endpoint;
};

constexpr uint64_t fuchsia_boot_SvcStashStoreOrdinal = 0xC2648E356CA2870;

inline zx::result<> SvcStashStore(const zx::channel& svc_stash_client_endpoint,
                                  zx::channel svc_server_endpoint) {
  zx_handle_t h = svc_server_endpoint.release();
  fuchsia_boot_SvcStashStoreRequestMessage request = {};
  fidl_init_txn_header(&request.hdr, 0, fuchsia_boot_SvcStashStoreOrdinal, 0);
  request.svc_endpoint = FIDL_HANDLE_PRESENT;
  return zx::make_result(svc_stash_client_endpoint.write(0, &request, sizeof(request), &h, 1));
}

struct fuchsia_boot_UserbootPushRequestMessage {
  FIDL_ALIGNDECL
  fidl_message_header_t hdr;
  zx_handle_t stash_svc_endpoint;
};

constexpr uint64_t fuchsia_boot_UserbootPushStashSvcOrdinal = 0x506ECF7DB01ADEAC;

inline zx::result<> UserbootPostStashSvc(const zx::channel& userboot_client_endpoint,
                                         zx::channel svc_stash_server_endpoint) {
  zx_handle_t h = svc_stash_server_endpoint.release();
  fuchsia_boot_UserbootPushRequestMessage request = {};
  fidl_init_txn_header(&request.hdr, 0, fuchsia_boot_UserbootPushStashSvcOrdinal, 0);
  request.stash_svc_endpoint = FIDL_HANDLE_PRESENT;
  return zx::make_result(userboot_client_endpoint.write(0, &request, sizeof(request), &h, 1));
}
