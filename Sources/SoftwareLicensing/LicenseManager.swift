import Foundation

/// Drives Rust-issued effects on the application actor. Create one per storage namespace.
/// Already-cancelled async calls leave recovery untouched. Accepted activation and
/// release outcomes still become durable when cancellation arrives during their provider call.
@MainActor
public final class LicenseManager {
  private var session: Session
  private let makeSession: @MainActor () throws -> Session
  private let storage: any LicenseStorage
  private let clock: @Sendable () -> Date
  private let policy: LicenseRefreshPolicy
  private var restorationBlocked = false
  private var restoring = false
  private let unavailableState: LicenseState
  private var observers: [UUID: AsyncStream<LicenseState>.Continuation] = [:]
  private var lastPublishedState: LicenseState?
  private var deferredCompletion: Completion?

  public private(set) var initialRestoreError: LicenseError?

  private enum Session {
    case standard(LicenseSession, any LicenseProvider)
    case nativeProvider(any LicenseSessionDriver)

    @MainActor func dispatch(_ command: [String: Any]) throws -> Data {
      switch self {
      case .standard(let session, _): try session.dispatch(command)
      case .nativeProvider(let driver): try driver.dispatch(LicensingNative.encode(command))
      }
    }

    @MainActor func stageStoredRecord(_ record: Data) throws {
      switch self {
      case .standard(let session, _): try session.stageStoredRecord(record)
      case .nativeProvider(let driver): try driver.stageStoredRecord(record)
      }
    }
  }

  private enum Completion {
    case command([String: Any])
    case nativeProvider(id: UInt64, cancelled: Bool)
  }

  private struct Reply: Decodable {
    let state: LicenseState
    let outcome: LicenseRefreshOutcome?
    let error: LicenseError?
    let needsRefresh: Bool
  }

  private struct Response {
    let data: Data
    let decoded: Result<Reply, any Error>

    init(_ data: Data) {
      self.data = data
      self.decoded = Result { try LicensingNative.decoder().decode(Reply.self, from: data) }
    }
  }

  /// Throws for an incompatible native ABI before reading storage.
  /// Persisted-record failures remain available through `initialRestoreError`.
  public convenience init(
    provider: any LicenseProvider, storage: any LicenseStorage,
    refreshPolicy: LicenseRefreshPolicy? = nil,
    restorePersistedActivation: Bool = true,
    clock: @escaping @Sendable () -> Date = { Date() }
  ) throws {
    try self.init(
      makeSession: { .standard(LicenseSession(), provider) }, storage: storage,
      refreshPolicy: refreshPolicy, restorePersistedActivation: restorePersistedActivation,
      clock: clock)
  }

  /// Drive a host-owned native session, leaving storage and observation to this manager.
  /// The factory must return a fresh session on each call. Reload replaces the
  /// current session only after the stored record is readable and accepted.
  public convenience init(
    sessionFactory: @escaping @MainActor () throws -> any LicenseSessionDriver,
    storage: any LicenseStorage, refreshPolicy: LicenseRefreshPolicy? = nil,
    restorePersistedActivation: Bool = true,
    clock: @escaping @Sendable () -> Date = { Date() }
  ) throws {
    try self.init(
      makeSession: { .nativeProvider(try sessionFactory()) }, storage: storage,
      refreshPolicy: refreshPolicy, restorePersistedActivation: restorePersistedActivation,
      clock: clock)
  }

  private init(
    makeSession: @escaping @MainActor () throws -> Session, storage: any LicenseStorage,
    refreshPolicy: LicenseRefreshPolicy?, restorePersistedActivation: Bool,
    clock: @escaping @Sendable () -> Date
  ) throws {
    try LicensingNative.checkABI()
    let firstSession = try makeSession()
    self.session = firstSession
    self.makeSession = makeSession
    self.unavailableState = LicenseManager.emptyState()
    self.storage = storage
    self.policy = refreshPolicy ?? .default
    self.clock = clock
    do {
      try restore(readStorage: restorePersistedActivation, initialSession: firstSession)
    } catch {
      initialRestoreError = (error as? LicenseError) ?? LicenseError(.storageFailure)
      restorationBlocked = true
      session = try makeSession()
      _ = try read(
        session.dispatch([
          "command": "restore", "policy": try LicensingNative.object(LicenseRefreshPolicy.never),
          "now": 0,
        ]))
    }
  }

