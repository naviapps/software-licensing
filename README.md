# SoftwareLicensing

Reusable licensing SDK for native applications. Rust owns license values,
operation admission, validation decisions, offline grace, persistence transactions,
and callback result mapping. Swift and .NET execute native effects. Host apps
own product scope, commercial policy, UI and secure-storage namespaces.

Start with [SwiftPM installation](#swiftpm-installation),
[Rust and C installation](#rust-and-c-installation), or the
[.NET package guide](https://github.com/naviapps/software-licensing/blob/main/bindings/dotnet/README.md). The
[storage and recovery contract](#storage-and-recovery) applies to every binding.
Maintainers use [build and acceptance](#build-and-acceptance) and
[development tooling](#development-tooling).

## Layout

| Path                                             | Contents                                                  |
| ------------------------------------------------ | --------------------------------------------------------- |
| `crates/`                                        | Rust implementation and C API in `crates/ffi/include/`    |
| `crates/ffi/tests/c/`                            | C ABI tests                                               |
| `Sources/`, `Tests/`                             | Swift bindings and tests                                  |
| `bindings/dotnet/src/<project>/`                 | Named .NET source projects                                |
| `bindings/dotnet/tests/SoftwareLicensing.Tests/` | .NET test project and fixtures                            |
| `Artifacts/`, `ThirdParty/`, `tools/`            | Native packages, upstream notices and development tooling |

## Products

| Component              | Swift                                            | Rust                         | .NET                         |
| ---------------------- | ------------------------------------------------ | ---------------------------- | ---------------------------- |
| Lifecycle and recovery | `SoftwareLicensing`                              | `software-licensing`         | `NaviApps.SoftwareLicensing` |
| Native access callback | `CallbackLicenseProvider` in `SoftwareLicensing` | Core callback mapper         | `CallbackLicenseProvider`    |
| Secure storage         | `SoftwareLicensingKeychain`                      | `software-licensing-windows` | `WindowsLicenseStorage`      |

The base exposes only lifecycle, secure storage and host-owned callback behavior.
Its Rust/C runtime has no vendor codecs or HTTP dependencies. Vendor providers
live in independent packages and binaries; they implement the public
`LicenseProvider` / `ILicenseProvider` port using the public Codable / JSON values.
Native vendor frameworks remain configured by the host.
The SDK does not implement checkout, billing, merchant APIs or license issuance.

## Requirements

- Swift 6.3 and macOS 26 or later. The universal framework supports ARM64 and Intel.
- Rust 1.99.1. See `rust-toolchain.toml` and the locked registry dependencies.
- .NET 10. The Windows package supplies `win-x64` and is tested with x64 .NET
  under emulation on Windows 11 ARM64 (build 26100 or later).

`Package.swift` exposes `SoftwareLicensing` and `SoftwareLicensingKeychain`.
The [.NET guide](https://github.com/naviapps/software-licensing/blob/main/bindings/dotnet/README.md)
covers local installation of the verified `.nupkg` and native runtime requirements.

## SwiftPM installation

Select a published full 40-character Git commit revision. Replace the placeholder
below before resolving this consumer package:

```swift
// swift-tools-version: 6.3
import PackageDescription

let package = Package(
  name: "MyApp",
  platforms: [.macOS(.v26)],
  dependencies: [
    .package(
      url: "https://github.com/naviapps/software-licensing.git",
      revision: "<published-full-40-character-commit-revision>"),
  ],
  targets: [
    .executableTarget(
      name: "MyApp",
      dependencies: [
        .product(name: "SoftwareLicensing", package: "software-licensing"),
        .product(name: "SoftwareLicensingKeychain", package: "software-licensing"),
      ]),
  ]
)
```

For a first run, save the [standalone Swift example][swift-consumer] as
`Sources/MyApp/Consumer.swift` in your package, then run `swift run MyApp` from
the package directory. The example exercises the public lifecycle with
in-memory storage and synthetic access checks. It constructs the Keychain adapter
without reading or writing a Keychain item. This is the same client exercised
by the SDK's external consumer check.

SwiftPM uses the checked-in native framework; ordinary Swift consumers do not
run the SDK's Rust, .NET or Python development tools.

[swift-consumer]: https://github.com/naviapps/software-licensing/blob/main/fixtures/swift-consumer/Sources/CoreLifecycleConsumer/Consumer.swift

In an app, supply its own source, plan, Keychain service and account, and a
read-only access check:

```swift
import SoftwareLicensing
import SoftwareLicensingKeychain

@MainActor
func makeLicenseManager(
  source: LicenseSource, planIdentifier: String, service: String, account: String,
  checkAccess: @escaping @Sendable () async throws -> Bool
) throws -> LicenseManager {
  let configuration = try CallbackLicenseProvider.Configuration(
    source: source, planIdentifier: planIdentifier)
  let provider = CallbackLicenseProvider(configuration: configuration, checkAccess: checkAccess)
  let storage = try KeychainLicenseStorage(service: service, account: account)
  return try LicenseManager(provider: provider, storage: storage)
}
```

The manager restores the existing record from the selected Keychain item. Check
`initialRestoreError` before using that record. Call `try await manager.activate(.automatic)`
from the app's activation action. To retain an existing callback activation, pass
its original source and plan values unchanged; no vendor identity is built into
the facade. Callback activation has no license key or remote instance. Key-based
providers are supplied separately through the same generic port.

`checkAccess` must only read host access, without creating or releasing remote instances.
`CallbackLicenseProvider` failures are `LicenseError` values with
`mutationOutcome: .notApplied` because this adapter only reads host access.
Typed error codes, status codes and retry delays are preserved; untyped failures
become `transportFailure` and cancellation becomes `cancelled`. The manager still
throws `CancellationError` for cancellation and durably clears failed callback
activation intents before returning the failure. A later uncancelled activation
can retry, including after restart.

Keep the manager on the application actor. Its `state` is a time-evaluated,
credential-free snapshot. `needsRefresh()` is a scheduling decision; `refresh()`
explicitly validates even when the normal interval has not elapsed. Host apps
should request `refresh(onlyIfDue: true)` / `RefreshAsync(onlyIfDue: true)` at
startup/resume and use an explicit refresh when users request it.
Snapshot equality uses the exact spelling of normalized provider source and plan
identifiers, so a change to either spelling remains visible to observers.
Use `plan` / `Plan` to read the current snapshot's plan. Use
`needsRefresh(now:)` / `NeedsRefresh(now)` to supply an observed wall time
without calling the manager clock, provider or storage. The session records
that time for rollback protection. An unsupported explicit time remains due.
When the manager clock is unavailable, snapshot reads and new observers see an
unlicensed state, and refresh eligibility reports due. Existing observers receive
the unlicensed snapshot when a snapshot read encounters the clock failure. The
next valid snapshot read restores the time-evaluated state without provider or
storage calls.
`nextRefreshAt` is the earliest automatic validation time; `nextTransitionAt` is
a local access deadline. These have different purposes. Retry timing is calculated
in Rust and persisted, including server `retryAfterSeconds` advice. The default
transient-failure retry interval is 60 seconds; the host selects policy and owns timers.

`refresh()` / `RefreshAsync()` returns `LicenseRefreshResult` with a typed
`LicenseRefreshOutcome`. Rust and wire values use the following closed vocabulary;
Swift uses the wire spelling as its case name and .NET uses PascalCase members.

| Wire / Swift case               | Meaning                                                                              |
| ------------------------------- | ------------------------------------------------------------------------------------ |
| `validated`                     | The provider accepted validation and committed access is active.                     |
| `gracePeriod`                   | The check failed and committed access has bounded offline grace.                     |
| `expired`                       | Committed access is expired.                                                         |
| `invalid`                       | Committed access is invalid, including rejection or failed validation without grace. |
| `deactivated`                   | Refresh completed a pending release and durably cleared its activation.              |
| `skippedActivationInProgress`   | Activation already owns the provider operation.                                      |
| `skippedRefreshInProgress`      | Validation already owns the provider operation.                                      |
| `skippedDeactivationInProgress` | Release is already in flight.                                                        |
| `skippedNoActivation`           | There is no saved activation to validate.                                            |
| `skippedRefreshDisabled`        | Policy disables refresh of the saved activation.                                     |
| `skippedNotDue`                 | An automatic refresh was requested before its eligible time.                         |

Validation outcomes are evaluated at persistence completion, so a deadline reached
while saving produces `expired` or `invalid`. Inspect `state.lastRefreshFailure`
(`State.LastRefreshFailure` in .NET) for validation failure details. Cancellation,
storage failures and failures to resume a release throw; they do not fabricate a
successful result. Unknown wire outcomes, or a missing final refresh outcome, are
errors. Swift consumers switching over this public enum should handle future cases
with `@unknown default`; do not infer access from an unknown outcome.

Swift `states()` and .NET `WatchStatesAsync()` publish the latest snapshot, including
in-flight operations and local persistence failures. Each consumer buffers only one
snapshot; cancel iteration to unsubscribe. Streams never expose credentials or
trigger background network requests. Schedule a local re-read at `nextTransitionAt`
(`NextTransitionAt` in .NET) to reflect expiration or the end of offline grace even
when the app remains open. Re-evaluate on resume as well. The host owns scheduling;
Rust owns the transition time and resulting access state.

## Externally verified activation

`adoptExternallyVerifiedActivation(_:)` / `AdoptExternallyVerifiedActivation(...)`
imports a value whose provider authority, product scope and current entitlement
have already been verified by the host. The Rust/C command is
`adoptExternallyVerifiedActivation`. It checks value shape and expiration and
persists the candidate; it does not contact a provider, verify a receipt or prove
entitlement. Successful persistence can grant access, so never pass untrusted
input or an old cached grant as fresh verification. Use normal activation or
validation when the SDK's configured provider should establish entitlement.
An existing remote credential can only be renewed under the same source/key/instance
identity; importing a different identity requires releasing it first. Pending
activation recovery must match the saved request key. A provider returning another
key cannot resolve that request or grant access.

## Storage and recovery

One manager exclusively coordinates each namespace. Activation and validation
state are stored together in one versioned confidential record. Keychain updates
replace the existing item atomically. Windows uses current-user DPAPI, namespace
entropy and atomic encrypted-file replacement in a host-selected private directory.
No machine-wide encryption or security-setting changes are used. Windows temporary
files become cleanup-owned only after exclusive creation succeeds; collisions
preserve existing bytes and try up to 32 new candidates. Failed writes or replacement
clean up only the owned temporary file and preserve the previous target.

Each Windows storage directory owns one `license.dpapi` record. The `namespace`
supplies DPAPI entropy only; it does not select a file or partition the directory.
Use a distinct private absolute directory for each logical record/manager namespace,
and keep both directory and namespace stable. For example, two records can use
separate `.../app-A/license` and `.../app-B/license` directories with their own
stable namespace strings.

The host selects a stable namespace for its current record. Persisted record
version **2** accepts version **1** records and upgrades them in memory without
I/O. The next successful transaction writes version 2. Original credentials,
instance identifiers and source values are retained. Unknown versions and corrupt
records block mutation and leave the stored bytes untouched. `reload()` retries
reading those bytes; it never replaces them with an empty grant.
An invalid startup clock also blocks mutation. After the clock recovers, reload
or retry persistence to restore the saved record.
State and refresh-eligibility reads continue observing the committed session while
reload loads storage. Other mutations remain excluded until the replacement is ready.

Rust/C ABI is **4**, independently of the storage schema. An ABI mismatch reports
`incompatibleABIVersion` / `IncompatibleAbiVersion` before reading storage.
Ship matching Rust/C, Swift/.NET bindings and native artifacts together.
`unsupportedVersion` denotes a persisted-record version error only; existing
records containing that historical failure remain readable.

Custom `LicenseStorage` / `ILicenseStorage` implementations provide only atomic
load and save operations. Return the stored bytes directly. Only an absent
value denotes an empty store. Rust decodes present bytes before the host samples
the restore time. It rejects a present empty value, malformed JSON, JSON null
and other scalar values as storage failures. Both bindings read UTF-8
(including a byte-order mark), UTF-16 and UTF-32 JSON from custom stores and
write UTF-8. The .NET binding now accepts the additional encodings supported
by Swift. The 1 MiB custom-storage limit applies to the compact UTF-8 record,
so formatting whitespace does not count. Keychain and DPAPI storage apply
their existing 1 MiB limit to plaintext record bytes.
Deactivation saves its recovery state. `KeychainLicenseStorage.delete()` and
`WindowsLicenseStorage.Delete()` remain explicit host operations on their concrete
storage types; `LicenseManager` never invokes them.

Activation saves a confidential intent before invoking its provider. The saved
intent contains the request, never a grant. If the process exits before confirming
the response, or a mutation fails with `mutationOutcome: unknown`,
`hasPendingActivation` blocks another activation, refresh or deactivation.
`lastOperationFailure` contains only stable failure metadata, never credentials.
A request known not to have applied durably clears its intent before returning
its failure. Cancellation before provider entry is known not applied; cancellation
after entry alone cannot establish that fact. Providers should preserve that distinction.

Unknown mutation failures throw `mutationOutcomeUnknown`. They must not be treated
as ordinary retryable activation failures. A host with fresh authoritative external
verification may resolve a matching issued instance through
`adoptExternallyVerifiedActivation`, or confirm a request did not apply through
`confirmActivationNotAppliedAfterExternalVerification`. The matching deactivation
confirmation API completes a release only after the host has external proof.
Neither confirmation API verifies authority itself. A generic 404 or an old cached
value is not that proof. If the provider cannot recover an unknown instance, the
host must offer support rather than silently create another remote seat.

Failed writes retain a pending transaction in memory. `retryPersistence()` saves
that candidate without making another activation request. If the write was the
first step of deactivation, it then resumes that already-planned release. Same-key activation
validates the saved instance; replacing a different key requires deactivation.
A retry while a write awaits completion fails with `operationInProgress`. Each
write attempt has its own opaque effect ID, including retries and the writes before
and after provider release. Rust/C effect drivers must save each emitted candidate
once, then acknowledge that exact ID once with `persisted`; only a failed attempt
can be retried. Late or repeated acknowledgements cannot commit a candidate or
advance a later operation. Swift and .NET managers handle these receipts internally.
If the final write after a successful release fails, retrying it in the same manager
does not call the provider again. After a restart, only the saved pending-release
marker remains, so recovery must ask the provider to release that instance again.
A saved release with no recorded failure is eligible for automatic refresh
immediately, regardless of the previous validation schedule. If a release
failure set a retry deadline, that deadline still applies after restart.
While a save is pending or has failed, validation rejection, shorter expiration
and bounded offline grace restrict access immediately. New activation grants,
plan changes and renewed deadlines require a successful save. The committed
record and pending candidate remain available for recovery; after restart only
the saved record can be restored.
A remotely created activation cannot be made durable if storage never succeeds
before the process exits. Surface the storage failure and retry it before exiting.

If a custom clock becomes invalid after a native save or provider call completes,
the binding retains that completed result in memory until Rust can accept it at a
valid time. Access is never granted using an invalid time. After the clock recovers,
`retryPersistence()` / `RetryPersistenceAsync()` acknowledges the saved result and
continues recovery without repeating a completed provider call or successful save.
A failed save becomes retryable after its failure is acknowledged. Cancellation
during that acknowledgement leaves the next write for a later request. Normal async
operations resume this recovery before starting their new work. In-memory completions
last for the process lifetime. The confidential journal governs recovery after restart.
Reloading requires one valid idle snapshot; a failed clock read never permits
replacement of an active session or a pending write. Reload owns its clock and
storage callbacks through replacement, rejecting reentrant mutations and reloads.
Read-only queries and new state observers continue to see the committed session.
Failed reads or schema validation keep the previous session and stored bytes intact.
Rust checks reload admission before storage access and again at the replacement
timestamp. A clock older than the live session's observed time, even by less than
a second, returns `clockRollback` instead of reviving an already observed expiration.
Wait for time to catch up or complete an authoritative provider validation; reloading
stored bytes alone cannot reset the clock floor. C drivers use `checkReload` at both
boundaries when replacing a live session; initial restore has no prior live floor.
.NET disposal uses cached Rust operation flags rather than the host clock. Idle
resources can be released, and repeated disposal stays harmless, while time is
unavailable; outstanding effects still block disposal.

Activation, refresh, deactivation and persistence retry check cancellation before
starting recovery and before invoking a provider. An already-cancelled request
does not reload or save recovery storage or invoke provider callbacks. Once a
provider accepts activation or release, its outcome is still saved even if the
request became cancelled during that call. Validation only reads remote state; a
cancelled validation ignores both a late result and a late provider failure. If
cancellation follows a saved
pending-release marker but precedes the provider call, an uncancelled retry can
resume that release.

Deactivation first saves an inaccessible pending-release record, then calls the
provider. A transport failure or cancellation preserves the instance identifier;
`deactivate()` or `refresh()` can resume the release after restart. Only successful
release clears the credential. Expired/rejected credentials remain inaccessible
but available for renewal validation and release.

Offline grace is anchored to the last successful validation, bounded by license
expiration and any existing grace deadline. Repeated failures cannot extend it.
Definitive rejection, expiration or invalid configuration cannot grant grace.
A live session retains the largest wall time it has observed, so moving the clock
back cannot restore access after observed expiry. Successful authoritative
validation/activation can establish a fresh boundary after durable saving. Every
normal transaction also retains its observation boundary in the confidential
record. Reading a snapshot performs no storage writes, so observations since the
last successful write are not durable across process exit. This is not tamper-proof
offline licensing or a substitute for the provider's authoritative check.

## Provider boundary

The base orders provider effects and validates returned activation/validation values;
it does not perform HTTP, define vendor endpoints or interpret vendor response bodies.
A separate provider package owns transport, scope validation, retry policy and its own
native binary. Provider adapters must retain an existing app's source strings and
stored activation identity. No base native internals are exposed to provider modules.

## Rust and C installation

Add the core to an independent Cargo workspace using a published full Git revision:

```toml
[dependencies]
software-licensing = { git = "https://github.com/naviapps/software-licensing.git", rev = "<published-full-40-character-commit-revision>" }
```

Replace the placeholder with the commit you selected. Import `software_licensing`
and keep your own `Cargo.lock`. A Windows Rust host can also select
`software-licensing-windows` at the same URL and revision for DPAPI storage.
The core `Session` returns effects for the caller to execute; it does not
perform storage or provider I/O itself.

For C on macOS, copy the universal `CSoftwareLicensing.framework` from the
`macos-arm64_x86_64` slice under `Artifacts/CSoftwareLicensing.xcframework`
at that revision to an installation directory alongside the original
`ThirdParty/` notices. Include `<CSoftwareLicensing/software_licensing.h>`
and link the installed framework. With `sdk_install` set to the absolute
installation directory and your source in `main.c`:

```sh
xcrun clang -std=c17 main.c -F "$sdk_install" -framework CSoftwareLicensing \
  -Wl,-rpath,"$sdk_install" -o licensing-client
./licensing-client
```

This is the same framework and header used by the independent fixture under
`fixtures/c-consumer/`. Preserve the original notices when redistributing the
binary. The C client needs no Cargo workspace or SDK development tooling.

## Build and acceptance

`make check` includes `python3 -B tools/consumer.py`. This stages only installable
SDK inputs and builds and runs the independent `fixtures/swift-consumer` package
with strict concurrency and warnings as errors. The fixture uses the public core
and Keychain products, an injected fixed clock, callbacks and in-memory storage to
check activation, reopen, refresh, deactivation and rejected activation. It only constructs
Keychain storage and never reads or writes a Keychain item. Rust implementation,
build caches and developer tooling trees are unavailable to this external package. This local
source/artifact gate does not prove remote Git installation or publication.

The isolated .NET fixture uses the same lifecycle through an installed NuGet
package. The macOS check packages the checked-in framework for the local test
runtime. The Windows check packs the freshly built x64 DLL. Both fixtures load
the packaged library through the runtime without an SDK source or library override.

With mise-managed tools, run `mise exec -- make test-installed-consumers` from a
clean, complete SDK checkout. This keeps the selected Cargo and Rust on `PATH`
when the check moves into its external workspace. The Rust client uses a local
mirror of the exact SDK commit and accepts Git packages only from that revision.
The C client builds against only the copied framework, its public header and
notices. Both exercises use synthetic entitlement and a fixed clock to verify
that access waits for persistence, rejects a duplicate acknowledgement and
expires without provider or storage calls. The macOS distribution job runs this
gate alongside `make check-distribution`. This does not exercise Git authentication,
production licensing or Windows DPAPI.

From the SDK root, review `mise.toml` and select its pinned developer tools.
Xcode must supply Swift 6.3 and the macOS 26 SDK or later:

```sh
mise trust mise.toml
mise install
mise exec -- cargo fetch --locked
mise exec -- make check
```

The checked-in framework is ready for validation after a fresh clone. Rebuild it
with `mise exec -- make native` after changing native inputs, before running checks.

`tools/native.py` builds and verifies the universal macOS 26 framework, locked
dependencies, notices, exact C exports and deployment targets.
Its receipt binds production inputs and build-tool bytes to the artifact hashes.
`make check` verifies that existing framework, then runs Rust, Swift, C ownership,
.NET fixtures and portable build-driver regressions without rebuilding the framework.
`make test-tools` runs only the portable driver checks.
Set `DEVELOPER_DIR` when Xcode is not the globally selected developer directory.
The C ownership check uses AddressSanitizer. Rust tests cover the read-only access
decision matrix and its mutation evidence. Swift and .NET tests exercise the same
engine with synthetic callbacks, cancellation and storage recovery.

Use the [Windows package acceptance](bindings/dotnet/README.md#windows-package-acceptance)
procedure to build one x64 DLL and exercise it from Rust, C, .NET and an isolated
NuGet consumer on an owned Windows 11 ARM64 host. The .NET fixture checks DPAPI
with synthetic records in a temporary directory. The receipt identifies the
ARM64 host, x64 target, .NET SDK, emulated execution and verified package bytes.
Local checks, macOS app integration and exact-source Windows executions provide
separate evidence.

## Development tooling

Developer commands use the scripts included in this checkout. The
[maintainer setup](#build-and-acceptance) installs the tools for these commands.

GitHub Actions runs the same strict Swift tests and standalone Swift consumer
on macOS 26 with Xcode 26.6 and macOS 27 with Xcode 27. A single macOS 27
distribution job checks the tracked framework receipt and runs Rust, C and .NET
fixtures. The Windows x64 workflow verifies the installed NuGet package. These
checks use synthetic fixture providers.

## Static checks

`mise install` prepares pinned developer tools. Run `mise run lint` before builds
or `mise run format` to format owned source. Formatting excludes retained build
helpers, native artifacts and original notices. The check targets run compiler
and native acceptance separately.

## License

SoftwareLicensing is available under the [MIT License](LICENSE).
`ThirdParty/` retains Rust runtime provenance and upstream notices, including the
previous LicenseKit notice for the storage adapter lineage. The macOS framework
contains the MIT license and unmodified notices in
`Resources/ThirdPartyNotices.txt`. NuGet packages retain the original files.
