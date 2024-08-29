# Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# Build variables
MISTOSROOT ?= $(PWD)
OUTPUT ?= out/default
HOST_ARCH ?= $(shell $(MISTOSROOT)/meta/scripts/host-arch.sh)
HOST_OS ?= $(shell $(MISTOSROOT)/meta/scripts/host-os.sh)
GN ?= $(MISTOSROOT)/prebuilt/third_party/gn/$(HOST_OS)-$(HOST_ARCH)/gn
NINJA ?= $(MISTOSROOT)/prebuilt/third_party/ninja/$(HOST_OS)-$(HOST_ARCH)/ninja
NOECHO ?= @

export NINJA_STATUS_MAX_COMMANDS=4
export NINJA_STATUS_REFRESH_MILLIS=100
export NINJA_PERSISTENT_MODE=0
export NINJA_STATUS=[%f/%t](%r)

# Some tools depends on this env var.
export FUCHSIA_BUILD_DIR=$(MISTOSROOT)/$(OUTPUT)

info: ## Print build info
	@echo "HOST_OS=$(HOST_OS)"
	@echo "HOST_ARCH=$(HOST_ARCH)"
	@cat $(OUTPUT)/args.gn
	@echo "FUCHSIA_BUILD_DIR=$(MISTOSROOT)"
.PHONY: info

args: ## Set up build dir and arguments file
	$(NOECHO)mkdir -p $(OUTPUT)
	$(NOECHO)echo "# Basic args:" > $(OUTPUT)/args.gn
	$(NOECHO)echo "target_os = \"mistos\"" >> $(OUTPUT)/args.gn
	$(NOECHO)echo "host_test_labels = []" >> $(OUTPUT)/args.gn
	$(NOECHO)echo "rust_incremental = \"incremental\"" >> $(OUTPUT)/args.gn
	$(NOECHO)echo "host_labels = [ \"//build/rust:cargo_toml_gen\" ]" >> $(OUTPUT)/args.gn
	$(NOECHO)echo "rbe_mode = \"off\"" >> $(OUTPUT)/args.gn
	$(NOECHO)echo "platform_enable_user_pci = false" >> $(OUTPUT)/args.gn
.PHONY: args

debug: args ## Set debug arguments
	$(NOECHO)echo "is_debug = true" >> $(OUTPUT)/args.gn
.PHONY: debug

gdb: args ## Set debug arguments
	$(NOECHO)echo "compress_debuginfo = \"none\"" >> $(OUTPUT)/args.gn
	$(NOECHO)echo "optimize = \"debug\"" >> $(OUTPUT)/args.gn
	$(NOECHO)echo "zircon_optimize = \"debug\"" >> $(OUTPUT)/args.gn
	$(NOECHO)echo "kernel_extra_defines = [ \"DISABLE_KASLR\" ]" >> $(OUTPUT)/args.gn
.PHONY: gdb

release: args ## Set release arguments
	$(NOECHO)echo "is_debug = false" >> $(OUTPUT)/args.gn
.PHONY: debug

kasan: args ## Compile with Kernel Address Sanitazier enabled
	$(NOECHO)echo "select_variant = [ \"kasan\" ]" >> $(OUTPUT)/args.gn
.PHONY: kasan

host_test: args ## Enable some host tests
	$(NOECHO)echo "host_test_labels += [ \"//zircon/system/ulib/zxtest/test:zxtest\" ]" >> $(OUTPUT)/args.gn
	$(NOECHO)echo "host_test_labels += [ \"//zircon/system/ulib/fbl/test:fbl\" ]" >> $(OUTPUT)/args.gn
.PHONY: zxtest

gen: ## Generate ninja
	$(NOECHO)echo "Running:$(GN) gen $(OUTPUT)"
	$(NOECHO)$(GN) gen $(OUTPUT)
.PHONY: gen

compile_commands: ## Generate ninja (with compile_commands.json to be imported by IDE)
	$(NOECHO)$(NINJA) -C $(OUTPUT) kernel_x64/kernel.zbi -t compdb > compile_commands.json
.PHONY: compile_commands

it: gen info ## Build linux-x86-boot-shim(bootloader) and kernel zircon binary image(zbi)
	$(NOECHO)$(NINJA) -C $(OUTPUT) kernel.phys32/linux-x86-boot-shim.bin kernel_x64/kernel.zbi
.PHONY: it

all: gen info ## Build all targets
	$(NOECHO)$(NINJA) -C $(OUTPUT)
.PHONY: all

rain: ## Run qemu with precompiled images (do not rebuild)
	$(NOECHO) $(MISTOSROOT)/zircon/scripts/run-zircon-x64 -q $(MISTOSROOT)/prebuilt/third_party/qemu/$(HOST_OS)-$(HOST_ARCH)/bin \
	-t $(OUTPUT)/kernel.phys32/linux-x86-boot-shim.bin \
	-z $(OUTPUT)/kernel_x64/kernel.zbi -c "kernel.shell=true" -- -no-reboot
.PHONY: rain

test: host_test gen info ## Run test kernel-unittests-boot-test.zbi
	$(NOECHO)$(NINJA) -C $(OUTPUT) kernel.phys32/linux-x86-boot-shim.bin kernel_x64/kernel.zbi kernel-unittests-boot-test
	$(NOECHO)$(MISTOSROOT)/zircon/scripts/run-zircon-x64 -q $(MISTOSROOT)/prebuilt/third_party/qemu/$(HOST_OS)-$(HOST_ARCH)/bin \
	-s 1 \
	-t $(OUTPUT)/kernel.phys32/linux-x86-boot-shim.bin \
	-z $(OUTPUT)/obj/zircon/kernel/kernel-unittests-boot-test.zbi \
	-- -no-reboot || ([ $$? -eq 31 ] && echo "Success!")
.PHONY: test

ci: debug kasan info gen ## Run test kernel-zxtest.zbi with kasan
	$(NOECHO)$(NINJA) -C $(OUTPUT) kernel.phys32/linux-x86-boot-shim.bin kernel_x64/kernel.zbi
	$(NOECHO)$(MISTOSROOT)/zircon/scripts/run-zircon-x64 -q $(MISTOSROOT)/prebuilt/third_party/qemu/$(HOST_OS)-$(HOST_ARCH)/bin \
	-s 1 \
	-t $(OUTPUT)/kernel.phys32/linux-x86-boot-shim.bin \
	-z $(OUTPUT)/obj/zircon/kernel/kernel-unittests-boot-test.zbi \
	-c "kernel.bypass-debuglog=true" \
	-- -no-reboot || ([ $$? -eq 31 ] && echo "Success!")
.PHONY: ci

starnix_lite_kernel: info gen
	$(NOECHO)$(NINJA) -C $(OUTPUT) kernel.phys32/linux-x86-boot-shim.bin starnix_lite_kernel_zbi
	$(NOECHO)$(MISTOSROOT)/zircon/scripts/run-zircon-x64 -q $(MISTOSROOT)/prebuilt/third_party/qemu/$(HOST_OS)-$(HOST_ARCH)/bin \
	-s 1 \
	-t $(OUTPUT)/kernel.phys32/linux-x86-boot-shim.bin \
	-z $(OUTPUT)/starnix_lite_kernel.zbi \
	-c "kernel.bypass-debuglog=false kernel.vdso.always_use_next=true" \
	-- -no-reboot || ([ $$? -eq 31 ] && echo "Success!")
.PHONY: starnix_lite_kernel

%: ## Make any ninja target
	$(NOECHO)$(NINJA) -C $(OUTPUT) $@
