# HoneyDew

HoneyDew is a test framework agnostic device controller written in Python that
provides Host-(Fuchsia)Target interaction.

## Usage

### Device object creation
```python

In [1]: import logging
   ...: logging.basicConfig(level=logging.INFO)

In [2]: import honeydew

# honeydew.create_device() will look for a specific Fuchsia device class implementation that matches the device type specified and if it finds, it returns that specific device type object, else returns GenericFuchsiaDevice object.
# In below examples,
#   * "fuchsia-54b2-038b-6e90" is a x64 device whose implementation is present in HoneyDew. Hence returning honeydew.device_classes.x64.X64 object.
#   * "fuchsia-d88c-79a3-aa1d" is Google's 1p device whose implementation is not present in HoneyDew. Hence returning a generic_fuchsia_device.GenericFuchsiaDevice object.
#   * "fuchsia-emulator" is an emulator device whose implementation is not present in HoneyDew. Hence returning a generic_fuchsia_device.GenericFuchsiaDevice object.

In [3]: fd_1p = honeydew.create_device("fuchsia-ac67-847a-2e50")
INFO:honeydew:Registered device classes with HoneyDew '{<class 'honeydew.device_classes.generic_fuchsia_device.GenericFuchsiaDevice'>, <class 'honeydew.device_classes.x64.X64'>, <class 'honeydew.device_classes.fuchsia_device_base.FuchsiaDeviceBase'>}'
INFO:honeydew:Didn't find any matching device class implementation for 'fuchsia-ac67-847a-2e50'. So returning 'GenericFuchsiaDevice'
INFO:honeydew.device_classes.fuchsia_device_base:Starting SL4F server on fuchsia-ac67-847a-2e50...

In [4]: type(fd_1p)
Out[4]: honeydew.device_classes.generic_fuchsia_device.GenericFuchsiaDevice

In [5]: ws = honeydew.create_device("fuchsia-54b2-038b-6e90")
INFO:honeydew:Registered device classes with HoneyDew '{<class 'honeydew.device_classes.generic_fuchsia_device.GenericFuchsiaDevice'>, <class 'honeydew.device_classes.x64.X64'>, <class 'honeydew.device_classes.fuchsia_device_base.FuchsiaDeviceBase'>}'
INFO:honeydew:Found matching device class implementation for 'fuchsia-54b2-038b-6e90' as 'X64'
INFO:honeydew.device_classes.fuchsia_device_base:Starting SL4F server on fuchsia-54b2-038b-6e90...

In [6]: type(ws)
Out[6]: honeydew.device_classes.x64.X64

In [7]: emu = honeydew.create_device("fuchsia-emulator")
INFO:honeydew:Registered device classes with HoneyDew '{<class 'honeydew.device_classes.generic_fuchsia_device.GenericFuchsiaDevice'>, <class 'honeydew.device_classes.x64.X64'>, <class 'honeydew.device_classes.fuchsia_device_base.FuchsiaDeviceBase'>}'
INFO:honeydew:Didn't find any matching device class implementation for 'fuchsia-emulator'. So returning 'GenericFuchsiaDevice'
INFO:honeydew.device_classes.fuchsia_device_base:Starting SL4F server on fuchsia-emulator...

In [8]: type(emu)
Out[8]: honeydew.device_classes.generic_fuchsia_device.GenericFuchsiaDevice

```

### Access the static properties
```python

In [9]: emu.name
Out[9]: 'fuchsia-emulator'

In [10]: emu.device_type
Out[10]: 'qemu-x64'

In [11]: emu.product_name
Out[11]: 'default-fuchsia'

In [12]: emu.manufacturer
Out[12]: 'default-manufacturer'

In [13]: emu.model
Out[13]: 'default-model'

In [14]: emu.serial_number

```

### Access the dynamic properties
```python

In [15]: emu.firmware_version
Out[15]: '2023-02-01T17:26:40+00:00'

```

### Access the public methods
```python

In [16]: emu.reboot()
INFO:honeydew.device_classes.fuchsia_device_base:Rebooting fuchsia-emulator...
INFO:honeydew.device_classes.fuchsia_device_base:Waiting for fuchsia-emulator to go offline...
INFO:honeydew.device_classes.fuchsia_device_base:fuchsia-emulator is offline.
INFO:honeydew.device_classes.fuchsia_device_base:Waiting for fuchsia-emulator to become pingable...
INFO:honeydew.device_classes.fuchsia_device_base:fuchsia-emulator now pingable.
INFO:honeydew.device_classes.fuchsia_device_base:Waiting for fuchsia-emulator to allow ssh connection...
Warning: Permanently added 'fe80::55da:a912:5df:ee98%qemu' (ED25519) to the list of known hosts.
INFO:honeydew.device_classes.fuchsia_device_base:fuchsia-emulator is available via ssh.
INFO:honeydew.device_classes.fuchsia_device_base:Starting SL4F server on fuchsia-emulator...
WARNING:honeydew.utils.http_utils:Send HTTP request failed with error: '<urlopen error [Errno 111] Connection refused>' on iteration 1/3

In [17]: emu.log_message_to_device(message="This is a test INFO message logged by HoneyDew", level=honeydew.custom_types.LEVEL.INFO)

In [18]: emu.snapshot(directory="/tmp/")
INFO:honeydew.device_classes.fuchsia_device_base:Snapshot file has been saved @ '/tmp/Snapshot_fuchsia-emulator_2023-03-01-01-09-43-PM.zip'
Out[18]: '/tmp/Snapshot_fuchsia-emulator_2023-03-01-01-09-43-PM.zip'

```

### Access the affordances
#### Component affordance
```python

In [19]: emu.component.search("wlanstack.cm")
Out[19]: True

```

### Device object destruction
```python

In [20]: emu.close()
In [21]: del emu

```
