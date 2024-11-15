# binder-proxy

This component will be the Binder RPC proxy between the TEE and external clients
including the host VM and (one day) other VMs. The proxy exposes the
Binder RPC protocol IMicrofuchsia over virtio-socket on the port specified in
the binder definition (currently 5680).
