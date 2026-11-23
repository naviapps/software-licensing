#include <CSoftwareLicensing/software_licensing.h>
#include <assert.h>
#include <errno.h>
#include <inttypes.h>
#include <stdio.h>
#include <string.h>

static void copy_result(SoftwareLicensingJson *result, uint32_t code, char *buffer,
                        size_t capacity) {
    assert(result != NULL);
    SoftwareLicensingJsonView view = software_licensing_json_view(result);
    assert(view.code == code && view.data != NULL && view.length < capacity);
    memcpy(buffer, view.data, view.length);
    buffer[view.length] = '\0';
    software_licensing_json_destroy(result);
}

static void dispatch(SoftwareLicensingSession *session, const char *command, uint32_t code,
                     char *buffer, size_t capacity) {
    copy_result(
        software_licensing_session_dispatch(session, (const uint8_t *)command, strlen(command)),
        code, buffer, capacity);
}

int main(void) {
    assert(software_licensing_abi_version() == SOFTWARE_LICENSING_ABI_VERSION);
    SoftwareLicensingSession *session = software_licensing_session_create();
    assert(session != NULL);
    char reply[4096];

    const uint8_t record[] = "{\"version\":2,\"status\":\"unlicensed\","
                             "\"pendingDeactivation\":false,"
                             "\"lastOperationFailure\":{\"code\":\"requestFailure\"}}";
    copy_result(software_licensing_session_stage_record(session, record, sizeof record - 1), 0,
                reply, sizeof reply);
    dispatch(session,
             "{\"command\":\"restore\",\"policy\":{\"isEnabled\":true,"
             "\"validationInterval\":86400,\"failureGracePeriod\":604800,"
             "\"serverFailureGracePeriod\":86400},\"now\":1000}",
             0, reply, sizeof reply);
    assert(strstr(reply, "\"isLicensed\":false"));
    assert(strstr(reply, "\"lastOperationFailure\":{\"code\":\"requestFailure\""));

    // The owned fixture supplies synthetic entitlement and simulates persistence in memory.
    const char *adopt = "{\"command\":\"adoptExternallyVerifiedActivation\",\"activation\":{"
                        "\"source\":\"fixture\",\"planIdentifier\":\"paid\","
                        "\"activatedAt\":1000,\"expiresAt\":1020},\"now\":1000}";
    SoftwareLicensingJson *pending =
        software_licensing_session_dispatch(session, (const uint8_t *)adopt, strlen(adopt));
    size_t stored_length = 0;
    const uint8_t *candidate = software_licensing_json_persistence_record(pending, &stored_length);
    uint8_t stored_record[4096];
    assert(candidate && stored_length > 0 && stored_length <= sizeof stored_record);
    memcpy(stored_record, candidate, stored_length);
    copy_result(pending, 0, reply, sizeof reply);
    assert(strstr(reply, "\"isLicensed\":false"));
    assert(strstr(reply, "\"kind\":\"persist\""));
    const char *field = strstr(reply, "\"id\":");
    assert(field != NULL);
    field += strlen("\"id\":");
    assert(*field >= '0' && *field <= '9');
    char *end = NULL;
    errno = 0;
    uintmax_t id = strtoumax(field, &end, 10);
    assert(errno == 0 && end != field && (*end == ',' || *end == '}'));

    char acknowledgement[128];
    int length = snprintf(
        acknowledgement, sizeof acknowledgement,
        "{\"command\":\"persisted\",\"id\":%" PRIuMAX ",\"success\":true,\"now\":1001}", id);
    assert(length > 0 && (size_t)length < sizeof acknowledgement);
    dispatch(session, acknowledgement, 0, reply, sizeof reply);
    assert(strstr(reply, "\"isLicensed\":true"));
    dispatch(session, acknowledgement, 1, reply, sizeof reply);
    assert(strstr(reply, "\"code\":\"operationInProgress\""));

    software_licensing_session_destroy(session);
    session = software_licensing_session_create();
    assert(session != NULL);
    copy_result(software_licensing_session_stage_record(session, stored_record, stored_length), 0,
                reply, sizeof reply);
    dispatch(session,
             "{\"command\":\"restore\",\"policy\":{\"isEnabled\":true,"
             "\"validationInterval\":86400,\"failureGracePeriod\":604800,"
             "\"serverFailureGracePeriod\":86400},\"now\":1001}",
             0, reply, sizeof reply);
    assert(strstr(reply, "\"isLicensed\":true"));
    dispatch(session, "{\"command\":\"snapshot\",\"now\":1020}", 0, reply, sizeof reply);
    assert(strstr(reply, "\"status\":\"expired\""));
    assert(strstr(reply, "\"isLicensed\":false"));
    assert(strstr(reply, "\"effect\":null"));
    software_licensing_session_destroy(session);
    puts("Standalone installed C SoftwareLicensing consumer passed persistence and expiry.");
    return 0;
}
