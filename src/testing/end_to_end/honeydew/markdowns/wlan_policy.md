# Wlan policy affordance

[TOC]

This page talks about [wlan_policy] affordance in HoneyDew.

## Usage
```python
>>> fd_1p.wlan_policy.create_client_controller()
>>> fd_1p.wlan_policy.save_network("ssid1", FuchsiaSecurityType.NONE)
>>> fd_1p.wlan_policy.save_network("ssid2", FuchsiaSecurityType.WEP, "password")
>>> fd_1p.wlan_policy.get_saved_networks()
>>> fd_1p.wlan_policy.get_update()
>>> fd_1p.wlan_policy.remove_all_networks()
>>> fd_1p.wlan_policy.set_new_update_listener()
>>> fd_1p.wlan_policy.start_client_connections()
>>> fd_1p.wlan_policy.get_update()
>>> fd_1p.wlan_policy.stop_client_connections()
```

[wlan_policy]: ../interfaces/affordances/wlan/wlan_policy.py
