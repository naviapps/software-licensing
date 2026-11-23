import Foundation
import Security
import SoftwareLicensing

/// Atomically stores confidential SDK bytes in a host-selected Keychain item.
/// The host selects a stable service/account for its current confidential record.
public final class KeychainLicenseStorage: @unchecked Sendable, LicenseStorage {
  private let service: String
  private let account: String
  private let accessibility: CFString

  public init(
    service: String, account: String,
    accessibility: CFString = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
  ) throws {
    self.service = try LicensingNative.normalized("identifier", service, as: String.self)
    self.account = try LicensingNative.normalized("identifier", account, as: String.self)
    self.accessibility = accessibility
  }

  public func load() throws -> Data? {
    var query = query
    query[kSecReturnData as String] = true
    query[kSecMatchLimit as String] = kSecMatchLimitOne
    var item: CFTypeRef?
    let status = SecItemCopyMatching(query as CFDictionary, &item)
    if status == errSecItemNotFound { return nil }
    guard status == errSecSuccess, let data = item as? Data, data.count <= 1_048_576 else {
      throw LicenseError(.storageFailure)
    }
    return data
  }

  public func save(_ record: Data) throws {
    guard !record.isEmpty, record.count <= 1_048_576 else { throw LicenseError(.storageFailure) }
    let values: [String: Any] = [
      kSecValueData as String: record, kSecAttrAccessible as String: accessibility,
    ]
    var insert = query
    insert.merge(values) { _, new in new }
    let status = SecItemAdd(insert as CFDictionary, nil)
    if status == errSecDuplicateItem {
      guard SecItemUpdate(query as CFDictionary, values as CFDictionary) == errSecSuccess else {
        throw LicenseError(.storageFailure)
      }
    } else if status != errSecSuccess {
      throw LicenseError(.storageFailure)
    }
  }

  public func delete() throws {
    let status = SecItemDelete(query as CFDictionary)
    guard status == errSecSuccess || status == errSecItemNotFound else {
      throw LicenseError(.storageFailure)
    }
  }

  private var query: [String: Any] {
    [
      kSecClass as String: kSecClassGenericPassword,
      kSecAttrService as String: service, kSecAttrAccount as String: account,
    ]
  }
}
