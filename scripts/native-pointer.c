/* Synthetic test pointer through the compositor's public virtual-pointer
 * protocol. Keeps pointer capability alive for the duration of desktop QA.
 * No device files, uinput, application hooks, or elevated privileges. */
#include <errno.h>
#include <stdint.h>
#include <stdlib.h>
#include <stdio.h>
#include <string.h>
#include <time.h>
#include <unistd.h>
#include <wayland-client.h>
#include "wlr-virtual-pointer-unstable-v1-client-protocol.h"

static struct zwlr_virtual_pointer_manager_v1 *manager;
static void global(void *data, struct wl_registry *registry, uint32_t name,
                   const char *interface, uint32_t version) {
    (void)data; (void)version;
    if (!strcmp(interface, "zwlr_virtual_pointer_manager_v1"))
        manager = wl_registry_bind(registry, name,
                                   &zwlr_virtual_pointer_manager_v1_interface, 1);
}
static void removed(void *data, struct wl_registry *registry, uint32_t name) {
    (void)data; (void)registry; (void)name;
}
static const struct wl_registry_listener listener = {.global=global, .global_remove=removed};
static uint32_t milliseconds(void) {
    struct timespec t;
    if (clock_gettime(CLOCK_MONOTONIC, &t) != 0) return 0;
    return (uint32_t)((uint64_t)t.tv_sec * 1000 + t.tv_nsec / 1000000);
}
static int coordinate(const char *text, unsigned *value) {
    if (!*text || strspn(text, "0123456789") != strlen(text)) return 0;
    errno = 0;
    char *end;
    unsigned long long number = strtoull(text, &end, 10);
    if (errno || *end || number > UINT32_MAX) return 0;
    *value = (unsigned)number;
    return 1;
}
int main(void) {
    struct wl_display *display = wl_display_connect(NULL);
    if (!display) return 2;
    struct wl_registry *registry = wl_display_get_registry(display);
    wl_registry_add_listener(registry, &listener, NULL);
    if (wl_display_roundtrip(display) < 0 || !manager) return 2;
    struct zwlr_virtual_pointer_v1 *pointer =
        zwlr_virtual_pointer_manager_v1_create_virtual_pointer(manager, NULL);
    if (wl_display_roundtrip(display) < 0) return 2;
    /* Let other clients receive seat capability changes before pointer use. */
    usleep(250000);
    puts("ready"); fflush(stdout);
    char line[128], extra, operation[6], sx[32], sy[32], sw[32], sh[32];
    unsigned x, y, width, height;
    while (fgets(line, sizeof(line), stdin)) {
        if (!strchr(line, '\n')
            || sscanf(line, "%5s %31s %31s %31s %31s %c", operation, sx, sy, sw, sh, &extra) != 5
            || (strcmp(operation, "click") && strcmp(operation, "move"))
            || !coordinate(sx, &x) || !coordinate(sy, &y)
            || !coordinate(sw, &width) || !coordinate(sh, &height)
            || !width || !height || x >= width || y >= height) return 2;
        zwlr_virtual_pointer_v1_motion_absolute(pointer, milliseconds(), x, y, width, height);
        zwlr_virtual_pointer_v1_frame(pointer);
        if (wl_display_roundtrip(display) < 0) return 2;
        if (!strcmp(operation, "move")) {
            puts("done"); fflush(stdout);
            continue;
        }
        usleep(100000);
        zwlr_virtual_pointer_v1_button(pointer, milliseconds(), 0x110, WL_POINTER_BUTTON_STATE_PRESSED);
        zwlr_virtual_pointer_v1_frame(pointer);
        if (wl_display_roundtrip(display) < 0) return 2;
        usleep(50000);
        zwlr_virtual_pointer_v1_button(pointer, milliseconds(), 0x110, WL_POINTER_BUTTON_STATE_RELEASED);
        zwlr_virtual_pointer_v1_frame(pointer);
        if (wl_display_roundtrip(display) < 0) return 2;
        puts("done"); fflush(stdout);
    }
    zwlr_virtual_pointer_v1_destroy(pointer);
    zwlr_virtual_pointer_manager_v1_destroy(manager);
    wl_registry_destroy(registry);
    wl_display_disconnect(display);
    return 0;
}
