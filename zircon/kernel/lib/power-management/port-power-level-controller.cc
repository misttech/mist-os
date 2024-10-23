// Copyright 2024 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include "lib/power-management/port-power-level-controller.h"

#include <lib/power-management/energy-model.h>
#include <lib/power-management/power-state.h>
#include <zircon/assert.h>
#include <zircon/errors.h>
#include <zircon/syscalls-next.h>
#include <zircon/syscalls/port.h>
#include <zircon/types.h>

#include <object/port_dispatcher.h>

#include "kernel/spinlock.h"

namespace power_management {

PortPowerLevelController::PacketQueue::~PacketQueue() {
  // No need to hold any locks here, the last reference to the controller went away, there can't
  // be any legal access.
  for (auto& packet : packets_) {
    port_->CancelQueued(&packet);
    ZX_ASSERT(!packet.InContainer());
  }
}

void PortPowerLevelController::PacketQueue::Queue(const zx_port_packet_t& packet) {
  PortPacket* curr = nullptr;
  {
    Guard<SpinLock, IrqSave> guard(&packet_lock_);
    curr = &packets_[current_ % 2];
    curr->packet = packet;
    if (packet_queued_) {
      packet_pending_ = true;
      return;
    }

    current_++;
    packet_queued_ = true;
    packet_pending_ = false;
  }
  // It is ok to release the lock before we queue here. Given that there is a `queued` packet
  // if its not yet in the port, updates will be stashed in the other entry.
  // It is not possible to hit `ZX_ERR_SHOULD_WAIT` since none of these packets is allocated with
  // the port's default allocator.
  ZX_ASSERT(port_->Queue(curr, ZX_SIGNAL_NONE) != ZX_ERR_SHOULD_WAIT);
}

void PortPowerLevelController::PacketQueue::Free(PortPacket* packet) {
  PortPacket* curr = nullptr;
  {
    Guard<SpinLock, IrqSave> guard(&packet_lock_);
    ZX_DEBUG_ASSERT(packet_queued_);
    ZX_DEBUG_ASSERT(packet == &packets_[(current_ + 1) % 2]);
    if (!packet_pending_) {
      packet_queued_ = false;
      return;
    }
    packet_pending_ = false;
    curr = &packets_[current_ % 2];
    current_++;
  }
  // At this point, any updates are going to the just released `packet`, and we can freely queue
  // `curr` without holding the lock.
  // It is not possible to hit `ZX_ERR_SHOULD_WAIT` since none of these packets is allocated with
  // the port's default allocator.
  ZX_ASSERT(port_->Queue(curr, ZX_SIGNAL_NONE) != ZX_ERR_SHOULD_WAIT);
}

zx::result<> PortPowerLevelController::Post(const PowerLevelUpdateRequest& pending) {
  if (pending.control != ControlInterface::kCpuDriver) {
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }

  if (port_->current_handle_count() == 0) {
    // There shouldn't be more attempts to queue.
    serving_.store(false, std::memory_order_relaxed);
    return zx::error(ZX_ERR_BAD_STATE);
  }

  zx_port_packet_t packet{
      // 'domain_id` used to register the port with a power domain.
      .key = pending.domain_id,
      .type = ZX_PKT_TYPE_PROCESSOR_POWER_LEVEL_TRANSITION_REQUEST,
      .status = ZX_OK,
      .processor_power_level_transition =
          {
              // `domain_id` in this context is subject to interpretation of `options`.
              .domain_id = pending.target_id,
              .options = pending.options,
              .control_interface = static_cast<uint64_t>(pending.control),
              .control_argument = pending.control_argument,
          },
  };

  packet_queue_.Queue(packet);

  return zx::ok();
}

}  // namespace power_management
