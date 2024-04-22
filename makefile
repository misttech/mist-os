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

info: gen ## Print build info
	@echo "HOST_OS=$(HOST_OS)"
	@echo "HOST_ARCH=$(HOST_ARCH)"
	@cat $(OUTPUT)/args.gn
.PHONY: info

args: ## Set build arguments file
	$(NOECHO)mkdir -p $(OUTPUT)
	$(NOECHO)echo "target_os=\"mistos\"" > $(OUTPUT)/args.gn
.PHONY: args

gen: args ## Generate ninja
	$(NOECHO)$(GN) gen $(OUTPUT)
.PHONY: gen

compile_commands: args ## Generate ninja (with compile_commands.json to be imported by IDE)
	$(NOECHO)$(NINJA) -C $(OUTPUT) kernel_x64/kernel.zbi -t compdb > compile_commands.json
.PHONY: compile_commands

it: info gen ## Build multiboot(bootloader) and kernel zircon binary image(zbi)
	$(NOECHO)$(NINJA) -C $(OUTPUT) multiboot.bin kernel_x64/kernel.zbi
.PHONY: it

rain: ## Run qemu with precompiled images (do not rebuild)
	$(NOECHO) $(MISTOSROOT)/zircon/scripts/run-zircon-x64 -q $(MISTOSROOT)/prebuilt/third_party/qemu/$(HOST_OS)-$(HOST_ARCH)/bin \
	-t $(OUTPUT)/multiboot.bin \
	-z $(OUTPUT)/kernel_x64/kernel.zbi -c "kernel.shell=true" -- -no-reboot
.PHONY: rain

%: ## Make any ninja target
	$(NOECHO)$(NINJA) -C $(OUTPUT) $@
