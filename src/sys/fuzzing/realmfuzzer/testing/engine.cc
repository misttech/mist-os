// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <stddef.h>

#include <memory>

#include <gtest/gtest.h>

#include "src/sys/fuzzing/common/input.h"
#include "src/sys/fuzzing/common/testing/async-test.h"
#include "src/sys/fuzzing/common/testing/component-context.h"
#include "src/sys/fuzzing/realmfuzzer/engine/adapter-client.h"
#include "src/sys/fuzzing/realmfuzzer/engine/corpus.h"
#include "src/sys/fuzzing/realmfuzzer/engine/runner.h"

// These tests replaces the engine when building a fuzzer test instead of a fuzzer.

namespace fuzzing {

class FuzzerTest : public AsyncTest {
 protected:
  void SetUp() override {
    AsyncTest::SetUp();
    context_ = ComponentContextForTest::Create(executor());
    runner_ = RealmFuzzerRunner::MakePtr(context_->executor());
    auto runner_impl = std::static_pointer_cast<RealmFuzzerRunner>(runner_);
    runner_impl->SetTargetAdapterHandler(context_->MakeRequestHandler<TargetAdapter>());
    ASSERT_EQ(runner_impl->BindCoverageDataProvider(context_->TakeChannel(1)), ZX_OK);
  }

  const RunnerPtr& runner() const { return runner_; }

 private:
  ComponentContextPtr context_;
  RunnerPtr runner_;
};

TEST_F(FuzzerTest, SeedCorpus) {
  auto runner = this->runner();

  std::vector<std::string> args;
  FUZZING_EXPECT_OK(runner->Initialize("/pkg", args));
  RunUntilIdle();

  auto corpus = runner->GetCorpus(CorpusType::SEED);
  corpus.emplace_back(Input());
  FX_LOGS(INFO) << "Testing with " << corpus.size() << " input(s).";

  FuzzResult fuzz_result;
  FUZZING_EXPECT_OK(runner->TryEach(std::move(corpus)), &fuzz_result);
  RunUntilIdle();

  EXPECT_EQ(fuzz_result, FuzzResult::NO_ERRORS);
}

}  // namespace fuzzing
