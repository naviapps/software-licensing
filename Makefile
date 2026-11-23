SWIFT_FLAGS := -Xswiftc -strict-concurrency=complete -Xswiftc -warnings-as-errors
DOTNET_BIN ?= dotnet
FRAMEWORK_DIR := $(CURDIR)/Artifacts/CSoftwareLicensing.xcframework/macos-arm64_x86_64
NATIVE_LIBRARY := $(FRAMEWORK_DIR)/CSoftwareLicensing.framework/CSoftwareLicensing

.DEFAULT_GOAL := check
.PHONY: check check-distribution native native-verify lint test test-rust test-tools test-dotnet test-c test-consumer test-installed-consumers format

check: check-distribution test test-consumer

check-distribution: lint test-tools native-verify test-rust test-dotnet test-c

native:
	python3 tools/native.py

native-verify:
	python3 tools/native.py --verify

lint:
	mise exec -- python3 -B tools/quality.py check --dotnet
	cargo clippy --workspace --all-targets --locked --offline -- -D warnings

format:
	mise exec -- python3 -B tools/quality.py format --dotnet

test:
	swift test $(SWIFT_FLAGS)

test-consumer:
	python3 -B tools/consumer.py

test-installed-consumers:
	python3 -B tools/install-consumers.py

test-tools:
	python3 -B -m unittest discover -s tools/tests -v

test-rust:
	cargo test --workspace --locked --offline

test-dotnet: native-verify
	SOFTWARE_LICENSING_NATIVE_LIBRARY="$(NATIVE_LIBRARY)" DOTNET_BIN="$(DOTNET_BIN)" python3 -B tools/test-dotnet.py

test-c: native-verify
	mkdir -p .native-build
	xcrun clang -std=c17 -Wall -Wextra -Werror -fsanitize=address -I crates/ffi/include crates/ffi/tests/c/abi.c -F "$(FRAMEWORK_DIR)" -framework CSoftwareLicensing -Wl,-rpath,"$(FRAMEWORK_DIR)" -o .native-build/abi-test
	.native-build/abi-test
