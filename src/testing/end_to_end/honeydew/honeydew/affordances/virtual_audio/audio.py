# Copyright 2025 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Abstract base class for Audio affordance."""

import abc

from honeydew.affordances import affordance
from honeydew.affordances.virtual_audio import types


class VirtualAudio(affordance.Affordance):
    """Abstract base class for Audio affordance."""

    @abc.abstractmethod
    def inject(self, wav_file: str) -> types.AudioInputWaiter:
        """Inject wav_file audio query.
        Args:
            wav_file: Audio .wav file

        Return:
            AudioInputWaiter: object. This object is used to wait until the input injection is done.

        Raises:
            VirtualAudioError: On failure
        """

    @abc.abstractmethod
    def capture(self) -> types.AudioResponse:
        """Start to capture the audio response.

        Return:
            AudioResponse: object. This object is used to stop and extract the captured audio.

        Raises:
            VirtualAudioError: On failure
        """
