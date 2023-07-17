// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_TESTS_FIDL_CHANNEL_UTIL_CHANNEL_H_
#define SRC_TESTS_FIDL_CHANNEL_UTIL_CHANNEL_H_

#include <lib/zx/channel.h>
#include <lib/zx/result.h>
#include <lib/zx/vmo.h>
#include <zircon/assert.h>
#include <zircon/fidl.h>

#include <algorithm>
#include <iostream>
#include <utility>

#include "src/tests/fidl/channel_util/bytes.h"

namespace channel_util {

// Non-owning handle types for channel read/write.
using HandleDispositions = std::vector<zx_handle_disposition_t>;
using HandleInfos = std::vector<zx_handle_info_t>;

// Value that can be set in the header as a placeholder when passing Bytes to
// read_and_check_unknown_txid. When using unknown_txid mode, the txid in the
// expected bytes is ignored; this constant can be used to document the fact
// that the txid value used in the expected bytes is a marker for an unknown
// value.
static const zx_txid_t kTxidNotKnown = 0;

// A value to use when constructing a header with an invalid magic number.
static const uint8_t kBadMagicNumber = 87;

const zx_rights_t kOverflowBufferRights =
    ZX_RIGHT_GET_PROPERTY | ZX_RIGHT_READ | ZX_RIGHT_TRANSFER | ZX_RIGHT_WAIT | ZX_RIGHT_INSPECT;

class Channel {
 public:
  Channel() = default;
  Channel(Channel&&) = default;
  Channel& operator=(Channel&&) = default;

  explicit Channel(zx::channel channel) : channel_(std::move(channel)) {}

  // Create a VMO, write to it, then append the handle representing that VMO to the passed in handle
  // list.
  static zx_status_t write_overflow_vmo(const Bytes& overflow_bytes,
                                        HandleDispositions& handle_dispositions) {
    zx::vmo vmo;
    zx::vmo::create(overflow_bytes.size(), 0, &vmo);
    zx_status_t status = vmo.write(overflow_bytes.data(), 0, overflow_bytes.size());
    handle_dispositions.push_back(zx_handle_disposition_t{
        .operation = ZX_HANDLE_OP_MOVE,
        .handle = vmo.release(),
        .type = ZX_OBJ_TYPE_VMO,
        .rights = kOverflowBufferRights,
    });
    return status;
  }

  zx_status_t write_with_overflow(const Bytes& channel_bytes, const Bytes& overflow_bytes,
                                  HandleDispositions handle_dispositions = {}) {
    ZX_ASSERT_MSG(channel_bytes.size() % FIDL_ALIGNMENT == 0,
                  "channel bytes must be 8-byte aligned");
    ZX_ASSERT_MSG(overflow_bytes.size() % FIDL_ALIGNMENT == 0,
                  "overflow bytes must be 8-byte aligned");
    ZX_ASSERT_MSG(handle_dispositions.size() < 64, "cannot pass more than 63 handles here");

    zx_status_t status = write_overflow_vmo(overflow_bytes, handle_dispositions);
    if (status != ZX_OK) {
      return status;
    }

    return channel_.write_etc(0, channel_bytes.data(), static_cast<uint32_t>(channel_bytes.size()),
                              const_cast<zx_handle_disposition_t*>(handle_dispositions.data()),
                              static_cast<uint32_t>(handle_dispositions.size()));
  }

  zx_status_t write(const Bytes& bytes, const HandleDispositions& handle_dispositions = {}) {
    ZX_ASSERT_MSG(0 == bytes.size() % FIDL_ALIGNMENT, "bytes must be 8-byte aligned");
    return channel_.write_etc(0, bytes.data(), static_cast<uint32_t>(bytes.size()),
                              const_cast<zx_handle_disposition_t*>(handle_dispositions.data()),
                              static_cast<uint32_t>(handle_dispositions.size()));
  }