  public var state: LicenseState {
    if let observed = try? read(send(["command": "snapshot"])).state { return observed }
    publish(unavailableState)
    return unavailableState
  }
  public var plan: LicensePlan { state.plan }

  /// Latest snapshots, buffered once per consumer. Cancel iteration to unsubscribe.
  /// Re-evaluate `state` at `nextTransitionAt` to observe a local deadline without I/O.
  public func states() -> AsyncStream<LicenseState> {
    let id = UUID()
    let initial = state
    return AsyncStream(bufferingPolicy: .bufferingNewest(1)) { continuation in
      observers[id] = continuation
      continuation.yield(initial)
      continuation.onTermination = { [weak self] _ in
        Task { @MainActor in self?.observers.removeValue(forKey: id) }
      }
    }
  }

  deinit { for observer in observers.values { observer.finish() } }

  /// Evaluates refresh eligibility. An explicit time bypasses the manager's clock
  /// and is observed by the session for rollback protection. No provider or storage
  /// call runs here. Unsupported times conservatively return `true`.
  public func needsRefresh(now: Date? = nil) -> Bool {
    if restorationBlocked { return true }
    return
      (try? read(send(["command": "needsRefresh", "now": (now ?? clock()).timeIntervalSince1970]))
      .needsRefresh) ?? true
  }

  /// Retry a locked/corrupt/unsupported restore without overwriting the stored bytes.
  public func reload() throws {
    try restore(readStorage: true, requireIdle: true)
    restorationBlocked = false
    initialRestoreError = nil
  }

  @discardableResult
  public func activate(_ request: LicenseActivationRequest) async throws -> LicenseState {
    try await recoverPersistence()
    var command: [String: Any] = ["command": "activate"]
    if case .licenseKey(let key) = request { command["licenseKey"] = key }
    return try await drive(send(command)).state
  }

  /// Adopts and persists an activation whose authority, product scope and current
  /// entitlement the host has already verified. No provider check runs here.
  /// A well-formed activation alone is not evidence of entitlement.
  /// Resolving a pending activation also requires its key to match the saved request.
  @discardableResult
  public func adoptExternallyVerifiedActivation(_ activation: LicenseActivation) throws
    -> LicenseState
  {
    guard !restorationBlocked else { throw LicenseError(.storageFailure) }
    let data = try send([
      "command": "adoptExternallyVerifiedActivation",
      "activation": LicensingNative.object(activation),
    ])
    return try read(persist(data)).state
  }

  /// Clears a pending activation only after the host has authoritative external
  /// proof that the request did not apply. This method only drives persistence;
  /// a timeout, cancellation or missing local activation does not establish that proof.
  @discardableResult
  public func confirmActivationNotAppliedAfterExternalVerification() throws -> LicenseState {
    guard !restorationBlocked else { throw LicenseError(.storageFailure) }
    return try read(
      persist(
        send([
          "command": "confirmActivationNotAppliedAfterExternalVerification"
        ]))
    ).state
  }

  /// Clears a pending release only after the host has authoritative external
  /// proof that the saved remote instance was released. This method only drives
  /// persistence; it does not contact the provider or verify the host's evidence.
  @discardableResult
  public func confirmDeactivationAfterExternalVerification() throws -> LicenseState {
    guard !restorationBlocked else { throw LicenseError(.storageFailure) }
    return try read(
      persist(
        send([
          "command": "confirmDeactivationAfterExternalVerification"
        ]))
    ).state
  }

  @discardableResult
  public func refresh(onlyIfDue: Bool = false) async throws -> LicenseRefreshResult {
    try await recoverPersistence()
    let result = try await drive(send(["command": "refresh", "onlyIfDue": onlyIfDue]))
    guard let outcome = result.outcome else { throw LicenseError(.unexpectedProviderResponse) }
    return LicenseRefreshResult(outcome: outcome, state: result.state)
  }

  @discardableResult
  public func deactivate() async throws -> LicenseState {
    try await recoverPersistence()
    return try await drive(send(["command": "deactivate"])).state
  }

