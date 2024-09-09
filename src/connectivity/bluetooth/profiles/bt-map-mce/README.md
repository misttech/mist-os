# Bluetooth Profile: Message Access Profile Message Client Equipment

This component implements the Message Client Equipment role of
[Message Access Profile (MAP) v1.4.2](https://www.bluetooth.com/specifications/specs/message-access-profile-1-4-2/)

## Running tests

MAP-MCE relies on unit tests to validate behavior. Add the following to your Fuchsia set configuration
to include the profile unit tests:

`--with //src/connectivity/bluetooth/profiles/bt-map-mce:tests`

To run the tests:

```
fx test bt-map-mce-unittests
```
