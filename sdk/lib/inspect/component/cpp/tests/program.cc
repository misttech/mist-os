// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/inspect/component/cpp/component.h>

using inspect::ComponentInspector;
using inspect::Inspector;
using inspect::PublishVmo;

int main() {
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);
  auto* dispatcher = loop.dispatcher();

  auto ci = ComponentInspector(dispatcher, {.tree_name = "ComponentInspector"});

  ci.root().RecordInt("val1", 1);
  ci.root().RecordInt("val2", 2);
  ci.root().RecordInt("val3", 3);
  ci.root().RecordLazyNode("child", [] {
    inspect::Inspector insp;
    insp.GetRoot().RecordInt("val", 0);
    return fpromise::make_ok_promise(std::move(insp));
  });
  ci.root().RecordLazyValues("values", [] {
    inspect::Inspector insp;
    insp.GetRoot().RecordInt("val4", 4);
    return fpromise::make_ok_promise(std::move(insp));
  });

  Inspector insp;
  PublishVmo(dispatcher, insp.DuplicateVmo(), {.tree_name = "VmoServer"});

  insp.GetRoot().RecordString("value1", "only in VMO");
  insp.GetRoot().RecordInt("value2", 10);

  ci.Health().Ok();
  loop.Run();

  return 0;
}
