// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_FIRMWARE_GIGABOOT_CPP_NETWORK_H_
#define SRC_FIRMWARE_GIGABOOT_CPP_NETWORK_H_

#include <lib/fit/result.h>
#include <lib/stdcompat/span.h>

#include <array>
#include <cstdint>

#include <efi/protocol/managed-network.h>
#include <phys/efi/protocol.h>

#include "utils.h"

namespace gigaboot {

constexpr size_t kMacAddrLen = 6;

using MacAddr = std::array<uint8_t, kMacAddrLen>;

class EthernetAgent {
 public:
  // Create and return an EthernetAgent, or an error if construction fails.
  // The EthernetAgent manages transmission and configuration of a single interface
  // via the ManagedNetworkProtocol.
  //
  // Construction may fail for any of the following reasons:
  // *) There are no interfaces
  // *) No interface has connected media
  // *) Failure to determine configuration for an interface
  // *) Failure to configure an interface
  static fit::result<efi_status, EthernetAgent> Create();

  // No default ctor or copy.
  // The whole point of an agent is that it is the sole owner of a managed protocol.
  EthernetAgent() = delete;
  EthernetAgent(const EthernetAgent&) = delete;
  EthernetAgent& operator=(const EthernetAgent&) = delete;

  // We do want move because of the factory function.
  EthernetAgent(EthernetAgent&&) = default;
  EthernetAgent& operator=(EthernetAgent&&) = default;

  constexpr MacAddr Addr() const { return mac_addr_; }

  // Transmit an IPv6 packet.
  //
  // Note: the only reason that this method specifically transmits an IPv6 packet is
  //       because it sets the ethertype to be that of IPv6. All v6 header data
  //       must be set in the payload by the caller.
  //
  // Parameters:
  // dst: the MAC address of the destination.
  //      The address can be unicast, multicast, or broadcast.
  //
  // data: the IPv6 packet to transmit. This must include the v6 header,
  //       any higher protocol headers, and any payload data.
  //
  // callback: a UEFI event to be signaled once the data has been transmitted.
  //           It must be created with a priority of TPL_CALLBACK and must be of type
  //           EVT_NOTIFY_SIGNAL.
  //           The event callback with associated context can be used to e.g. set a timer,
  //           update a counter, or clean up a resource.
  //
  //           See the documentation for EFI_BOOT_SERVICES.CreateEvent for more details
  //           on creating and using signalled events.
  //
  // token: (output) a transmission token that, after transmission is complete,
  //        contains the return status of transmission.
  //        The UEFI spec states that the completion token's status is updated after the
  //        operation completes, and that the operation is asynchronous.
  //        The token is provided by the caller because the caller is better equipped
  //        to guarantee that the token is not mutated until transmission is complete
  //        and `callback` has been signalled.
  //
  // The transmission data is copied into a buffer before being sent to the
  // simple network protocol, so it is safe to overwrite that as soon
  // as SendV6Frame returns.
  fit::result<efi_status> SendV6Frame(MacAddr dst, cpp20::span<const uint8_t> data,
                                      efi_event callback,
                                      efi_managed_network_sync_completion_token* token);

 private:
  explicit EthernetAgent(EfiProtocolPtr<efi_managed_network_protocol> net_proto,
                         const MacAddr& mac_addr)
      : net_proto_(std::move(net_proto)), mac_addr_(mac_addr) {}

  EfiProtocolPtr<efi_managed_network_protocol> net_proto_;
  MacAddr mac_addr_;
};

}  // namespace gigaboot

#endif  // SRC_FIRMWARE_GIGABOOT_CPP_NETWORK_H_
