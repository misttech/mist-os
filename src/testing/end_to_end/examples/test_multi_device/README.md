## Multi-device Local Testing Guide

### Pre-requisites
1. Host Linux machine
2. Lacewing and Honeydew guides were installed and completed. [Lacewing](https://cs.opensource.google/fuchsia/fuchsia/+/main:src/testing/end_to_end/README.md) and [Honeydew](https://cs.opensource.google/fuchsia/fuchsia/+/main:src/testing/end_to_end/honeydew/README.md)
3. Ability to run a Lacewing test with one Fuchsia device [Example Test](https://fuchsia.googlesource.com/fuchsia/+/refs/heads/main/src/testing/end_to_end/examples#Soft-Reboot-Test)

### Intro
This README will document the steps to setup multiple Fuchsia devices for Lacewing and/or Honeydew testing. Please ensure that you have at least two Fuchsia devices ready. For one device setup, please go to
[Lacewing End to End Framework](https://cs.opensource.google/fuchsia/fuchsia/+/main:src/testing/end_to_end/README.md)

This example will use two Smart Displays with
```sh
$ smart_display_max_eng_arrested.sherlock
```
Then, after we setup the two devices, we will run a Bluetooth Sample test which will emulate connecting the two devices.

### Steps (Example flow as of Sept 27 2023)
1. Connect the two Fuchsia devices to the host machine.
2. If you already have the builds flashed and devices accessible via ssh, then skip to step #4.
3. Let's flash the builds to our devices respectively.

    Please follow [Flash Fuchsia and start a tunnel](https://fuchsia.dev/internal/intree/get-started/flash-fuchsia-and-start-a-tunnel)

    For an example flow, we will use the two devices and their names:

    Device 1: fuchsia-f80f-f96b-6f59

    Device 2: fuchsia-201f-3b62-e9d3

    ```sh
    $ fx set smart_display_max_eng.sherlock --release

    $ fx build

    $ fx -d fuchsia-f80f-f96b-6f59 flash

    $ fx -d fuchsia-f80f-f96b-6f59 serve

    $ ffx -t fuchsia-f80f-f96b-6f59 target show
    # This should return all the values and no errors.

    # Follow the same for Device 2: we don't need to rebuild
    $ fx -d fuchsia-201f-3b62-e9d3 flash

    $ fx -d fuchsia-201f-3b62-e9d3 serve

    $ ffx -t fuchsia-201f-3b62-e9d3 target show
    ```

4. Stabilize the connection for multi-device setup. For each device, follow the below steps
    1. Navigate to Network Settings menu of your Linux host [step 1](../images/multi_device_4a.png)
    2. Find the USB Ethernet Interface, then click the + (Plus) symbol [step 2](../images/multi_device_4b.png)
    3. In the New Profile window, in the "Identity" section, Create an unique name; i.e. FuchsiaDevice1 [step 3](../images/multi_device_4c.png)
    4. Switch to "IPv4" tab and select "Disable" [step 4](../images/multi_device_4d.png)
    5. Switch to "IPv6" tab and select "Link-Local Only" [step 5](../images/multi_device_4e.png)
    6. Then click Add on the top right.

    If this was done correctly, you will see the newly created profile under "USB Ethernet".
    Click on the name of the profile you created. If done correctly, a Checkmark will appear next to the new name profile.
    (We are now using IPv6) Repeat for all the number of devices accordingly.

5. Afterwards, ensure that this stable connection is complete via
    ```sh
    $ ffx target list
    NAME                      SERIAL            TYPE                                       STATE      ADDRS/IP                                       RCS
    fuchsia-f80f-f96b-6f59    04140YCABZZ25M    smart_display_max_eng_arrested.sherlock    Product    [fe80::4a9c:d65:1e95:999e%enxf80ff96b6f58]     Y
    fuchsia-201f-3b62-e9d3*   1C281F4ABZZ07Z    smart_display_max_eng_arrested.sherlock    Product    [fe80::f02f:c160:bfbf:3690%enx201f3b62e9d2]    Y
    ```

6. Finally, determine if you need to provide a local Mobly config yaml file.
    1. If any combination of the connected devices can be used during the test, skip this step.
    2. If only specific subsets of connected devices can be used for testing, provide a handcrafted local Mobly config YAML file See example below of a local Mobly config YAML.
        ```sh
        Bluetooth_Test.yaml
        ```
        to point our testbeds to those devices.
        ```yaml
        TestBeds:
          - Name: Testbed-One-BT
            Controllers:
              FuchsiaDevice:
                - name: fuchsia-201f-3b62-e9d3
                  ssh_private_key: ~/.ssh/fuchsia_ed25519
                - name: fuchsia-f80f-f96b-6f59
                  ssh_private_key: ~/.ssh/fuchsia_ed25519
        ```
        Save this file inside the same folder as the test.
        Finally, update the BUILD.gn that you had created in [Lacewing Getting Started Guide](../../README.md). Below is an example of an updated BUILD.gn with the local_config_source for this multi device test
        ```sh
        python_mobly_test("test_multi_device") {
          main_source = "test_multi_device.py"

          # The library below provides device interaction APIs.
          libraries = [
            "//src/testing/end_to_end/honeydew",
            "//src/testing/end_to_end/mobly_base_tests:fuchsia_base_test",
          ]
          local_config_source = "Bluetooth_Test.yaml"
          params_source = "params.yaml"
        }
        ```
### Execution
Finally, let's run the test!
```sh
$ fx set core.vim3 \
    --args=core_realm_shards+="[\"//src/testing/sl4f:sl4f_core_shard\"]" \
    --with //src/testing/end_to_end/examples

$ fx test //src/testing/end_to_end/examples/test_multi_device:multi_device_test_sl4f --e2e --output
```
