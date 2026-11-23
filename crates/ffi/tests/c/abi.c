#include "software_licensing.h"
#include <assert.h>
#include <errno.h>
#include <inttypes.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static void success(SoftwareLicensingJson *value) {
    SoftwareLicensingJsonView view = software_licensing_json_view(value);
    assert(view.code == 0 && view.data && view.length);
    size_t length = SIZE_MAX;
    assert(!software_licensing_json_persistence_record(value, &length) && length == 0);
    software_licensing_json_destroy(value);
}
static SoftwareLicensingJson *dispatch(SoftwareLicensingSession *session, const char *command) {
    return software_licensing_session_dispatch(session, (const uint8_t *)command, strlen(command));
}
static void copy_json(SoftwareLicensingJson *value, char *buffer, size_t size) {
    SoftwareLicensingJsonView view = software_licensing_json_view(value);
    assert(view.data && view.length < size);
    memcpy(buffer, view.data, view.length);
    buffer[view.length] = '\0';
}
static uint64_t effect_id(SoftwareLicensingJson *value) {
    char buffer[4096];
    copy_json(value, buffer, sizeof(buffer));
    const char *field = strstr(buffer, "\"id\":");
    assert(field);
    field += strlen("\"id\":");
    assert(*field >= '0' && *field <= '9');
    char *end = NULL;
    errno = 0;
    unsigned long long id = strtoull(field, &end, 10);
    assert(errno == 0 && end != field && (*end == ',' || *end == '}'));
    return (uint64_t)id;
}
static void rejected_receipt(SoftwareLicensingJson *value) {
    char buffer[4096];
    copy_json(value, buffer, sizeof(buffer));
    assert(software_licensing_json_view(value).code == 1);
    assert(strstr(buffer, "\"code\":\"operationInProgress\""));
    software_licensing_json_destroy(value);
}
int main(void) {
    assert(software_licensing_abi_version() == SOFTWARE_LICENSING_ABI_VERSION);
    assert(software_licensing_session_has_completion(NULL) == 0);
    size_t length = SIZE_MAX;
    assert(!software_licensing_json_persistence_record(NULL, &length) && length == 0);
    software_licensing_json_destroy(NULL);
    software_licensing_session_destroy(NULL);
    SoftwareLicensingJson *invalid = software_licensing_session_dispatch(NULL, NULL, 0);
    assert(software_licensing_json_view(invalid).code == 1);
    software_licensing_json_destroy(invalid);
    const char *policy = "{\"kind\":\"defaultPolicy\"}";
    success(software_licensing_value((const uint8_t *)policy, strlen(policy)));
    SoftwareLicensingSession *raw_session = software_licensing_session_create();
    const char *invalid_storage[] = {"", " \nnull\t", "{"};
    for (size_t i = 0; i < sizeof invalid_storage / sizeof invalid_storage[0]; ++i) {
        const char *record = invalid_storage[i];
        invalid = software_licensing_session_stage_record(raw_session, (const uint8_t *)record,
                                                          strlen(record));
        char raw_failure[256];
        copy_json(invalid, raw_failure, sizeof(raw_failure));
        assert(software_licensing_json_view(invalid).code == 1);
        assert(strstr(raw_failure, "\"code\":\"storageFailure\""));
        software_licensing_json_destroy(invalid);
    }
    software_licensing_session_destroy(raw_session);
    const char *callback = "{\"source\":\"host-membership\",\"planIdentifier\":\"paid\","
                           "\"operation\":\"activate\",\"hasAccess\":true,\"now\":1000}";
    SoftwareLicensingJson *mapped =
        software_licensing_access_reply((const uint8_t *)callback, strlen(callback));
    char callback_result[2048];
    copy_json(mapped, callback_result, sizeof(callback_result));
    assert(strstr(callback_result, "\"needsAccessCheck\":false"));
    assert(strstr(callback_result, "\"source\":\"host-membership\""));
    assert(strstr(callback_result, "\"planIdentifier\":\"paid\""));
    assert(!strstr(callback_result, "setapp"));
    success(mapped);
    const char *denied = "{\"source\":\"host-membership\",\"planIdentifier\":\"paid\","
                         "\"operation\":\"activate\",\"hasAccess\":false,\"now\":1000}";
    mapped = software_licensing_access_reply((const uint8_t *)denied, strlen(denied));
    copy_json(mapped, callback_result, sizeof(callback_result));
    assert(strstr(callback_result, "\"kind\":\"failed\""));
    assert(strstr(callback_result, "\"code\":\"invalidLicense\""));
    assert(strstr(callback_result, "\"mutationOutcome\":\"notApplied\""));
    success(mapped);
    SoftwareLicensingSession *session = software_licensing_session_create();
    const char *restore =
        "{\"command\":\"restore\",\"policy\":{\"isEnabled\":true,\"validationInterval\":86400,"
        "\"failureGracePeriod\":604800,\"serverFailureGracePeriod\":86400},\"now\":1000}";
    success(
        software_licensing_session_dispatch(session, (const uint8_t *)restore, strlen(restore)));
    const char *apply =
        "{\"command\":\"adoptExternallyVerifiedActivation\",\"activation\":{\"source\":\"fixture\","
        "\"planIdentifier\":\"paid\",\"activatedAt\":1000,\"licenseKey\":\"FIXTURE-"
        "KEY\"},\"now\":1000}";
    SoftwareLicensingJson *pending =
        software_licensing_session_dispatch(session, (const uint8_t *)apply, strlen(apply));
    SoftwareLicensingJsonView view = software_licensing_json_view(pending);
    assert(view.code == 0 && view.length > 0);
    char metadata[4096];
    copy_json(pending, metadata, sizeof(metadata));
    assert(!strstr(metadata, "\"record\":"));
    assert(!strstr(metadata, "FIXTURE-KEY"));
    const uint8_t *bytes = software_licensing_json_persistence_record(pending, &length);
    assert(bytes && length > 0);
    assert(bytes == software_licensing_json_persistence_record(pending, NULL));
    uint64_t receipt_id = effect_id(pending);
    /* A borrowed view remains valid across calls until its own owner is destroyed. */
    const char *snapshot = "{\"command\":\"snapshot\",\"now\":1000}";
    success(
        software_licensing_session_dispatch(session, (const uint8_t *)snapshot, strlen(snapshot)));
    assert(view.data[0] == '{');
    assert(bytes[0] == '{' && bytes[length - 1] == '}');
    software_licensing_json_destroy(pending);
    char receipt[256];
    snprintf(receipt, sizeof(receipt),
             "{\"command\":\"completeEffect\",\"id\":%" PRIu64
             ",\"completion\":{\"kind\":\"persisted\",\"success\":true},\"now\":null}",
             receipt_id);
    SoftwareLicensingJson *deferred = dispatch(session, receipt);
    assert(software_licensing_json_view(deferred).code == 1);
    char invalid_time[256];
    copy_json(deferred, invalid_time, sizeof(invalid_time));
    assert(strstr(invalid_time, "\"code\":\"invalidData\""));
    software_licensing_json_destroy(deferred);
    assert(software_licensing_session_has_completion(session) == 1);
    rejected_receipt(dispatch(session, receipt));
    success(
        dispatch(session, "{\"command\":\"resumeCompletion\",\"cancelled\":false,\"now\":1001}"));
    assert(software_licensing_session_has_completion(session) == 0);
    rejected_receipt(dispatch(session, receipt));
    software_licensing_session_destroy(session);
    puts("C ABI ownership and saved receipt routing passed.");
    return 0;
}
