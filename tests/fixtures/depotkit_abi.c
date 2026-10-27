#include <stddef.h>
#include <stdint.h>
#include <string.h>

typedef struct {
    uint32_t depot_id;
    const char *manifest_path;
    const char *key_hex;
} Depot;

typedef struct {
    uint32_t app_id;
    const Depot *depots;
    size_t depot_count;
    const char *install_dir;
    uint32_t max_downloads;
    int validate;
    uint32_t cell_id;
} Request;

typedef struct {
    uint64_t bytes_done;
    uint64_t bytes_total;
    double rate_bps;
    uint32_t files_done;
    uint32_t files_total;
    uint32_t depot_index;
    uint32_t depot_count;
    uint32_t phase;
    const char *detail;
} Progress;

static int cancelled;
static uint32_t cancel_count;

void *depotkit_control_create(void) { cancelled = 0; return &cancelled; }
void depotkit_control_destroy(void *control) { (void)control; }
void depotkit_cancel(void *control) { *(int *)control = 1; ++cancel_count; }
uint32_t depotkit_fixture_cancel_count(void) { return cancel_count; }

int depotkit_download(const Request *request, void (*progress)(const Progress *, void *), void *user, void *control) {
    if (!request || request->app_id != 480 || request->depot_count != 1 ||
        !request->depots || request->depots[0].depot_id != 381 ||
        !request->depots[0].manifest_path || !request->depots[0].key_hex ||
        !strstr(request->depots[0].manifest_path, "manifest-\xc3\xa9.bin") ||
        strcmp(request->depots[0].key_hex, "test-key") || !request->install_dir ||
        request->max_downloads != 16 || request->validate != 1 || request->cell_id != 0 ||
        !progress || !user || control != &cancelled) return 12;
    const Progress report = {256, 1024, 0, 0, 1, 0, 1, 3, "Downloading"};
    progress(&report, user);
    return cancelled ? 7 : 0;
}
