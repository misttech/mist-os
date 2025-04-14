# Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

HOST_PLATFORM := $(shell uname -s | tr '[:upper:]' '[:lower:]')
HOST_ARCH := $(shell uname -m)

ifeq ($(HOST_ARCH),x86_64)
HOST_ARCH = x64
else ifeq ($(HOST_ARCH),aarch64)
HOST_ARCH = arm64
else
$(error Unsupported host architecture: $(HOST_ARCH))
endif

ifeq ($(HOST_PLATFORM),linux)
HOST_OS = linux
else ifeq ($(HOST_PLATFORM),darwin)
HOST_OS = mac
else
$(error Unsupported host platform: $(HOST_PLATFORM))
endif

# Build variables
MISTOSROOT ?= $(PWD)
OUTPUT ?= out/default
GN ?= $(MISTOSROOT)/prebuilt/third_party/gn/$(HOST_OS)-$(HOST_ARCH)/gn
NINJA ?= $(MISTOSROOT)/prebuilt/third_party/ninja/$(HOST_OS)-$(HOST_ARCH)/ninja
NOECHO ?= @

# By default, also show the number of actively running actions.
export NINJA_STATUS="[%f/%t][%p/%w](%r) "
# By default, print the 4 oldest commands that are still running.
export NINJA_STATUS_MAX_COMMANDS=4
export NINJA_STATUS_REFRESH_MILLIS=100
export NINJA_PERSISTENT_MODE=0


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
	$(NOECHO)if [ -f vendor/misttech/additional_default_targets.gni ]; then \
		cat vendor/misttech/additional_default_targets.gni >> $(OUTPUT)/args.gn; \
	fi
.PHONY: args

debug: args ## Set debug arguments
	$(NOECHO)echo "compilation_mode = \"debug\"" >> $(OUTPUT)/args.gn
.PHONY: debug

gdb: args ## Set debug arguments
	$(NOECHO)echo "compress_debuginfo = \"none\"" >> $(OUTPUT)/args.gn
	$(NOECHO)echo "optimize = \"debug\"" >> $(OUTPUT)/args.gn
	$(NOECHO)echo "zircon_optimize = \"debug\"" >> $(OUTPUT)/args.gn
	$(NOECHO)echo "kernel_extra_defines = [ \"DISABLE_KASLR\" ]" >> $(OUTPUT)/args.gn
.PHONY: gdb

release: args ## Set release arguments
	$(NOECHO)echo "compilation_mode = \"release\"" >> $(OUTPUT)/args.gn
.PHONY: release

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

kernel_unit_test: gen info ## Make kernel-unittests-boot-test.zbi
	$(NOECHO)$(NINJA) -C $(OUTPUT) kernel.phys32/linux-x86-boot-shim.bin zircon/kernel:kernel-unittests-boot-test.zbi
.PHONY: kernel_unit_test

run_kernel_unit_test: ## Run test kernel-unittests-boot-test.zbi
	$(NOECHO)$(MISTOSROOT)/zircon/scripts/run-zircon-x64 -q $(MISTOSROOT)/prebuilt/third_party/qemu/$(HOST_OS)-$(HOST_ARCH)/bin \
	-s 1 \
	-m 4096 \
	-t $(OUTPUT)/kernel.phys32/linux-x86-boot-shim.bin \
	-z $(OUTPUT)/obj/zircon/kernel/kernel-unittests-boot-test.zbi \
	-- -no-reboot || ([ $$? -eq 31 ] && echo "Success!")
.PHONY: run_kernel_unit_test

ci: debug kasan gen info ## Run test kernel-zxtest.zbi with kasan
	$(NOECHO)$(NINJA) -C $(OUTPUT) kernel.phys32/linux-x86-boot-shim.bin kernel_x64/kernel.zbi
	$(NOECHO)$(MISTOSROOT)/zircon/scripts/run-zircon-x64 -q $(MISTOSROOT)/prebuilt/third_party/qemu/$(HOST_OS)-$(HOST_ARCH)/bin \
	-s 1 \
	-t $(OUTPUT)/kernel.phys32/linux-x86-boot-shim.bin \
	-z $(OUTPUT)/obj/zircon/kernel/kernel-unittests-boot-test.zbi \
	-c "kernel.bypass-debuglog=true" \
	-- -no-reboot || ([ $$? -eq 31 ] && echo "Success!")
.PHONY: ci

starnix_lite_kernel: gen info
	$(NOECHO)$(NINJA) -C $(OUTPUT) kernel.phys32/linux-x86-boot-shim.bin starnix_lite_kernel_zbi
	$(NOECHO)$(MISTOSROOT)/zircon/scripts/run-zircon-x64 -q $(MISTOSROOT)/prebuilt/third_party/qemu/$(HOST_OS)-$(HOST_ARCH)/bin \
	-s 1 \
	-t $(OUTPUT)/kernel.phys32/linux-x86-boot-shim.bin \
	-z $(OUTPUT)/starnix_lite_kernel.zbi \
	-c "kernel.bypass-debuglog=false kernel.vdso.always_use_next=true" \
	-- -no-reboot || ([ $$? -eq 31 ] && echo "Success!")
.PHONY: starnix_lite_kernel

iso:
	$(NOECHO)$(NINJA) -C $(OUTPUT) kernel.phys32/multiboot-shim.bin
	$(NOECHO)$(NINJA) -C $(OUTPUT) kernel_x64/kernel.zbi
	$(MISTOSROOT)/zircon/scripts/make-zircon-x64-grub
.PHONY: iso

img: iso
	$(NOECHO)$(MISTOSROOT)/prebuilt/third_party/qemu/$(HOST_OS)-$(HOST_ARCH)/bin/qemu-img convert -O raw out/default/mistos.iso out/default/mistos.img
.PHONY: img

%: ## Make any ninja target
	$(NOECHO)$(NINJA) -C $(OUTPUT) $@
