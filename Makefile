.PHONY: help bundle iso iso-docker install-image

OPENSHELL_DIR ?= ../openshell
BUNDLE ?= $(OPENSHELL_DIR)/apps/desktop/dist-bundle/openshell-desktop
NODE ?= node
PNPM ?= pnpm

help:
	@echo "  make bundle         build OpenShell Desktop"
	@echo "  make iso            generate the UEFI live ISO and SHA-256 with Docker"
	@echo "  make iso-docker     same as make iso"
	@echo "  make install-image generate the UEFI GPT disk image and SHA-256 with Docker"
	@echo "  Set BUNDLE to use an existing Desktop bundle directory."

bundle:
	$(PNPM) --dir "$(OPENSHELL_DIR)" install --frozen-lockfile --filter '@openshell/desktop...' --filter '@openshell/web...'
	$(PNPM) --dir "$(OPENSHELL_DIR)" --filter '@openshell/desktop' bundle

iso iso-docker:
	$(NODE) tools/build/native.mjs live-iso "$(BUNDLE)"

install-image:
	$(NODE) tools/build/native.mjs install-image "$(BUNDLE)"
