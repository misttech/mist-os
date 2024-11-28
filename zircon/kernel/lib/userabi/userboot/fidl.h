// Copyright 2024 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_LIB_USERABI_USERBOOT_FIDL_H_
#define ZIRCON_KERNEL_LIB_USERABI_USERBOOT_FIDL_H_

#include <lib/fidl/txn_header.h>
#include <lib/zx/channel.h>
#include <lib/zx/result.h>
#include <lib/zx/vmo.h>
#include <zircon/fidl.h>

#include <span>

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

struct BootfsFileVmo {
  uint32_t offset;
  zx::vmo contents;
};

struct fuchsia_boot_BootfsFileVmo {
  FIDL_ALIGNDECL
  uint32_t offset;
  zx_handle_t contents;
};
struct fuchsia_boot_PostBootfsFilesRequestMessage {
  FIDL_ALIGNDECL
  fidl_message_header_t hdr;
  fidl_vector_t vector;
};

constexpr uint64_t fuchsia_boot_UserbootPostBootfsFilesOrdinal = 2985117035928536724l;

inline zx::result<> UserbootPostBootfsEntries(const zx::channel& userboot_client_endpoint,
                                              std::span<BootfsFileVmo> entries) {
  if (entries.empty()) {
    return zx::ok();
  }
  std::array<uint8_t,
             FIDL_ALIGN(sizeof(fuchsia_boot_PostBootfsFilesRequestMessage)) +
                 FIDL_ALIGN(sizeof(fuchsia_boot_BootfsFileVmo) * ZX_CHANNEL_MAX_MSG_HANDLES)>
      buffer;
  std::array<zx_handle_t, ZX_CHANNEL_MAX_MSG_HANDLES> handles;

  auto* request = reinterpret_cast<fuchsia_boot_PostBootfsFilesRequestMessage*>(buffer.data());
  fidl_init_txn_header(&request->hdr, 0, fuchsia_boot_UserbootPostBootfsFilesOrdinal, 0);

  // Aligned main object and secondary object.
  // | request        | payload     |
  // | hdr | vector_t | vector data |
  auto* raw_entries_ptr = buffer.data() + sizeof(fuchsia_boot_PostBootfsFilesRequestMessage);
  auto* typed_entries = reinterpret_cast<fuchsia_boot_BootfsFileVmo*>(raw_entries_ptr);
  for (size_t i = 0; i < entries.size(); ++i) {
    typed_entries[i].contents = FIDL_HANDLE_PRESENT;
    typed_entries[i].offset = entries[i].offset;
    handles[i] = entries[i].contents.release();
  }
  size_t num_bytes = FIDL_ALIGN(reinterpret_cast<uintptr_t>(typed_entries + entries.size())) -
                     reinterpret_cast<uintptr_t>(buffer.data());
  request->vector.count = entries.size();
  // Turn this into a flat offset from message start.
  request->vector.data = reinterpret_cast<void*>(FIDL_ALLOC_PRESENT);
  return zx::make_result(
      userboot_client_endpoint.write(0, buffer.data(), static_cast<uint32_t>(num_bytes),
                                     handles.data(), static_cast<uint32_t>(entries.size())));
}

#endif  // ZIRCON_KERNEL_LIB_USERABI_USERBOOT_FIDL_H_
