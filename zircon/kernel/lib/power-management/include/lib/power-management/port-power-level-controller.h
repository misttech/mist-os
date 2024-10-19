// Copyright 2024 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_LIB_POWER_MANAGEMENT_INCLUDE_LIB_POWER_MANAGEMENT_PORT_POWER_LEVEL_CONTROLLER_H_
#define ZIRCON_KERNEL_LIB_POWER_MANAGEMENT_INCLUDE_LIB_POWER_MANAGEMENT_PORT_POWER_LEVEL_CONTROLLER_H_

#include <lib/zircon-internal/thread_annotations.h>
#include <lib/zx/result.h>
#include <zircon/assert.h>

#include <fbl/ref_ptr.h>
#include <kernel/spinlock.h>
#include <ktl/array.h>
#include <object/port_dispatcher.h>

#include "power-level-controller.h"

namespace power_management {

// Interface representing an entity in charge of update requests that are not handled by the kernel.
//
// In essence there will be only one type of transition handler, but we introduce the interface to
// decouple most of the code from the kernel environment.
class PortPowerLevelController final : public PowerLevelController {
 public:
  explicit PortPowerLevelController(fbl::RefPtr<PortDispatcher> dispatcher)
      : PowerLevelController(), port_(std::move(dispatcher)), packet_queue_(port_.get()) {}
  ~PortPowerLevelController() final = default;

  // Process a pending request, which is a pending transition which could not be performed in the
  // context it originated. This method provide no guarantees on what exactly is performed. It may
  // provide defer with another entity
  zx::result<> Post(const PowerLevelUpdateRequest& pending) final;

  // Unique id of the `ControlInterface` handler.
  uint64_t id() const final { return port_->get_koid(); }

 private:
  // Stashes the latest update on the available (unqueued) packet. By becoming the packet allocator,
  // the `Free` hook will tell us when the unavailable packet becomes available. At this point, we
  // can check if there are any pending updates. In that case, we queue the next packet.
  //
  // Also this construct immediately bounds the amount of possible queued packets to one at any
  // given time and the memory used per power domain to two packets.
  class PacketQueue final : public PortAllocator {
   public:
    explicit PacketQueue(PortDispatcher* port) : port_(port) {}
    ~PacketQueue() final;

    PortPacket* Alloc() final { return nullptr; }
    void Free(PortPacket* packet) final;

    void Queue(const zx_port_packet_t& packet);

   private:
    DECLARE_SPINLOCK(PacketQueue) packet_lock_;
    // Current packet stashing changes.
    TA_GUARDED(&packet_lock_) size_t current_ = 0;
    TA_GUARDED(&packet_lock_) bool packet_pending_ = false;
    TA_GUARDED(&packet_lock_) bool packet_queued_ = false;
    TA_GUARDED(&packet_lock_)
    ktl::array<PortPacket, 2> packets_ = {
        PortPacket{nullptr, this},
        PortPacket{nullptr, this},
    };

    // This is safe, the `port` reference is kept by the controller, and due to member destruction
    // ordering we will never be destructed after the reference is released.
    PortDispatcher* const port_ = nullptr;
  };

  // Required to protect access to the preallocated packets, when one is freed.
  fbl::RefPtr<PortDispatcher> port_ = nullptr;
  PacketQueue packet_queue_;
};

}  // namespace power_management

#endif  // ZIRCON_KERNEL_LIB_POWER_MANAGEMENT_INCLUDE_LIB_POWER_MANAGEMENT_PORT_POWER_LEVEL_CONTROLLER_H_
