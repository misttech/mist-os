This library provides a fake replacement Resource syscall purpose of testing
driver code in an unprivileged environment.  It works by defining strong symbols
for the following system calls:

- **zx_object_get_info**()
- **zx_resource_create**()
- **zx_vmo_create_physical**()
- **zx_ioports_request**()
- **zx_ioports_release**()
- **zx_smc_call**()

`<lib/fake-resource/resource.h>` provides the falling functions to configure syscall behavior:

- **fake_root_resource_create**()
- **fake_smc_set_handler**()
- **fake_smc_set_results**()
- **fake_smc_unset**()