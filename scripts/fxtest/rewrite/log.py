# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import json
import typing

import event


async def writer(
    recorder: event.EventRecorder,
    out_stream: typing.TextIO,
):
    """Asynchronously serialize events to the given stream.

    Args:
        recorder (event.EventRecorder): The source of events to
            drain. Continues until all events are written.
        out_stream (typing.TextIO): Output text stream.
    """
    value: event.Event
    async for value in recorder.iter():
        json.dump(value.to_dict(), out_stream)  # type:ignore
        out_stream.write("\n")
