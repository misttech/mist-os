// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "rx_queue.h"

#include <lib/async/cpp/task.h>
#include <lib/fdf/cpp/env.h>
#include <zircon/assert.h>

#include "log.h"
#include "session.h"

namespace network::internal {

constexpr char kRxSchedulerRole[] = "fuchsia.devices.network.core.rx";

std::atomic<uint32_t> RxQueue::num_instances_ = 0;

RxQueue::~RxQueue() {
  // running_ is tied to the lifetime of the watch thread, it's cleared in`RxQueue::JoinThread`.
  // This assertion protects us from destruction paths where `RxQueue::JoinThread` is not called.
  ZX_ASSERT_MSG(!running_, "RxQueue destroyed without disposing of port and thread first.");
}

zx::result<std::unique_ptr<RxQueue>> RxQueue::Create(DeviceInterface* parent) {
  fbl::AllocChecker ac;
  std::unique_ptr<RxQueue> queue(new (&ac) RxQueue(parent));
  if (!ac.check()) {
    return zx::error(ZX_ERR_NO_MEMORY);
  }

  fbl::AutoLock lock(&queue->parent_->rx_lock());
  // The RxQueue's capacity is the device's FIFO rx depth as opposed to the hardware's queue depth
  // so we can (possibly) reduce the amount of reads on the rx fifo during rx interrupts.
  auto capacity = parent->rx_fifo_depth();

  zx::result available_queue = RingQueue<uint32_t>::Create(capacity);
  if (available_queue.is_error()) {
    return available_queue.take_error();
  }
  queue->available_queue_ = std::move(available_queue.value());

  zx::result in_flight = IndexedSlab<InFlightBuffer>::Create(capacity);
  if (in_flight.is_error()) {
    return in_flight.take_error();
  }
  queue->in_flight_ = std::move(in_flight.value());

  auto device_depth = parent->info().rx_depth().value_or(0);

  std::unique_ptr<fuchsia_hardware_network_driver::wire::RxSpaceBuffer[]> buffers(
      new (&ac) fuchsia_hardware_network_driver::wire::RxSpaceBuffer[device_depth]);
  if (!ac.check()) {
    return zx::error(ZX_ERR_NO_MEMORY);
  }
  if (zx_status_t status = zx::port::create(0, &queue->rx_watch_port_); status != ZX_OK) {
    LOGF_ERROR("failed to create rx watch port: %s", zx_status_get_string(status));
    return zx::error(status);
  }

  // Make sure the driver framework allows for the creation of the necessary threads. Keep track of
  // the number of RX queue instances globally.
  uint32_t instances = ++num_instances_;
  if (zx_status_t status =
          fdf_env_set_thread_limit(kRxSchedulerRole, strlen(kRxSchedulerRole), instances);
      status != ZX_OK && status != ZX_ERR_OUT_OF_RANGE) {
    // ZX_ERR_OUT_OF_RANGE indicates that the value is less than the current value. This can happen
    // if a number of threads have recently shut down or two RX queues are being created at the same
    // time and the loading of the atomic and setting of the limit are interleaved. It's safe to
    // ignore that in this context, the important part is that there are enough threads.
    LOGF_ERROR("failed to update thread limit: %s", zx_status_get_string(status));
    return zx::error(status);
  }

  // In order to ensure that the async::PostTask below works this has to be a synchronized
  // dispatcher that allows synchronous calls. Any other combination of dispatcher and options could
  // lead to the async::PostTask call being inlined, meaning the task would run on the calling
  // thread, blocking it indefinitely. Use a unique owner to ensure inlining of calls from inside
  // the task. Calls to a dispatcher with the same owner might not be inlined.
  auto dispatcher = fdf_env::DispatcherBuilder::CreateSynchronizedWithOwner(
      queue.get(), fdf::SynchronizedDispatcher::Options::kAllowSyncCalls, "netdevice:rx_watch",
      [queue = queue.get()](fdf_dispatcher_t*) { queue->dispatcher_shutdown_.Signal(); },
      kRxSchedulerRole);
  if (dispatcher.is_error()) {
    LOGF_ERROR("rx queue failed to create dispatcher: %s", dispatcher.status_string());
    return dispatcher.take_error();
  }
  queue->dispatcher_ = std::move(dispatcher.value());

  async::PostTask(queue->dispatcher_.async_dispatcher(),
                  [queue = queue.get(), rx_buffers = std::move(buffers)]() mutable {
                    queue->WatchThread(std::move(rx_buffers));
                  });

  queue->running_ = true;
  return zx::ok(std::move(queue));
}

void RxQueue::TriggerRxWatch() {
  if (!running_) {
    return;
  }

  zx_port_packet_t packet;
  packet.type = ZX_PKT_TYPE_USER;
  packet.key = kTriggerRxKey;
  packet.status = ZX_OK;
  zx_status_t status = rx_watch_port_.queue(&packet);
  if (status != ZX_OK) {
    LOGF_ERROR("TriggerRxWatch failed: %s", zx_status_get_string(status));
  }
}

void RxQueue::TriggerSessionChanged() {
  if (!running_) {
    return;
  }
  zx_port_packet_t packet;
  packet.type = ZX_PKT_TYPE_USER;
  packet.key = kSessionSwitchKey;
  packet.status = ZX_OK;
  zx_status_t status = rx_watch_port_.queue(&packet);
  if (status != ZX_OK) {
    LOGF_ERROR("TriggerSessionChanged failed: %s", zx_status_get_string(status));
  }
}

void RxQueue::JoinThread() {
  if (dispatcher_.get()) {
    zx_port_packet_t packet;
    packet.type = ZX_PKT_TYPE_USER;
    packet.key = kQuitWatchKey;
    zx_status_t status = rx_watch_port_.queue(&packet);
    if (status != ZX_OK) {
      LOGF_ERROR("RxQueue::JoinThread failed to send quit key: %s", zx_status_get_string(status));
    }
    // Mark the queue as not running anymore.
    running_ = false;
    dispatcher_.ShutdownAsync();
    dispatcher_shutdown_.Wait();
    dispatcher_.reset();
    --num_instances_;
  }
}

void RxQueue::PurgeSession(Session& session) {
  fbl::AutoLock lock(&parent_->rx_lock());
  // Get rid of all available buffers that belong to the session and stop its rx path.
  session.AssertParentRxLock(*parent_);
  session.StopRx();
  for (auto nu = available_queue_->count(); nu > 0; nu--) {
    auto b = available_queue_->Pop();
    if (in_flight_->Get(b).session == &session) {
      in_flight_->Free(b);
    } else {
      // Push back to the end of the queue.
      available_queue_->Push(b);
    }
  }
}

std::tuple<RxQueue::InFlightBuffer*, uint32_t> RxQueue::GetBuffer() {
  if (available_queue_->count() != 0) {
    auto idx = available_queue_->Pop();
    return std::make_tuple(&in_flight_->Get(idx), idx);
  }
  // Need to fetch more from the session.
  if (in_flight_->available() == 0) {
    // No more space to keep in flight buffers.
    LOG_ERROR("can't fit more in-flight buffers");
    return std::make_tuple(nullptr, 0);
  }

  RxSessionTransaction transaction(this);
  switch (zx_status_t status = parent_->LoadRxDescriptors(transaction); status) {
    case ZX_OK:
      break;
    default:
      LOGF_ERROR("failed to load rx buffer descriptors: %s", zx_status_get_string(status));
      __FALLTHROUGH;
    case ZX_ERR_PEER_CLOSED:  // Primary FIFO closed.
    case ZX_ERR_SHOULD_WAIT:  // No Rx buffers available in FIFO.
    case ZX_ERR_BAD_STATE:    // Primary session stopped or paused.
      return std::make_tuple(nullptr, 0);
  }
  // LoadRxDescriptors can't return OK if it couldn't load any descriptors.
  auto idx = available_queue_->Pop();
  return std::make_tuple(&in_flight_->Get(idx), idx);
}

zx_status_t RxQueue::PrepareBuff(fuchsia_hardware_network_driver::wire::RxSpaceBuffer* buff) {
  auto [session_buffer, index] = GetBuffer();
  if (session_buffer == nullptr) {
    return ZX_ERR_NO_RESOURCES;
  }

  buff->id = index;
  if (zx_status_t status =
          session_buffer->session->FillRxSpace(session_buffer->descriptor_index, buff);
      status != ZX_OK) {
    // If the session can't fill Rx for any reason, kill it.
    session_buffer->session->Kill();
    // Put the index back at the end of the available queue.
    available_queue_->Push(index);
    return status;
  }

  session_buffer->session->RxTaken();
  device_buffer_count_++;
  return ZX_OK;
}

void RxQueue::CompleteRxList(
    const fidl::VectorView<::fuchsia_hardware_network_driver::wire::RxBuffer>& rx_buffer_list) {
  fbl::AutoLock lock(&parent_->rx_lock());
  SharedAutoLock control_lock(&parent_->control_lock());
  device_buffer_count_ -= rx_buffer_list.count();
  for (const auto& rx_buffer : rx_buffer_list.get()) {
    // Always increment frame index for anything the device sends us. Sessions
    // get their local indices for frames that make their way through.
    rx_completed_frame_index_++;
    ZX_ASSERT_MSG(rx_buffer.data.count() <= MAX_BUFFER_PARTS,
                  "too many buffer parts in rx buffer: %ld", rx_buffer.data.count());
    std::array<SessionRxBuffer, MAX_BUFFER_PARTS> session_parts;
    auto session_parts_iter = session_parts.begin();
    bool drop_frame = false;
    uint32_t total_length = 0;

    Session* primary_session = nullptr;
    cpp20::span rx_parts = rx_buffer.data.get();
    for (const fuchsia_hardware_network_driver::wire::RxBufferPart& rx_part : rx_parts) {
      InFlightBuffer& in_flight_buffer = in_flight_->Get(rx_part.id);

      total_length += rx_part.length;
      *session_parts_iter++ = SessionRxBuffer{
          .descriptor = in_flight_buffer.descriptor_index,
          .offset = rx_part.offset,
          .length = rx_part.length,
      };

      if (primary_session && in_flight_buffer.session != primary_session) {
        // Received buffers from different sessions, meaning the primary session just changed and
        // the device chained things together.
        // If we don't want to drop this frame, we'd need to figure out which one is the new primary
        // session, try and allocate buffers from it and copy things.
        // That's complicated enough and this is unexpected enough that the current decision is to
        // drop the frame on the floor.
        LOGF_WARN(
            "dropping chained frame with %ld buffers spanning different sessions: "
            "%s, %s",
            rx_buffer.data.count(), primary_session->name(), in_flight_buffer.session->name());
        drop_frame = true;
      }
      ZX_DEBUG_ASSERT(in_flight_buffer.session != nullptr);
      primary_session = in_flight_buffer.session;
    }

    if (!primary_session) {
      // Buffer contained no parts.
      LOG_WARN("attempted to return an rx buffer with no parts");
      continue;
    }

    // Drop any frames containing no data or where inconsistencies were found above.
    if (total_length == 0 || drop_frame) {
      for (const fuchsia_hardware_network_driver::wire::RxBufferPart& rx_part : rx_parts) {
        InFlightBuffer& in_flight_buffer = in_flight_->Get(rx_part.id);
        in_flight_buffer.session->AssertParentRxLock(*parent_);
        if (in_flight_buffer.session->CompleteUnfulfilledRx()) {
          // Make buffer available again for reuse if session is still valid.
          available_queue_->Push(rx_part.id);
        } else {
          // Free it otherwise.
          in_flight_->Free(rx_part.id);
        }
      }
      continue;
    }

    primary_session->AssertParentControlLockShared(*parent_);
    parent_->NotifyPortRxFrame(rx_buffer.meta.port, total_length);
    const RxFrameInfo frame_info = {
        .meta = rx_buffer.meta,
        .port_id_salt = parent_->GetPortSalt(rx_buffer.meta.port),
        .buffers = cpp20::span(session_parts.begin(), session_parts_iter),
        .total_length = total_length,
    };
    primary_session->AssertParentRxLock(*parent_);
    if (primary_session->CompleteRx(frame_info)) {
      std::for_each(rx_parts.begin(), rx_parts.end(),
                    [this](const fuchsia_hardware_network_driver::wire::RxBufferPart& rx)
                        __TA_REQUIRES(parent_->rx_lock()) { available_queue_->Push(rx.id); });
    } else {
      std::for_each(rx_parts.begin(), rx_parts.end(),
                    [this](const fuchsia_hardware_network_driver::wire::RxBufferPart& rx)
                        __TA_REQUIRES(parent_->rx_lock()) { in_flight_->Free(rx.id); });
    }
  }
  parent_->CommitAllSessions();
  if (device_buffer_count_ <= parent_->rx_notify_threshold()) {
    TriggerRxWatch();
  }
  parent_->TryDelegateRxLease(rx_completed_frame_index_);
}

int RxQueue::WatchThread(
    std::unique_ptr<fuchsia_hardware_network_driver::wire::RxSpaceBuffer[]> space_buffers) {
  auto loop = [this, space_buffers = std::move(space_buffers)]() -> zx_status_t {
    fbl::RefPtr<RefCountedFifo> observed_fifo(nullptr);
    bool waiting_on_fifo = false;
    for (;;) {
      zx_port_packet_t packet;
      bool fifo_readable = false;
      if (zx_status_t status = rx_watch_port_.wait(zx::time::infinite(), &packet);
          status != ZX_OK) {
        LOGF_ERROR("RxQueue::WatchThread port wait failed %s", zx_status_get_string(status));
        return status;
      }
      parent_->NotifyRxQueuePacket(packet.key);
      switch (packet.key) {
        case kQuitWatchKey:
          LOG_TRACE("RxQueue::WatchThread got quit key");
          return ZX_OK;
        case kSessionSwitchKey:
          if (observed_fifo && waiting_on_fifo) {
            if (zx_status_t status = rx_watch_port_.cancel(observed_fifo->fifo, kFifoWatchKey);
                status != ZX_OK) {
              LOGF_ERROR("RxQueue::WatchThread port cancel failed %s",
                         zx_status_get_string(status));
              return status;
            }
            waiting_on_fifo = false;
          }
          observed_fifo = parent_->primary_rx_fifo();
          LOGF_TRACE("RxQueue primary FIFO changed, valid=%d", static_cast<bool>(observed_fifo));
          break;
        case kFifoWatchKey:
          if ((packet.signal.observed & ZX_FIFO_PEER_CLOSED) || packet.status != ZX_OK) {
            // If observing the FIFO fails, we're assuming that the session is being closed. We're
            // just going to dispose of our reference to the observed FIFO and wait for
            // `DeviceInterface` to signal us that a new primary session is available when that
            // happens.
            observed_fifo.reset();
            LOGF_TRACE("RxQueue fifo closed or bad status %s", zx_status_get_string(packet.status));
          } else {
            fifo_readable = true;
          }
          waiting_on_fifo = false;
          break;
        default:
          ZX_ASSERT_MSG(packet.key == kTriggerRxKey, "Unrecognized packet in rx queue");
          break;
      }

      size_t pushed = 0;
      bool should_wait_on_fifo;

      fbl::AutoLock rx_lock(&parent_->rx_lock());
      SharedAutoLock control_lock(&parent_->control_lock());
      const uint16_t rx_depth = parent_->info().rx_depth().value_or(0);
      size_t push_count = rx_depth - device_buffer_count_;
      if (parent_->IsDataPlaneOpen()) {
        for (; pushed < push_count; pushed++) {
          if (PrepareBuff(&space_buffers[pushed]) != ZX_OK) {
            break;
          }
        }
      }

      if (fifo_readable && push_count == 0 && in_flight_->available()) {
        RxSessionTransaction transaction(this);
        parent_->LoadRxDescriptors(transaction);
      }
      // We only need to wait on the FIFO if we didn't get enough buffers.
      // Otherwise, we'll trigger the loop again once the device calls CompleteRx.
      //
      // Similarly, we should not wait on the FIFO if the device has not started yet.
      should_wait_on_fifo = device_buffer_count_ < rx_depth && parent_->IsDataPlaneOpen();

      // We release the main rx queue and control locks before calling into the parent device so we
      // don't cause a re-entrant deadlock.
      rx_lock.release();
      control_lock.release();

      if (pushed != 0) {
        // Send buffers in batches of at most |MAX_RX_SPACE_BUFFERS| at a time to stay within the
        // FIDL channel maximum.
        netdriver::wire::RxSpaceBuffer* buffers = space_buffers.get();
        while (pushed > 0) {
          const uint32_t batch = std::min(static_cast<uint32_t>(pushed), MAX_RX_SPACE_BUFFERS);
          parent_->QueueRxSpace(cpp20::span(buffers, static_cast<uint32_t>(batch)));
          buffers += batch;
          pushed -= batch;
        }
      }

      // No point waiting in RX fifo if we filled the device buffers, we'll get a signal to wait
      // on the fifo later.
      if (should_wait_on_fifo) {
        if (!observed_fifo) {
          // This can happen if we get triggered to fetch more buffers, but the primary session is
          // already tearing down, it's fine to just proceed.
          LOG_TRACE("RxQueue::WatchThread Should wait but no FIFO is here");
        } else if (!waiting_on_fifo) {
          zx_status_t status = observed_fifo->fifo.wait_async(
              rx_watch_port_, kFifoWatchKey, ZX_FIFO_READABLE | ZX_FIFO_PEER_CLOSED, 0);
          if (status == ZX_OK) {
            waiting_on_fifo = true;
          } else {
            LOGF_ERROR("RxQueue::WatchThread wait_async failed: %s", zx_status_get_string(status));
            return status;
          }
        }
      }
    }
  };

  zx_status_t status = loop();
  if (status != ZX_OK) {
    LOGF_ERROR("RxQueue::WatchThread finished loop with error: %s", zx_status_get_string(status));
  }
  LOG_TRACE("watch thread done");
  return 0;
}

uint32_t RxQueue::SessionTransaction::remaining() __TA_REQUIRES(queue_->parent_->rx_lock()) {
  // NB: __TA_REQUIRES here is just encoding that a SessionTransaction always holds a lock for
  // its parent queue, the protection from misuse comes from the annotations on
  // `SessionTransaction`'s constructor and destructor.
  return queue_->in_flight_->available();
}

void RxQueue::SessionTransaction::Push(Session* session, uint16_t descriptor)
    __TA_REQUIRES(queue_->parent_->rx_lock()) {
  // NB: __TA_REQUIRES here is just encoding that a SessionTransaction always holds a lock for
  // its parent queue, the protection from misuse comes from the annotations on
  // `SessionTransaction`'s constructor and destructor.
  uint32_t idx = queue_->in_flight_->Push(InFlightBuffer(session, descriptor));
  queue_->available_queue_->Push(idx);
}

}  // namespace network::internal
