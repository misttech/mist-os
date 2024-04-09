#!/usr/bin/env fuchsia-vendored-python
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Abstract base class for SystemPowerStateController affordance."""

import abc
from dataclasses import dataclass
from typing import ClassVar


@dataclass(frozen=True)
class SuspendState(abc.ABC):
    """Abstract base class for different suspend states"""


@dataclass(frozen=True)
class IdleSuspend(SuspendState):
    """Idle suspend mode"""

    def __str__(self) -> str:
        return "IdleSuspend"


@dataclass(frozen=True)
class ResumeMode(abc.ABC):
    """Abstract base class for different resume modes"""


@dataclass(frozen=True)
class AutomaticResume(ResumeMode):
    """Automatically resume after 5sec"""

    duration: ClassVar[float] = 5

    def __str__(self) -> str:
        return f"AutomaticResume after {self.duration}sec"


@dataclass(frozen=True)
class ButtonPressResume(ResumeMode):
    """Resumes only on the button press"""

    def __str__(self) -> str:
        return "ButtonPressResume"


class SystemPowerStateController(abc.ABC):
    """Abstract base class for SystemPowerStateController affordance."""

    # List all the public methods

    # Note - Creating this method based on the current understanding.
    # Once we learn more about suspend-resume feature, if needed we can update
    # this interface (such as splitting this API into multiple etc) accordingly
    # to meet the feature needs.
    @abc.abstractmethod
    def suspend_resume(
        self,
        suspend_state: SuspendState,
        resume_mode: ResumeMode,
    ) -> None:
        """Perform suspend-resume operation on the device.

        Args:
            suspend_state: Which state to suspend the Fuchsia device into.
            resume_mode: Information about how to resume the device.

        Raises:
            errors.SystemPowerStateControllerError: In case of failure
            errors.NotSupportedError: If any of the suspend_state or resume_type
                is not yet supported
        """

    @abc.abstractmethod
    def idle_suspend_auto_resume(self) -> None:
        """Perform idle-suspend and auto-resume operation on the device.

        Raises:
            errors.SystemPowerStateControllerError: In case of failure
        """
