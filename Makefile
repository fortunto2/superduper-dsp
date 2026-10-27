# SuperDuper DSP — convenience entry point.
#
# Most users want:           make all          (CLAP + VST3 + AU, installed)
# Developers iterating CLAP: make clap         (just the .clap bundles)
# Developers iterating UI:   make <plugin>     (one bundle, e.g. `make wave`)
# Testing:                   make test         (workspace tests, all green)
# Release zips:              make release VERSION=0.11.0
#
# `make` (no args) is the same as `make all`.

# `make wave` builds one plugin: every short name in cmake/plugin_list.cmake.
PLUGINS := $(shell grep -oE '^ +"superduper-[a-z0-9-]+' cmake/plugin_list.cmake | sed 's/.*superduper-//')

.PHONY: help all clap wrappers install test test-fast clean release ios \
        tools validate hooks validator-install $(PLUGINS)

help:
	@echo "SuperDuper DSP build targets:"
	@echo ""
	@echo "  make all              CLAP + VST3 + AU, installed to ~/Library/Audio/Plug-Ins/"
	@echo "  make validate         clap-validator over dist/ bundles (make hooks: run it pre-commit;"
	@echo "                          make validator-install: the validator with tools/clap-validator-patches)"
	@echo "  make clap             Every CLAP bundle → ~/Library/Audio/Plug-Ins/CLAP/"
	@echo "  make wrappers         VST3 + AU wrappers → ~/Library/Audio/Plug-Ins/{VST3,Components}/"
	@echo "                          (requires CLAP installed first; depends on 'make clap')"
	@echo "  make <plugin>         One plugin's .clap bundle (e.g. 'make wave')"
	@echo "                          plugins: $(PLUGINS)"
	@echo "  make ios              Rebuild the live2play in-app synth XCFramework"
	@echo "                          (synth-core DSP → SDSP.xcframework in the reelcam repo)"
	@echo "  make test             cargo test --release --workspace"
	@echo "  make test-fast        Just the smoke + e2e tests (skip quality_audit)"
	@echo "  make release VERSION=0.11.0"
	@echo "                        Versioned signed zips in ./dist/"
	@echo "  make clean            Remove cargo + cmake build artefacts"

# Default target: end-to-end build + install.
all: wrappers

# Every CLAP bundle in cmake/plugin_list.cmake.
clap:
	@./scripts/build_all_bundles.sh

# VST3 + AU wrappers. Depends on `clap` since the wrappers need the
# .clap bundles installed at runtime (and the install_local target also
# copies the wrappers themselves into the user plugin folders).
wrappers: clap
	@./scripts/build_wrappers.sh --install

# `install` is the natural verb users reach for.
install: all

# Rebuild the iOS XCFramework that the live2play app links for its in-app synth. Any DSP you put
# in synth-core (the iOS-safe shared crate) flows to the phone through here — the repeatable
# "rebuild my DSP for iPhone" step. Then `make deploy` in the reelcam repo installs it.
ios:
	@./mobile/sdsp-ios/build-xcframework.sh

# Per-plugin shortcuts.
$(PLUGINS):
	@./scripts/build_plugin_bundle.sh $@

validate:
	@python3 tools/validate_claps.py

hooks:
	@./scripts/install_hooks.sh

validator-install:
	@./scripts/install_clap_validator.sh

# The bus gateway and the chain renderer as the skills and .mcp.json run them:
# built ALONE (feature unification in a workspace build hides a synth-core
# that only compiles with `gui` on — it did, 2026-10-21), the gateway then
# cargo-installed because .mcp.json starts ~/.cargo/bin/sdsp-bus.
tools:
	@cargo build --release -p sdsp-bus
	@cargo build --release -p sdsp-chain
	@cargo install --path tools/sdsp-bus --force

test:
	@cargo test --release --workspace

# Lib + dsp_smoke only — skips the slow clack-host e2e binaries
# (click_audit, mod_matrix_audit, quality_audit, clap_e2e). Pass
# anything to `cargo test`'s `--skip` runner filter. Doesn't actually
# save much on a warm cargo cache; mostly useful during DSP fiddling
# when you don't want to wait for plugin instantiation 5+ times.
test-fast:
	@cargo test --release --workspace --lib --tests -- \
		--skip click_audit --skip mod_matrix_audit \
		--skip quality_audit --skip clap_e2e --skip cc_no_feedback

release:
	@if [ -z "$(VERSION)" ]; then \
		echo "usage: make release VERSION=0.11.0"; exit 1; \
	fi
	@./scripts/build_release.sh $(VERSION)

clean:
	@echo "==> cargo clean"
	@cargo clean
	@echo "==> removing CMake build dir"
	@rm -rf build-wrappers
