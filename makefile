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

info: ## Print build info
	@echo "HOST_OS=$(HOST_OS)"
	@echo "HOST_ARCH=$(HOST_ARCH)"
	@cat $(OUTPUT)/args.gn
.PHONY: info

args: ## Set up build dir and arguments file
	$(NOECHO)mkdir -p $(OUTPUT)
	$(NOECHO)echo "target_os=\"mistos\"" > $(OUTPUT)/args.gn
.PHONY: args

gen: ## Generate ninja
	$(NOECHO)$(GN) gen $(OUTPUT)
.PHONY: gen

compile_commands: ## Generate ninja (with compile_commands.json to be imported by IDE)
	$(NOECHO)$(NINJA) -C $(OUTPUT) kernel_x64/kernel.zbi -t compdb > compile_commands.json
.PHONY: compile_commands

it: args gen info ## Build multiboot(bootloader) and kernel zircon binary image(zbi)
	$(NOECHO)$(NINJA) -C $(OUTPUT) multiboot.bin kernel_x64/kernel.zbi
.PHONY: it

all: args gen info ## Build all targets
	$(NOECHO)$(NINJA) -C $(OUTPUT)
.PHONY: all

rain: ## Run qemu with precompiled images (do not rebuild)
	$(NOECHO) $(MISTOSROOT)/zircon/scripts/run-zircon-x64 -q $(MISTOSROOT)/prebuilt/third_party/qemu/$(HOST_OS)-$(HOST_ARCH)/bin \
	-t $(OUTPUT)/multiboot.bin \
	-z $(OUTPUT)/kernel_x64/kernel.zbi -c "kernel.shell=true" -- -no-reboot
.PHONY: rain

kasan: ## Compile with Kernel Address Sanitazier enabled
	$(NOECHO)echo "select_variant = [ \"kasan\" ]" >> $(OUTPUT)/args.gn
.PHONY: kasan

zxtest: ## Enable zxtest
	$(NOECHO)echo "register_zxtest=true" >> $(OUTPUT)/args.gn
.PHONY: zxtest

#test: args zxtest kasan gen info ## Run test kernel-zxtest.zbi ZBI (with kasan enabled)
test: args zxtest gen info ## Run test kernel-zxtest.zbi ZBI 
	$(NOECHO)$(NINJA) -C $(OUTPUT) multiboot.bin kernel_x64/kernel.zbi kernel-zxtest.zbi
	$(NOECHO)$(MISTOSROOT)/zircon/scripts/run-zircon-x64 -q $(MISTOSROOT)/prebuilt/third_party/qemu/$(HOST_OS)-$(HOST_ARCH)/bin \
	-t $(OUTPUT)/multiboot.bin \
	-z $(OUTPUT)/obj/zircon/kernel/kernel-zxtest.zbi -s1 \
	-- -no-reboot || ([ $$? -eq 31 ] && echo "Success!")
.PHONY: test

%: ## Make any ninja target
	$(NOECHO)$(NINJA) -C $(OUTPUT) $@
