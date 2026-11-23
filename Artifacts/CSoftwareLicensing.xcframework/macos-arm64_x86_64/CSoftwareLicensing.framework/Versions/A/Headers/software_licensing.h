#ifndef SOFTWARE_LICENSING_H
#define SOFTWARE_LICENSING_H
#define SOFTWARE_LICENSING_ABI_VERSION 6U
#include <stddef.h>
#include <stdint.h>
#ifdef __cplusplus
extern "C" {
#endif

typedef struct SoftwareLicensingSession SoftwareLicensingSession;
typedef struct SoftwareLicensingJson SoftwareLicensingJson;
typedef struct SoftwareLicensingJsonView {
    uint32_t code;
    const uint8_t *data;
    size_t length;
} SoftwareLicensingJsonView;

/* Record version 2 reads and upgrades version 1. */
uint32_t software_licensing_abi_version(void);
SoftwareLicensingSession *software_licensing_session_create(void);
/* Returns 1 when a completed effect is waiting for a valid timestamp, else 0. */
uint32_t software_licensing_session_has_completion(const SoftwareLicensingSession *);
/* Live session; bounded readable UTF-8 input; owns returned JSON. Never logs input.
 * Effect IDs are opaque. Save the bytes from software_licensing_json_persistence_record
 * once for each persist effect and acknowledge its exact ID once with persisted
 * or completeEffect. completeEffect takes id, now and a
 * completion object with kind provider (result) or persisted (success). A null now
 * retains the outcome; resumeCompletion takes now and cancelled and applies it once.
 * Retry only after failure; every retry has a fresh ID.
 * adoptExternallyVerifiedActivation requires host-verified authority, product scope
 * and current entitlement. It performs value checks and persistence, no provider check. */
SoftwareLicensingJson *software_licensing_session_dispatch(const SoftwareLicensingSession *,
                                                           const uint8_t *, size_t);
/* Decode present storage bytes into a fresh session. Dispatch restore without
 * an inline record to use the staged value. Skip staging for absent storage.
 * The JSON size limit applies to the normalized record.
 * The byte range is borrowed for the call. The returned JSON is owned. */
SoftwareLicensingJson *software_licensing_session_stage_record(const SoftwareLicensingSession *,
                                                               const uint8_t *, size_t);
void software_licensing_session_destroy(SoftwareLicensingSession *);
SoftwareLicensingJson *software_licensing_value(const uint8_t *, size_t);
SoftwareLicensingJson *software_licensing_access_reply(const uint8_t *, size_t);
/* For save, the record is borrowed UTF-8 JSON and its exact bytes are encrypted.
 * Pass NULL and zero for the record when loading or deleting. */
SoftwareLicensingJson *software_licensing_windows_storage(const uint8_t *request, size_t length,
                                                          const uint8_t *record,
                                                          size_t record_length);
/* Borrowed bytes expire when the owner is destroyed. code 0: value, 1: static failure. */
SoftwareLicensingJsonView software_licensing_json_view(const SoftwareLicensingJson *);
/* Borrowed UTF-8 storage record for this reply's persist effect. Copy before
 * destroying the owner. With no record or a null owner, returns NULL and writes
 * zero to length. Pass NULL for length when no count is needed. */
const uint8_t *software_licensing_json_persistence_record(const SoftwareLicensingJson *,
                                                          size_t *length);
void software_licensing_json_destroy(SoftwareLicensingJson *);

#ifdef __cplusplus
}
#endif
#endif
