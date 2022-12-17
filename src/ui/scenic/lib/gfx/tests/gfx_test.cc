// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/scenic/lib/gfx/tests/gfx_test.h"

#include "src/ui/scenic/lib/scheduling/default_frame_scheduler.h"
#include "src/ui/scenic/lib/scheduling/windowed_frame_predictor.h"

namespace scenic_impl::gfx::test {

void GfxSystemTest::TearDown() {
  ScenicTest::TearDown();
  engine_.reset();
  FX_DCHECK(gfx_system_.expired());
}

void GfxSystemTest::InitializeScenic(std::shared_ptr<Scenic> scenic) {
  engine_ = std::make_shared<Engine>(escher::EscherWeakPtr());
  auto image_pipe_updater = std::make_shared<ImagePipeUpdater>(*frame_scheduler_);
  gfx_system_ =
      scenic->RegisterSystem<GfxSystem>(engine_.get(),
                                        /* sysmem */ nullptr,
                                        /* display_manager */ nullptr, image_pipe_updater);
  frame_scheduler_->Initialize(
      std::make_shared<scheduling::VsyncTiming>(),
      /*update_sessions*/
      [this, scenic, image_pipe_updater](auto& sessions_to_update, auto trace_id,
                                         auto fences_from_previous_presents) {
        auto results = image_pipe_updater->UpdateSessions(sessions_to_update, trace_id);
        results.merge(scenic->UpdateSessions(sessions_to_update, trace_id));

        engine_->SignalFencesWhenPreviousRendersAreDone(std::move(fences_from_previous_presents));

        return results;
      },
      /*on_cpu_work_done*/
      [scenic, image_pipe_updater] {
        image_pipe_updater->OnCpuWorkDone();
        scenic->OnCpuWorkDone();
      },
      /*on_frame_presented*/
      [scenic, image_pipe_updater](auto latched_times, auto present_times) {
        image_pipe_updater->OnFramePresented(latched_times, present_times);
        scenic->OnFramePresented(latched_times, present_times);
      },
      /*render_scheduled_frame*/
      [this](auto frame_number, auto presentation_time, auto callback) {
        engine_->RenderScheduledFrame(frame_number, presentation_time, std::move(callback));
      });
}

}  // namespace scenic_impl::gfx::test
