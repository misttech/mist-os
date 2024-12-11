// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "magma_system_context.h"

#include <lib/magma/platform/platform_trace.h>
#include <lib/magma/util/macros.h>

#include <memory>
#include <unordered_set>
#include <vector>

#include "lib/magma/magma_common_defs.h"
#include "magma_system_connection.h"
#include "magma_system_device.h"

namespace msd {

magma::Status MagmaSystemContext::ExecuteCommandBuffers(
    std::vector<magma_exec_command_buffer>& command_buffers,
    std::vector<magma_exec_resource>& resources, std::vector<uint64_t>& wait_semaphore_ids,
    std::vector<uint64_t>& signal_semaphore_ids, uint64_t flags) {
  std::vector<std::shared_ptr<MagmaSystemBuffer>> system_resources;
  system_resources.reserve(resources.size());

  std::vector<msd::Buffer*> msd_buffers;
  msd_buffers.reserve(resources.size());

  // validate resources
  for (auto& resource : resources) {
    uint64_t id = resource.buffer_id;

    auto buffer = owner_->LookupBufferForContext(id);
    if (!buffer) {
      return MAGMA_DRET_MSG(MAGMA_STATUS_INVALID_ARGS,
                            "ExecuteCommandBuffer: exec resource has invalid buffer handle");
    }

    system_resources.push_back(buffer);
    msd_buffers.push_back(buffer->msd_buf());
  }

  // validate command buffer
  for (auto& command_buffer : command_buffers) {
    if (command_buffer.resource_index >= resources.size()) {
      return MAGMA_DRET_MSG(MAGMA_STATUS_INVALID_ARGS,
                            "ExecuteCommandBuffer: invalid command buffer resource index");
    }

    if (command_buffer.start_offset >= system_resources[command_buffer.resource_index]->size()) {
      return MAGMA_DRET_MSG(MAGMA_STATUS_INVALID_ARGS, "invalid batch start offset 0x%lx",
                            command_buffer.start_offset);
    }
  }

  std::vector<msd::Semaphore*> msd_wait_semaphores;
  msd_wait_semaphores.reserve(wait_semaphore_ids.size());

  std::vector<msd::Semaphore*> msd_signal_semaphores;
  msd_signal_semaphores.reserve(signal_semaphore_ids.size());

  // validate semaphores
  for (auto& semaphore_id : wait_semaphore_ids) {
    auto semaphore = owner_->LookupSemaphoreForContext(semaphore_id);
    if (!semaphore) {
      return MAGMA_DRET_MSG(MAGMA_STATUS_INVALID_ARGS, "wait semaphore id not found 0x%" PRIx64,
                            semaphore_id);
    }
    msd_wait_semaphores.push_back(semaphore->msd_semaphore());
  }
  for (auto& semaphore_id : signal_semaphore_ids) {
    auto semaphore = owner_->LookupSemaphoreForContext(semaphore_id);
    if (!semaphore) {
      return MAGMA_DRET_MSG(MAGMA_STATUS_INVALID_ARGS, "signal semaphore id not found 0x%" PRIx64,
                            semaphore_id);
    }
    msd_signal_semaphores.push_back(semaphore->msd_semaphore());
  }

  magma_status_t status = msd_ctx()->ExecuteCommandBuffers(
      command_buffers, resources, msd_buffers, msd_wait_semaphores, msd_signal_semaphores);

  if (status == MAGMA_STATUS_UNIMPLEMENTED) {
    if (command_buffers.size() > 1) {
      return MAGMA_DRET_MSG(MAGMA_STATUS_UNIMPLEMENTED,
                            "can't fallback to ExecuteCommandBufferWithResources");
    }
    const bool has_command_buffers = command_buffers.size() > 0;

    struct magma_command_buffer magma_command_buffer = {
        .resource_count = static_cast<uint32_t>(resources.size()),
        .batch_buffer_resource_index = has_command_buffers ? command_buffers[0].resource_index : 0,
        .batch_start_offset = has_command_buffers ? command_buffers[0].start_offset : 0,
        .wait_semaphore_count = static_cast<uint32_t>(msd_wait_semaphores.size()),
        .signal_semaphore_count = static_cast<uint32_t>(msd_signal_semaphores.size()),
        .flags = flags,
    };
    return msd_ctx()->ExecuteCommandBufferWithResources(
        &magma_command_buffer, resources.data(), msd_buffers.data(), msd_wait_semaphores.data(),
        msd_signal_semaphores.data());
  }

  return MAGMA_DRET_MSG(status, "ExecuteCommandBuffers failed: %d", status);
}

magma::Status MagmaSystemContext::ExecuteImmediateCommands(uint64_t commands_size, void* commands,
                                                           uint64_t semaphore_count,
                                                           uint64_t* semaphore_ids) {
  TRACE_DURATION("magma", "MagmaSystemContext::ExecuteImmediateCommands");
  std::vector<msd::Semaphore*> msd_semaphores(semaphore_count);
  for (uint32_t i = 0; i < semaphore_count; i++) {
    auto semaphore = owner_->LookupSemaphoreForContext(semaphore_ids[i]);
    if (!semaphore)
      return MAGMA_DRET_MSG(MAGMA_STATUS_INVALID_ARGS, "semaphore id not found 0x%" PRIx64,
                            semaphore_ids[i]);
    msd_semaphores[i] = semaphore->msd_semaphore();
    // This is used to connect with command submission in
    // src/ui/scenic/lib/flatland/renderer/vk_renderer.cc, so it uses the koid.
    TRACE_FLOW_END("gfx", "semaphore", semaphore->global_id());
  }
  magma_status_t result = msd_ctx()->ExecuteImmediateCommands(
      cpp20::span(reinterpret_cast<uint8_t*>(commands), commands_size), msd_semaphores);

  return MAGMA_DRET_MSG(
      result, "ExecuteImmediateCommands: msd_context_execute_immediate_commands failed: %d",
      result);
}

magma::Status MagmaSystemContext::ExecuteInlineCommands(
    std::vector<magma_inline_command_buffer> commands) {
  TRACE_DURATION("magma", "MagmaSystemContext::ExecuteInlineCommands");

  for (auto& command : commands) {
    std::vector<msd::Semaphore*> msd_semaphores(command.semaphore_count);

    for (uint32_t i = 0; i < command.semaphore_count; i++) {
      auto semaphore = owner_->LookupSemaphoreForContext(command.semaphore_ids[i]);
      if (!semaphore)
        return MAGMA_DRET_MSG(MAGMA_STATUS_INVALID_ARGS, "semaphore id not found 0x%" PRIx64,
                              command.semaphore_ids[i]);
      msd_semaphores[i] = semaphore->msd_semaphore();
      // This is used to connect with command submission in
      // src/ui/scenic/lib/gfx/engine/engine.cc and
      // src/ui/scenic/lib/flatland/renderer/vk_renderer.cc, so it uses the koid.
      TRACE_FLOW_END("gfx", "semaphore", semaphore->global_id());
    }

    magma_status_t result = msd_ctx()->ExecuteInlineCommand(&command, msd_semaphores.data());
    if (result != MAGMA_STATUS_OK) {
      return MAGMA_DRET_MSG(result, "ExecuteInlineCommand failed: %d", result);
    }
  }

  return MAGMA_STATUS_OK;
}

}  // namespace msd