  /// Retries a pending write and resumes its planned provider effect once.
  /// A completed provider activation is never repeated to retry its final save.
  public func retryPersistence() async throws {
    try Task.checkCancellation()
    if restorationBlocked && !state.hasPendingPersistence { try reload() }
    let reply = try resumeCompletion() ?? send(["command": "retryPersistence"])
    _ = try await drive(reply)
    restorationBlocked = false
    initialRestoreError = nil
  }

  private var now: TimeInterval { clock().timeIntervalSince1970 }

  private static func emptyState() -> LicenseState {
    let session = LicenseSession()
    let reply = try! session.dispatch([
      "command": "restore", "policy": try! LicensingNative.object(LicenseRefreshPolicy.never),
      "now": 0,
    ])
    return try! LicensingNative.decoder().decode(Reply.self, from: reply).state
  }

  private func send(_ command: [String: Any]) throws -> Response {
    let kind = command["command"] as? String
    // Reads keep observing the committed session; only mutations need exclusion.
    guard !restoring || kind == "snapshot" || kind == "needsRefresh" else {
      throw LicenseError(.operationInProgress)
    }
    var command = command
    if command["now"] == nil { command["now"] = now }
    return observe(try session.dispatch(command))
  }

  private func observe(_ data: Data) -> Response {
    let response = Response(data)
    if case .success(let reply) = response.decoded {
      publish(reply.state)
    }
    return response
  }

  private func publish(_ state: LicenseState) {
    guard state != lastPublishedState else { return }
    lastPublishedState = state
    for observer in observers.values { observer.yield(state) }
  }

  private func read(_ data: Data) throws -> Reply {
    try read(Response(data))
  }

  private func read(_ response: Response) throws -> Reply {
    let reply: Reply
    do { reply = try response.decoded.get() } catch {
      throw LicenseError(.unexpectedProviderResponse)
    }
    if let error = reply.error {
      if error.code == .cancelled { throw CancellationError() }
      throw error
    }
    return reply
  }

  private func effect(_ data: Data) throws -> [String: Any]? {
    (try JSONSerialization.jsonObject(with: data) as? [String: Any])?["effect"] as? [String: Any]
  }

  private func restore(
    readStorage: Bool, requireIdle: Bool = false, initialSession: Session? = nil
  ) throws {
    guard !restoring && (!requireIdle || deferredCompletion == nil) else {
      throw LicenseError(.operationInProgress)
    }
    // Own the entire replacement, including reentrant clock and storage callbacks.
    restoring = true
    defer { restoring = false }
    if requireIdle {
      // Access may fall back to unlicensed, but coordination requires valid state.
      let current = try read(session.dispatch(["command": "checkReload", "now": now])).state
      publish(current)
    }
    let candidate = try initialSession ?? makeSession()
    if readStorage {
      let record: Data?
      do { record = try storage.load() } catch { throw LicenseError(.storageFailure) }
      if let record { try candidate.stageStoredRecord(record) }
    }
    let instant = now
    if requireIdle {
      _ = try read(session.dispatch(["command": "checkReload", "now": instant]))
    }
    // Rust restore only reads/upgrades in memory; it cannot issue native effects.
    let reply = try read(
      candidate.dispatch([
        "command": "restore", "policy": try LicensingNative.object(policy), "now": instant,
      ]))
    session = candidate
    publish(reply.state)
  }

  private func recoverPersistence() async throws {
    try Task.checkCancellation()
    if deferredCompletion != nil || state.hasPendingPersistence { try await retryPersistence() }
    if restorationBlocked { try reload() }
    try Task.checkCancellation()
  }

  private func persist(_ original: Response) throws -> Response {
    var response = original
    while let effect = try effect(response.data), effect["kind"] as? String == "persist" {
      guard let id = effect["id"], let record = effect["record"] else {
        throw LicenseError(.unexpectedProviderResponse)
      }
      let success: Bool
      do {
        try storage.save(LicensingNative.encode(record))
        success = true
      } catch { success = false }
      response = try complete(.command(["command": "persisted", "id": id, "success": success]))
      if !success { throw LicenseError(.storageFailure) }
    }
    return response
  }

