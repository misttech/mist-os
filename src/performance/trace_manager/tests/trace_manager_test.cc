// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/performance/trace_manager/tests/trace_manager_test.h"

#include <lib/syslog/cpp/macros.h>

#include <memory>
#include <utility>

namespace tracing {
namespace test {

TraceManagerTest::TraceManagerTest() : executor_(dispatcher()) {
  controller_.events().OnSessionStateChange = [this](controller::SessionState state) {
    FidlOnSessionStateChange(state);
  };
}

void TraceManagerTest::SetUp() {
  TestLoopFixture::SetUp();

  Config config;
  ASSERT_TRUE(config.ReadFrom(kConfigFile));

  app_ = std::make_unique<TraceManagerApp>(context_provider_.TakeContext(), std::move(config),
                                           executor_);
}

void TraceManagerTest::TearDown() {
  fake_provider_bindings_.clear();
  app_.reset();
  TestLoopFixture::TearDown();
}

void TraceManagerTest::ConnectToProvisionerService() {
  FX_LOGS(DEBUG) << "ConnectToProvisionerService";
  context_provider().ConnectToPublicService(provisioner_.NewRequest());
}

void TraceManagerTest::DisconnectFromControllerService() {
  FX_LOGS(DEBUG) << "DisconnectFromControllerService";
  controller_.Unbind();
}

bool TraceManagerTest::AddFakeProvider(zx_koid_t pid, const std::string& name,
                                       FakeProvider** out_provider) {
  provider::RegistryPtr registry;
  context_provider().ConnectToPublicService(registry.NewRequest());

  auto provider_impl = std::make_unique<FakeProvider>(pid, name);
  auto provider = std::make_unique<FakeProviderBinding>(std::move(provider_impl));

  fidl::InterfaceHandle<provider::Provider> provider_client{provider->NewBinding()};
  if (!provider_client.is_valid()) {
    return false;
  }

  registry->RegisterProvider(std::move(provider_client), provider->impl()->pid(),
                             provider->impl()->name());
  if (out_provider) {
    *out_provider = provider->impl().get();
  }
  fake_provider_bindings_.push_back(std::move(provider));
  return true;
}

void TraceManagerTest::OnSessionStateChange() {
  FX_LOGS(DEBUG) << "Session state change, QuitLoop";
  QuitLoop();
}

TraceManagerTest::SessionState TraceManagerTest::GetSessionState() const {
  if (trace_manager()->session()) {
    switch (trace_manager()->session()->state()) {
#define TRANSLATE_STATE(state)     \
  case TraceSession::State::state: \
    return SessionState::state;
      TRANSLATE_STATE(kReady);
      TRANSLATE_STATE(kInitialized);
      TRANSLATE_STATE(kStarting);
      TRANSLATE_STATE(kStarted);
      TRANSLATE_STATE(kStopping);
      TRANSLATE_STATE(kStopped);
      TRANSLATE_STATE(kTerminating);
#undef TRANSLATE_STATE
    }
  }
  return SessionState::kNonexistent;
}

// static
controller::TraceConfig TraceManagerTest::GetDefaultTraceConfig() {
  std::vector<std::string> categories{kTestUmbrellaCategory};
  controller::TraceConfig config;
  config.set_categories(std::move(categories));
  config.set_buffer_size_megabytes_hint(kDefaultBufferSizeMegabytes);
  config.set_start_timeout_milliseconds(kDefaultStartTimeoutMilliseconds);
  config.set_buffering_mode(fuchsia::tracing::BufferingMode::ONESHOT);
  return config;
}

void TraceManagerTest::InitializeSessionWorker(controller::TraceConfig config, bool* success) {
  // Require a mode to be set, no default here.
  FX_CHECK(config.has_buffering_mode());

  zx::socket our_socket, their_socket;
  zx_status_t status = zx::socket::create(0u, &our_socket, &their_socket);
  ASSERT_EQ(status, ZX_OK);

  provisioner_->InitializeTracing(controller_.NewRequest(), std::move(config),
                                  std::move(their_socket));
  RunLoopUntilIdle();
  FX_LOGS(DEBUG) << "Loop done, expecting session initialized";
  ASSERT_EQ(GetSessionState(), SessionState::kInitialized);

  // Run one more time to finish up provider initialization. This happens
  // after the session transitions to the initialized state, but after all
  // providers have been told to initialize. Since everything is happening
  // on one thread, we can assume that when the loop is idle all registered
  // providers have initialized.
  // This doesn't run forever as there's no session state change involved.
  RunLoopUntilIdle();

  // The counts always have a fixed value here.
  VerifyCounts(0, 0);

  destination_ = std::move(our_socket);

  *success = true;
}

bool TraceManagerTest::InitializeSession(controller::TraceConfig config) {
  bool success{};
  FX_LOGS(DEBUG) << "Initializing session";
  InitializeSessionWorker(std::move(config), &success);
  if (success) {
    FX_LOGS(DEBUG) << "Session initialized";
  }
  return success;
}

// static
controller::StartOptions TraceManagerTest::GetDefaultStartOptions() {
  std::vector<std::string> additional_categories{};
  controller::StartOptions options;
  options.set_buffer_disposition(fuchsia::tracing::BufferDisposition::RETAIN);
  options.set_additional_categories(std::move(additional_categories));
  return options;
}

void TraceManagerTest::BeginStartSession(controller::StartOptions options) {
  FX_LOGS(DEBUG) << "Starting session";

  MarkBeginOperation();

  start_state_.start_completed = false;
  auto callback = [this](controller::Session_StartTracing_Result result) {
    start_state_.start_completed = true;
    start_state_.start_result = std::move(result);
  };
  controller_->StartTracing(std::move(options), callback);

  RunLoopUntilIdle();
  // The loop will exit for the transition to kStarting.
}

bool TraceManagerTest::FinishStartSession() {
  // If there are no tracees then it will also subsequently transition to
  // kStarted before the loop exits. If there are tracees then we need to
  // wait for them to start.
  if (fake_provider_bindings().size() > 0) {
    FX_LOGS(DEBUG) << "Loop done, expecting session starting";
    SessionState state = GetSessionState();
    EXPECT_EQ(state, SessionState::kStarting);
    if (state != SessionState::kStarting) {
      return false;
    }

    // Make sure all providers are marked kStarting.
    // The loop exits when we transition to kStarting, but providers won't have
    // processed their Start requests yet.
    RunLoopUntilIdle();

    MarkAllProvidersStarted();
    // Wait until all providers are started.
    RunLoopUntilIdle();
  }

  // The loop will exit for the transition to kStarted.
  FX_LOGS(DEBUG) << "Loop done, expecting all providers started";
  SessionState state = GetSessionState();
  EXPECT_EQ(state, SessionState::kStarted);
  if (state != SessionState::kStarted) {
    return false;
  }

  // Run the loop one more time to ensure we pick up the result.
  // Remember the loop prematurely exits on session state changes.
  RunLoopUntilIdle();
  EXPECT_TRUE(start_state_.start_completed);
  if (!start_state_.start_completed) {
    return false;
  }
  EXPECT_FALSE(start_state_.start_result.is_err());

  FX_LOGS(DEBUG) << "Session started";
  return true;
}

bool TraceManagerTest::StartSession(controller::StartOptions options) {
  BeginStartSession(std::move(options));
  return FinishStartSession();
}

// static
controller::StopOptions TraceManagerTest::GetDefaultStopOptions() {
  controller::StopOptions options;
  options.set_write_results(true);
  return options;
}

void TraceManagerTest::BeginStopSession(controller::StopOptions options) {
  FX_LOGS(DEBUG) << "Stopping session";

  MarkBeginOperation();

  stop_state_.stop_completed = false;
  auto callback = [this](controller::Session_StopTracing_Result result) {
    ASSERT_TRUE(result.is_response());
    stop_state_.stop_completed = true;
    stop_state_.stop_result = std::move(result.response().result);
    // We need to run the loop one last time to pick up the result.
    // Be sure to exit it now that we have the result.
    QuitLoop();
  };
  controller_->StopTracing(std::move(options), callback);

  RunLoopUntilIdle();
  // The loop will exit for the transition to kStopping.
}

bool TraceManagerTest::FinishStopSession() {
  // If there are no tracees then it will also subsequently transition to
  // kStopped before the loop exits. If there are tracees then we need to
  // wait for them to stop.
  if (fake_provider_bindings().size() > 0) {
    FX_LOGS(DEBUG) << "Loop done, expecting session stopping";
    SessionState state = GetSessionState();
    EXPECT_EQ(state, SessionState::kStopping);
    if (state != SessionState::kStopping) {
      return false;
    }

    // Make sure all providers are marked kStopping.
    // The loop exits when we transition to kStopping, but providers won't have
    // processed their Stop requests yet.
    RunLoopUntilIdle();

    MarkAllProvidersStopped();
    // Wait until all providers are stopped.
    RunLoopUntilIdle();
  }

  // The loop will exit for the transition to kStopped.
  FX_LOGS(DEBUG) << "Loop done, expecting session stopped";
  SessionState state = GetSessionState();
  EXPECT_EQ(state, SessionState::kStopped);
  if (state != SessionState::kStopped) {
    return false;
  }

  // Run one more time to ensure we pick up the stop result.
  RunLoopUntilIdle();
  EXPECT_TRUE(stop_state_.stop_completed);
  if (!stop_state_.stop_completed) {
    return false;
  }

  FX_LOGS(DEBUG) << "Session stopped";
  return true;
}

bool TraceManagerTest::StopSession(controller::StopOptions options) {
  BeginStopSession(std::move(options));
  return FinishStopSession();
}

void TraceManagerTest::BeginTerminateSession() {
  FX_LOGS(DEBUG) << "Terminating session";

  MarkBeginOperation();

  // Disconnecting from the controller will terminate the session
  DisconnectFromControllerService();

  RunLoopUntilIdle();
  // The loop will exit for the transition to kTerminating.
  // Note: If there are no providers then the state will transition again
  // to kNonexistent(== "terminated") before the loop exits.
}

bool TraceManagerTest::FinishTerminateSession() {
  // If there are no tracees then it will also subsequently transition to
  // kTerminated before the loop exits. If there are tracees then we need to
  // wait for them to terminate.
  if (fake_provider_bindings().size() > 0) {
    FX_LOGS(DEBUG) << "Loop done, expecting session terminating";
    SessionState state = GetSessionState();
    state = GetSessionState();
    EXPECT_EQ(state, SessionState::kTerminating);
    if (state != SessionState::kTerminating) {
      return false;
    }

    // Make sure all providers are marked kTerminating.
    RunLoopUntilIdle();

    MarkAllProvidersTerminated();
    RunLoopUntilIdle();
  }

  FX_LOGS(DEBUG) << "Loop done, expecting session terminated";
  EXPECT_EQ(GetSessionState(), SessionState::kNonexistent);

  return true;
}

bool TraceManagerTest::TerminateSession() {
  BeginTerminateSession();
  return FinishTerminateSession();
}

void TraceManagerTest::MarkAllProvidersStarted() {
  FX_LOGS(DEBUG) << "Marking all providers started";
  for (const auto& p : fake_provider_bindings()) {
    p->impl()->MarkStarted();
  }
}

void TraceManagerTest::MarkAllProvidersStopped() {
  FX_LOGS(DEBUG) << "Marking all providers stopped";
  for (const auto& p : fake_provider_bindings()) {
    p->impl()->MarkStopped();
  }
}

void TraceManagerTest::MarkAllProvidersTerminated() {
  FX_LOGS(DEBUG) << "Marking all providers terminated";
  for (const auto& p : fake_provider_bindings()) {
    p->impl()->MarkTerminated();
  }
}

void TraceManagerTest::VerifyCounts(int expected_start_count, int expected_stop_count) {
  SessionState state{GetSessionState()};
  for (const auto& p : fake_provider_bindings()) {
    const std::string& name = p->impl()->name();
    if (state != SessionState::kReady) {
      EXPECT_EQ(p->impl()->initialize_count(), 1) << name;
    } else {
      EXPECT_EQ(p->impl()->initialize_count(), 0) << name;
    }
    EXPECT_EQ(p->impl()->start_count(), expected_start_count) << name;
    EXPECT_EQ(p->impl()->stop_count(), expected_stop_count) << name;
    if (state != SessionState::kNonexistent) {
      EXPECT_EQ(p->impl()->terminate_count(), 0) << name;
    } else {
      EXPECT_EQ(p->impl()->terminate_count(), 1) << name;
    }
  }
}

// fidl event
void TraceManagerTest::FidlOnSessionStateChange(controller::SessionState state) {
  FX_LOGS(DEBUG) << "FidlOnSessionStateChange " << state;
  ++on_session_state_change_event_count_;
  last_session_state_event_ = state;
}

}  // namespace test
}  // namespace tracing
