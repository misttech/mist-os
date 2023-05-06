#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""HoneyDew python module."""

import importlib
import inspect
import logging
import os
import pkgutil
import types
from typing import Any, List, Optional, Set, Type

from honeydew import device_classes
from honeydew.device_classes import generic_fuchsia_device
from honeydew.interfaces.device_classes import fuchsia_device
from honeydew.transports import ffx as ffx_transport
from honeydew.utils import properties

_LOGGER: logging.Logger = logging.getLogger(__name__)

_DEVICE_CLASSES_MODULE = "honeydew.device_classes"
_REGISTERED_DEVICE_CLASSES: Set[Type[fuchsia_device.FuchsiaDevice]] = set()


# pytype: disable=not-instantiable
# List all the public methods in alphabetical order
def create_device(
        device_name: str,
        ssh_private_key: Optional[str] = None,
        ssh_user: Optional[str] = None) -> fuchsia_device.FuchsiaDevice:
    """Factory method that creates and returns the device class.

    This method will look at all the device class implementations available, and
    if it finds a match it will return the corresponding device class object.
    If not, GenericFuchsiaDevice instance will be returned.

    Args:
        device_name: Device name returned by `ffx target list`.

        ssh_private_key: Absolute path to the SSH private key file needed to SSH
            into fuchsia device.

        ssh_user: Username to be used to SSH into fuchsia device.
            Default is "fuchsia".

    Returns:
        Fuchsia device object

    Raises:
        errors.FuchsiaDeviceError: Failed to create Fuchsia device object.
    """
    device_class = _get_device_class(device_name)
    device_class_args = (device_name, ssh_private_key, ssh_user)

    return device_class(*device_class_args)  # type: ignore[call-arg]


def get_all_affordances(device_name: str) -> List[str]:
    """Returns list of all affordances implemented for this device class.

    Please note that this method returns list of affordances implemented for
    this device class. This is not same as affordances supported by the device.

    Args:
        device_name: Device name returned by `ffx target list`.

    Returns:
        List of affordances implemented for this device class.
    """
    device_class = _get_device_class(device_name)

    affordances: List[str] = []
    for attr in dir(device_class):
        if attr.startswith("_"):
            continue
        attr_type: Any | None = getattr(device_class, attr, None)
        if isinstance(attr_type, properties.Affordance):
            affordances.append(attr)
    return affordances


def get_device_classes(
        device_classes_path: str, device_classes_module_name: str
) -> Set[Type[fuchsia_device.FuchsiaDevice]]:
    """Get set of all device classes located in specified path and module name.

    This is useful if caller has custom device class implementation(s). So
    caller should first call this method to register those custom
    implementations prior to calling create_device so that create_device is
    aware of these custom device class implementations.

    Note:
        We need this method for HoneyDew to know what all the device class
        implementations exists. This method will be called:
        * once to locate all device classes implemented with in HoneyDew at
          honeydew.device_classes
        * once to locate all device classes implemented elsewhere but got
          registered with HoneyDew (using register_device_classes)

    Args:
        device_classes_path: Absolute path of the device classes
        device_classes_module_name: module name of the device classes

    Returns:
        set of all device classes
    """
    fuchsia_device_classes: Set[Type[fuchsia_device.FuchsiaDevice]] = set()
    for _, module_name, _ in pkgutil.iter_modules([device_classes_path]):
        if module_name.startswith("__"):
            continue

        module: types.ModuleType = importlib.import_module(
            "." + module_name, package=device_classes_module_name)

        # Iterate items inside imported python file
        for item in dir(module):
            value: Any = getattr(module, item)
            if not value:
                continue

            if not inspect.isclass(value):
                continue

            if inspect.isabstract(value):
                continue

            if device_classes_module_name in value.__module__:
                fuchsia_device_classes.add(value)
    return fuchsia_device_classes


def register_device_classes(
        fuchsia_device_classes: Set[Type[fuchsia_device.FuchsiaDevice]]
) -> None:
    """Registers a custom fuchsia device classes implementation.

    Args:
        fuchsia_device_classes: Set of fuchsia device class modules
    """
    _LOGGER.info(
        "Registering device classes '%s' with HoneyDew", fuchsia_device_classes)
    _REGISTERED_DEVICE_CLASSES.update(fuchsia_device_classes)


# List all the private methods in alphabetical order
def _get_all_register_device_classes(
) -> Set[Type[fuchsia_device.FuchsiaDevice]]:
    """Get list of all custom fuchsia device class implementations registered
    with HoneyDew.

    Returns:
        Set of all the registered device classes
    """
    device_classes_path: str = os.path.dirname(device_classes.__file__)
    this_package_device_classes: Set[Type[
        fuchsia_device.FuchsiaDevice]] = get_device_classes(
            device_classes_path, _DEVICE_CLASSES_MODULE)

    all_device_classes: Set[Type[
        fuchsia_device.FuchsiaDevice]] = _REGISTERED_DEVICE_CLASSES.union(
            this_package_device_classes)

    _LOGGER.info(
        "Registered device classes with HoneyDew '%s'", all_device_classes)
    return all_device_classes


def _get_device_class(device_name: str) -> Type[fuchsia_device.FuchsiaDevice]:
    """Returns device class associated with the device.

    Args:
        device_name: Device name returned by `ffx target list`.

    Returns:
        Device class type.
    """
    ffx: ffx_transport.FFX = ffx_transport.FFX(target=device_name)
    product_type: str = ffx.get_target_type()

    for device_class in _get_all_register_device_classes():
        if product_type.lower() == device_class.__name__.lower():
            _LOGGER.info(
                "Found matching device class implementation for '%s' as '%s'",
                device_name, device_class.__name__)
            return device_class
    _LOGGER.info(
        "Didn't find any matching device class implementation for '%s'. "
        "So returning '%s'", device_name,
        generic_fuchsia_device.GenericFuchsiaDevice.__name__)
    return generic_fuchsia_device.GenericFuchsiaDevice