  private func complete(_ completion: Completion) throws -> Response {
    let instant = now
    do {
      // Rust validates the captured time without changing the outstanding effect.
      _ = try session.dispatch(["command": "snapshot", "now": instant])
    } catch {
      // The effect already ran. Retain its result, not another request to run it.
      deferredCompletion = completion
      throw error
    }
    deferredCompletion = nil
    switch completion {
    case .command(var command):
      command["now"] = instant
      do { return try send(command) } catch {
        if command["command"] as? String == "provider", let id = command["id"] {
          // Invalid custom-provider payloads must release their lease using
          // the validated timestamp.
          _ = try? send(["command": "cancel", "id": id, "now": instant])
        }
        throw error
      }
    case .nativeProvider(let id, let cancelled):
      guard case .nativeProvider(let driver) = session else {
        throw LicenseError(.unexpectedProviderResponse)
      }
      return observe(try driver.completeProviderEffect(id, now: instant, cancelled: cancelled))
    }
  }

  private func resumeCompletion() throws -> Response? {
    guard let completion = deferredCompletion else { return nil }
    let response = try complete(completion)
    if case .command(let command) = completion,
      command["command"] as? String == "persisted", command["success"] as? Bool == false
    {
      // Record the failed write first; this fresh recovery request may then retry it.
      let reply = try response.decoded.get()
      if reply.error?.code == .storageFailure {
        try Task.checkCancellation()
        return try send(["command": "retryPersistence"])
      }
    }
    return response
  }

  private func drive(_ original: Response) async throws -> Reply {
    var response = original
    while true {
      response = try persist(response)
      let reply = try read(response)
      guard let effect = try effect(response.data) else { return reply }
      guard effect["kind"] as? String == "provider", let id = effect["id"] as? UInt64 else {
        throw LicenseError(.unexpectedProviderResponse)
      }
      switch session {
      case .nativeProvider(let driver):
        try await driver.executeProviderEffect(id)
        response = try complete(.nativeProvider(id: id, cancelled: Task.isCancelled))
      case .standard(_, let provider):
        guard let operation = effect["operation"] as? String else {
          throw LicenseError(.unexpectedProviderResponse)
        }
        let result = try await callProvider(provider, operation: operation, effect: effect)
        response = try complete(.command(["command": "provider", "id": id, "result": result]))
      }
    }
  }

  private func callProvider(
    _ provider: any LicenseProvider, operation: String, effect: [String: Any]
  ) async throws -> [String: Any] {
    var providerEntered = false
    do {
      try Task.checkCancellation()
      switch operation {
      case "activate":
        let request: LicenseActivationRequest =
          if let key = effect["licenseKey"] as? String { .licenseKey(key) } else { .automatic }
        providerEntered = true
        let activation = try await provider.activate(request)
        // Once a provider creates an activation, persist its returned identity even
        // if cancellation arrived meanwhile. Never create another seat to retry saving.
        return ["kind": "activated", "activation": try LicensingNative.object(activation)]
      case "validate":
        let activation = try decodeActivation(effect)
        providerEntered = true
        let validation = try await provider.validate(activation)
        try Task.checkCancellation()
        return ["kind": "validated", "validation": try LicensingNative.object(validation)]
      case "deactivate":
        let activation = try decodeActivation(effect)
        providerEntered = true
        try await provider.deactivate(activation)
        return ["kind": "deactivated"]
      default: throw LicenseError(.unexpectedProviderResponse)
      }
    } catch let error
      where error is CancellationError
      || (operation == "validate" && Task.isCancelled)
    {
      return [
        "kind": "failed",
        "failure": try LicensingNative.object(
          LicenseError(
            .cancelled, mutationOutcome: providerEntered ? nil : .notApplied)),
      ]
    } catch {
      let failure = (error as? LicenseError) ?? LicenseError(.transportFailure)
      return ["kind": "failed", "failure": try LicensingNative.object(failure)]
    }
  }

  private func decodeActivation(_ effect: [String: Any]) throws -> LicenseActivation {
    guard let value = effect["activation"] else { throw LicenseError(.unexpectedProviderResponse) }
    return try LicensingNative.decoder().decode(
      LicenseActivation.self, from: LicensingNative.encode(value))
  }
}