  zx_status_t wait_for_signal(zx_signals_t signal) {
    ZX_ASSERT_MSG(__builtin_popcount(signal) == 1, "wait_for_signal expects exactly 1 signal");
    return channel_.wait_one(signal, zx::time::infinite(), nullptr);
  }

  bool is_signal_present(zx_signals_t signal) {
    ZX_ASSERT_MSG(__builtin_popcount(signal) == 1, "wait_for_signal expects exactly 1 signal");
    return ZX_OK == channel_.wait_one(signal, zx::time::infinite_past(), nullptr);
  }

  zx_status_t read_and_check(const Bytes& expected, const HandleInfos& expected_handles = {}) {
    return read_and_check_impl(expected, expected_handles);
  }

  zx_status_t read_and_check_with_overflow(const Bytes& expected_channel, const Bytes& expected_vmo,
                                           const HandleInfos& expected_handles) {
    zx_txid_t out_unknown_txid;
    uint64_t overflow_size;
    std::vector<zx_handle_info_t> handles;
    zx_status_t status = read_and_check_impl(expected_channel, expected_handles, &out_unknown_txid,
                                             &overflow_size, &handles);
    if (status != ZX_OK) {
      // No need to print output, as |read_and_check_impl()| will have printed more precise info
      // anyway.
      return status;
    }

    zx_handle_info_t& overflow_buffer_handle = handles.back();
    if (overflow_buffer_handle.type != ZX_OBJ_TYPE_VMO) {
      status = ZX_ERR_WRONG_TYPE;
      std::cerr << "read_and_check_with_overflow: last handle is not VMO, is :"
                << overflow_buffer_handle.type << std::endl;
    }
    if (overflow_buffer_handle.rights != kOverflowBufferRights) {
      status = ZX_ERR_BAD_STATE;
      std::cerr << "read_and_check_with_overflow: handle rights are incorrect, bit array value is :"
                << overflow_buffer_handle.rights << std::endl;
    }
    if (expected_vmo.size() != overflow_size) {
      status = ZX_ERR_INVALID_ARGS;
      std::cerr << "read_and_check_with_overflow: num expected bytes: " << expected_vmo.size()
                << " num actual bytes: " << overflow_size << std::endl;
    }

    // Read the VMO, and validate contents.
    auto vmo = zx::vmo(overflow_buffer_handle.handle);
    std::vector<uint8_t> bytes(overflow_size);
    vmo.read(bytes.data(), 0, overflow_size);

    zx_status_t comparison_status =
        compare_bytes(expected_vmo, &out_unknown_txid, bytes.size(), bytes.data());
    if (comparison_status != ZX_OK) {
      status = comparison_status;
      std::cerr << "read_and_check_with_overflow: bytes mismatch: " << comparison_status
                << std::endl;
    }

    return status;
  }

  zx_status_t read_and_check_unknown_txid(zx_txid_t* out_txid, const Bytes& expected,
                                          const HandleInfos& expected_handles = {}) {
    return read_and_check_impl(expected, expected_handles, out_txid);
  }

  zx::channel& get() { return channel_; }
  void reset() { channel_.reset(); }

