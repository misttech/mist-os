// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/sys/fuzzing/common/tests/integration-tests.h"

#include "src/sys/fuzzing/common/artifact.h"
#include "src/sys/fuzzing/common/async-socket.h"
#include "src/sys/fuzzing/common/engine.h"
#include "src/sys/fuzzing/common/testing/artifact.h"
#include "src/sys/fuzzing/common/testing/component-context.h"

namespace fuzzing {

using ::fuchsia::fuzzer::FUZZ_MODE;

void EngineIntegrationTest::SetUp() {
  AsyncTest::SetUp();
  context_ = ComponentContextForTest::Create(executor());
  engine_ = std::make_unique<ChildProcess>(executor());
}

ZxPromise<ControllerPtr> EngineIntegrationTest::Start() {
  registrar_ = std::make_unique<FakeRegistrar>(executor());
  return fpromise::make_promise([this]() -> ZxResult<> {
           fidl::InterfaceHandle<Registrar> registrar = registrar_->NewBinding();
           engine_->Reset();
           if (auto status = engine_->AddArg(program_binary()); status != ZX_OK) {
             return fpromise::error(status);
           }
           if (auto status = engine_->AddArg(component_url()); status != ZX_OK) {
             return fpromise::error(status);
           }
           for (const auto& arg : extra_args()) {
             if (auto status = engine_->AddArg(arg); status != ZX_OK) {
               return fpromise::error(status);
             }
           }
           if (auto status = engine_->AddArg(FUZZ_MODE); status != ZX_OK) {
             return fpromise::error(status);
           }
           if (auto status = engine_->AddChannel(ComponentContextForTest::kRegistrarId,
                                                 registrar.TakeChannel());
               status != ZX_OK) {
             return fpromise::error(status);
           }
           if (auto status =
                   engine_->AddChannel(ComponentContextForTest::kCoverageId, fuzz_coverage());
               status != ZX_OK) {
             return fpromise::error(status);
           }
           return fpromise::ok();
         })
      .and_then([this]() { return AsZxResult(engine_->Spawn()); })
      .and_then(registrar_->TakeProvider())
      .and_then([this, controller = ControllerPtr(), fut = ZxFuture<>()](
                    Context& context,
                    ControllerProviderHandle& handle) mutable -> ZxResult<ControllerPtr> {
        if (!fut) {
          auto request = controller.NewRequest(executor()->dispatcher());
          provider_ = handle.Bind();
          Bridge<> bridge;
          provider_->Connect(std::move(request), bridge.completer.bind());
          fut = ConsumeBridge(bridge);
        }
        if (!fut(context)) {
          return fpromise::pending();
        }
        if (fut.is_error()) {
          return fpromise::error(fut.error());
        }
        return fpromise::ok(std::move(controller));
      })
      .wrap_with(scope_);
}

void EngineIntegrationTest::TearDown() {
  Schedule(engine_->Kill());
  RunUntilIdle();
  AsyncTest::TearDown();
}

void EngineIntegrationTest::Crash() {
  ControllerPtr controller;
  FUZZING_EXPECT_OK(Start(), &controller);
  RunUntilIdle();

  Artifact actual;
  FUZZING_EXPECT_OK(WatchArtifact(executor(), controller), &actual);
  RunUntilIdle();
  EXPECT_TRUE(actual.is_empty());

  FUZZING_EXPECT_OK(WatchArtifact(executor(), controller), &actual);
  Input input("FUZZ");
  ZxBridge<> bridge1;
  controller->TryOne(AsyncSocketWrite(executor(), input), ZxBind<>(std::move(bridge1.completer)));
  FUZZING_EXPECT_OK(ConsumeBridge(bridge1));
  RunUntilIdle();
  ASSERT_FALSE(actual.is_empty());
  EXPECT_EQ(actual.fuzz_result(), FuzzResult::CRASH);

  Bridge<Status> bridge2;
  controller->GetStatus(bridge2.completer.bind());
  Status status;
  FUZZING_EXPECT_OK(ConsumeBridge(bridge2), &status);
  RunUntilIdle();
  EXPECT_TRUE(status.has_elapsed());
}

}  // namespace fuzzing
