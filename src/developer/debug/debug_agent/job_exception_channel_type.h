// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVELOPER_DEBUG_DEBUG_AGENT_JOB_EXCEPTION_CHANNEL_TYPE_H_
#define SRC_DEVELOPER_DEBUG_DEBUG_AGENT_JOB_EXCEPTION_CHANNEL_TYPE_H_

namespace debug_agent {

enum class JobExceptionChannelType {
  // Requests the "normal" job exception channel, this will report exceptions that are not handled
  // by any of the job's children to us before they are handled by the system RootJob exception
  // handler. A strong filter with the "job_only" configuration will result in using this channel.
  kException,
  // Requests the "JobDebugger" exception channel, which registers us for notifications for process
  // starting events, but not exceptions. There may be many instances of this type of channel, see
  // https://fuchsia.dev/fuchsia-src/concepts/kernel/exceptions#exception_types. This setting is the
  // result of a filter with both "job_only" and "weak" configured.
  kDebugger,
};

}  // namespace debug_agent

#endif  // SRC_DEVELOPER_DEBUG_DEBUG_AGENT_JOB_EXCEPTION_CHANNEL_TYPE_H_
