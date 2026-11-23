SDK_TOOLING ?= mise run sdk-tooling --
SWIFT_FLAGS := -Xswiftc -strict-concurrency=complete -Xswiftc -warnings-as-errors
DOTNET_BIN ?= dotnet
FRAMEWORK_DIR := $(CURDIR)/Artifacts/CSoftwareLicensing.xcframework/macos-arm64_x86_64
NATIVE_LIBRARY := $(FRAMEWORK_DIR)/CSoftwareLicensing.framework/CSoftwareLicensing

.DEFAULT_GOAL := check
.PHONY: check check-source test-managed check-distribution native native-verify lint test test-rust test-distribution-tools test-dotnet test-c test-consumer test-installed-consumers format

check: check-source test test-consumer

check-source: lint test-rust test-managed

check-distribution: test-distribution-tools native-verify test-dotnet test-c

native:
	$(SDK_TOOLING) native

native-verify:
	$(SDK_TOOLING) native --verify

lint:
	$(SDK_TOOLING) quality check --dotnet
	cargo clippy --workspace --all-targets --locked --offline -- -D warnings

format:
	$(SDK_TOOLING) quality format --dotnet

test:
	swift test $(SWIFT_FLAGS)

test-consumer:
	$(SDK_TOOLING) swift-consumer

test-installed-consumers:
	$(SDK_TOOLING) installed-consumers

test-distribution-tools:
	$(SDK_TOOLING) python -m unittest discover -s distribution/tests -v

test-rust:
	cargo test --workspace --locked --offline

test-dotnet: native-verify
	SOFTWARE_LICENSING_NATIVE_LIBRARY="$(NATIVE_LIBRARY)" DOTNET_BIN="$(DOTNET_BIN)" $(SDK_TOOLING) python distribution/verify_dotnet.py

test-c: native-verify
	mkdir -p .native-build
	xcrun clang -std=c17 -Wall -Wextra -Werror -fsanitize=address -I crates/ffi/include crates/ffi/tests/c/abi.c -F "$(FRAMEWORK_DIR)" -framework CSoftwareLicensing -Wl,-rpath,"$(FRAMEWORK_DIR)" -o .native-build/abi-test
	.native-build/abi-test

test-managed:
	dotnet run --configuration Release --project bindings/dotnet/tests/SoftwareLicensing.Tests/SoftwareLicensing.Tests.csproj -- --managed-only
