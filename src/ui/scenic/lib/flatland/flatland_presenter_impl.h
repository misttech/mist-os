// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_UI_SCENIC_LIB_FLATLAND_FLATLAND_PRESENTER_IMPL_H_
#define SRC_UI_SCENIC_LIB_FLATLAND_FLATLAND_PRESENTER_IMPL_H_

#include <fuchsia/ui/composition/cpp/fidl.h>
#include <lib/async/dispatcher.h>

#include <memory>

#include "src/ui/scenic/lib/flatland/flatland_presenter.h"
#include "src/ui/scenic/lib/scheduling/frame_scheduler.h"
#include "src/ui/scenic/lib/scheduling/id.h"

namespace flatland {

class FlatlandPresenterImpl final : public FlatlandPresenter,
                                    public std::enable_shared_from_this<FlatlandPresenterImpl> {
 public:
  // The |main_dispatcher| must be the dispatcher that GFX sessions run and update on. That thread
  // is typically refered to as the "main thread" or "render thread".
  // FrameScheduler is what FlatlandPresenterImpl will use for frame scheduling calls.
  FlatlandPresenterImpl(async_dispatcher_t* main_dispatcher,
                        scheduling::FrameScheduler& frame_scheduler);

  // Return all release fences that were accumulated during calls to UpdateSessions().  The caller
  // takes responsibility for signaling these fences when it is safe for clients to reuse the
  // associated resources.
  std::vector<zx::event> TakeReleaseFences();

  // |FlatlandPresenter|
  void ScheduleUpdateForSession(zx::time requested_presentation_time,
                                scheduling::SchedulingIdPair id_pair, bool unsquashable,
                                std::vector<zx::event> release_fences) override;

  // |FlatlandPresenter|.
  std::vector<scheduling::FuturePresentationInfo> GetFuturePresentationInfos() override;

  // |FlatlandPresenter|
  void RemoveSession(scheduling::SessionId session_id,
                     std::optional<zx::event> release_fence) override;

  // Called at FrameScheduler's UpdateSessions() time.
  // Takes the release fences up to the corresponding PresentId for each SessionId in
  // |sessions_to_update| and moves them to the set of fences to be returned by the next call to
  // TakeReleaseFences().
  // This way the caller can get all fences accumulated since the last call to
  // TakeReleaseFences(), i.e. the complete set of fences for one frame (allowing for multiple
  // UpdateSessions() calls per frame).
  void AccumulateReleaseFences(
      const std::unordered_map<scheduling::SessionId, scheduling::PresentId>& sessions_to_update);

 private:
  async_dispatcher_t* const main_dispatcher_;
  scheduling::FrameScheduler& frame_scheduler_;
  std::map<scheduling::SchedulingIdPair, std::vector<zx::event>> release_fences_;
  std::vector<zx::event> accumulated_release_fences_;

  // Ask for 8 frames of information for GetFuturePresentationInfos().
  const int64_t kDefaultPredictionInfos = 8;

  // The default frame interval assumes a 60Hz display.
  const zx::duration kDefaultFrameInterval = zx::usec(16'667);

  const zx::duration kDefaultPredictionSpan = kDefaultFrameInterval * kDefaultPredictionInfos;
};

}  // namespace flatland

#endif  // SRC_UI_SCENIC_LIB_FLATLAND_FLATLAND_PRESENTER_IMPL_H_
