This directory contains hand-crafted textual SELinux policies and their compiled binary policies.

Policies can be complied using the `checkpolicy` Linux utility:

```
checkpolicy --mls -c 33 --output testdata/micro_policies/${POLICY_NAME}_policy.pp -t selinux testdata/micro_policies/${POLICY_NAME}_policy.conf
```