 private:
  zx_status_t read_and_check_impl(const Bytes& expected, const HandleInfos& expected_handles,
                                  zx_txid_t* out_unknown_txid = nullptr,
                                  uint64_t* out_overflow_size = nullptr,
                                  std::vector<zx_handle_info_t>* out_handles = nullptr) {
    ZX_ASSERT_MSG(0 == expected.size() % 8, "bytes must be 8-byte aligned");
    uint8_t bytes[ZX_CHANNEL_MAX_MSG_BYTES];
    zx_handle_info_t handles[ZX_CHANNEL_MAX_MSG_HANDLES];
    uint32_t actual_bytes;
    uint32_t actual_handles;
    zx_status_t status = channel_.read_etc(0, bytes, handles, std::size(bytes), std::size(handles),
                                           &actual_bytes, &actual_handles);
    if (status != ZX_OK) {
      std::cerr << "read_and_check*: channel read() returned status code: " << status << std::endl;
      return status;
    }
    if (out_unknown_txid != nullptr) {
      // If out_unknown_txid is non-null, we need to retrieve the txid.
      if (actual_bytes < sizeof(fidl_message_header_t)) {
        std::cerr << "read_and_check*: message body smaller than FIDL message header" << std::endl;
        return ZX_ERR_INVALID_ARGS;
      }
      fidl_message_header_t hdr;
      memcpy(&hdr, bytes, sizeof(fidl_message_header_t));
      *out_unknown_txid = hdr.txid;
    }
    if (expected_handles.size() != actual_handles) {
      status = ZX_ERR_INVALID_ARGS;
      std::cerr << "read_and_check*: num expected handles: " << expected_handles.size()
                << " num actual handles: " << actual_handles << std::endl;
      return ZX_ERR_INVALID_ARGS;
    }

    zx_status_t comparison_status = compare_bytes(expected, out_unknown_txid, actual_bytes, bytes);
    if (comparison_status != ZX_OK) {
      status = comparison_status;
      std::cerr << "read_and_check*: bytes mismatch: " << comparison_status << std::endl;
    }

    for (uint32_t i = 0;
         i < std::min(static_cast<uint32_t>(expected_handles.size()), actual_handles); i++) {
      // Sanity checks. These should always be true for a handle sent over a channel.
      ZX_ASSERT(ZX_HANDLE_INVALID != handles[i].handle);
      ZX_ASSERT(0 == handles[i].unused);

      // Ensure rights and object type match expectations.
      if (expected_handles[i].rights != handles[i].rights) {
        status = ZX_ERR_INVALID_ARGS;
        std::cerr << std::dec << "read_and_check: handles[" << i << "].rights != expected_handles["
                  << i << "].rights: 0x" << std::hex << expected_handles[i].rights << " != 0x"
                  << handles[i].rights << std::endl;
      }
      if (expected_handles[i].type != handles[i].type) {
        status = ZX_ERR_INVALID_ARGS;
        std::cerr << std::dec << "read_and_check: handles[" << i << "].type != expected_handles["
                  << i << "].type: 0x" << std::hex << expected_handles[i].type << " != 0x"
                  << handles[i].type << std::endl;
      }
    }

    if (out_handles != nullptr) {
      for (size_t i = 0; i < actual_handles; i++) {
        out_handles->push_back(handles[i]);
      }
    }
    if (out_overflow_size != nullptr) {
      std::memcpy(out_overflow_size, bytes + 24, sizeof(uint64_t));
    }
    return status;
  }

  static zx_status_t compare_bytes(const Bytes& expected, const zx_txid_t* out_unknown_txid,
                                   uint64_t actual_bytes, const uint8_t* bytes) {
    zx_status_t status = ZX_OK;
    if (expected.size() != actual_bytes) {
      status = ZX_ERR_INVALID_ARGS;
      std::cerr << "read_and_check*: num expected bytes: " << expected.size()
                << " num actual bytes: " << actual_bytes << std::endl;
    }

    for (uint32_t i = 0; i < std::min(static_cast<uint64_t>(expected.size()), actual_bytes); i++) {
      constexpr uint32_t kTxidOffset = offsetof(fidl_message_header_t, txid);
      if (out_unknown_txid != nullptr && i >= kTxidOffset &&
          i < kTxidOffset + sizeof(fidl_message_header_t::txid)) {
        // If out_unknown_txid is non-null, the txid value is unknown so it shouldn't be checked.
        continue;
      }
      if (expected.data()[i] != bytes[i]) {
        status = ZX_ERR_INVALID_ARGS;
        std::cerr << std::dec << "read_and_check: bytes[" << i << "] != expected[" << i << "]: 0x"
                  << std::hex << +bytes[i] << " != 0x" << +expected.data()[i] << std::endl;
      }
    }
    return status;
  }

  zx::channel channel_;
};

}  // namespace channel_util

#endif  // SRC_TESTS_FIDL_CHANNEL_UTIL_CHANNEL_H_
