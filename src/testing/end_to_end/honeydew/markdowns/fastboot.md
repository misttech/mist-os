# Fastboot transport

[TOC]

This page talks about Fastboot transport in HoneyDew.

## Usage
```python
>>> sherlock.fastboot.is_in_fuchsia_mode()
True
>>> sherlock.fastboot.is_in_fastboot_mode()
False


>>> sherlock.fastboot.boot_to_fastboot_mode()
INFO:honeydew.transports.fastboot:Waiting for fuchsia-d88c-799b-0e3a to go fastboot mode...
INFO:honeydew.utils.common:Waiting for 45 sec for Fastboot.is_in_fastboot_mode to return True...
INFO:honeydew.transports.fastboot:fuchsia-d88c-799b-0e3a is in fastboot mode...
>>> sherlock.fastboot.is_in_fastboot_mode()
True
>>> sherlock.fastboot.is_in_fuchsia_mode()
False

>>> sherlock.fastboot.run(["getvar", "hw-revision"])
['hw-revision: sherlock-b4']

>>> sherlock.fastboot.boot_to_fuchsia_mode()
INFO:honeydew.transports.fastboot:Waiting for fuchsia-d88c-799b-0e3a to go fuchsia mode...
INFO:honeydew.utils.common:Waiting for 45 sec for Fastboot.is_in_fuchsia_mode to return True...
INFO:honeydew.transports.fastboot:fuchsia-d88c-799b-0e3a is in fuchsia mode...
INFO:honeydew.device_classes.sl4f.fuchsia_device:Waiting for fuchsia-d88c-799b-0e3a to go online...
INFO:honeydew.device_classes.sl4f.fuchsia_device:fuchsia-d88c-799b-0e3a is online.
INFO:honeydew.transports.sl4f:Starting SL4F server on fuchsia-d88c-799b-0e3a...
>>> sherlock.fastboot.is_in_fuchsia_mode()
True
>>> sherlock.fastboot.is_in_fastboot_mode()
False
```
