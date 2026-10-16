/* Synthetic compositor IME fixture. Public input-method-v2 only; no app hook.
 * The sole injected payload is fixed "你". Native Return is consumed by the
 * compositor keyboard grab, and no key event is forwarded or replayed. */
#include <errno.h>
#include <poll.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>
#include <wayland-client.h>
#include <xkbcommon/xkbcommon.h>
#include <xkbcommon/xkbcommon-keysyms.h>
#include "input-method-unstable-v2-client-protocol.h"

static const char preedit_token[] = "EMBERPREEDITMARKER";

struct state {
    struct wl_display *display;
    struct wl_registry *registry;
    struct wl_seat *seat;
    struct zwp_input_method_manager_v2 *manager;
    struct zwp_input_method_v2 *method;
    struct zwp_input_method_keyboard_grab_v2 *grab;
    struct xkb_context *xkb;
    struct xkb_keymap *keymap;
    struct xkb_state *keys;
    const char *directory;
    const char *stage;
    uint32_t serial, enter_key;
    bool pending_active, active, failed, unavailable, synced;
    bool preedit, committed, released;
};
static double monotonic(void) {
    struct timespec t;
    if (clock_gettime(CLOCK_MONOTONIC, &t)) abort();
    return (double)t.tv_sec + (double)t.tv_nsec / 1e9;
}
static bool marker(struct state *s, const char *name, bool write_marker) {
    char path[4096];
    int n = snprintf(path, sizeof(path), "%s/%s", s->directory, name);
    if (n < 0 || n >= (int)sizeof(path)) { s->failed = true; return false; }
    if (!write_marker) return access(path, F_OK) == 0;
    FILE *f = fopen(path, "w");
    if (!f) { s->failed = true; return false; }
    bool ok = fputs("ready\n", f) >= 0;
    if (fclose(f)) ok = false;
    if (!ok) s->failed = true;
    return ok;
}
/* Standard prepare/read loop: every blocking poll is bounded by the single
 * monotonic fixture deadline; flush EAGAIN never turns into an unbounded call. */
static bool pump(struct state *s, double deadline) {
    while (wl_display_prepare_read(s->display) != 0) {
        if (monotonic() >= deadline || wl_display_dispatch_pending(s->display) < 0) return false;
    }
    int flushed = wl_display_flush(s->display);
    if (flushed < 0 && errno != EAGAIN) { wl_display_cancel_read(s->display); return false; }
    double remaining = deadline - monotonic();
    if (remaining <= 0) { wl_display_cancel_read(s->display); return false; }
    int timeout = remaining < 0.05 ? (int)(remaining * 1000) : 50;
    struct pollfd fd = {.fd=wl_display_get_fd(s->display), .events=POLLIN};
    if (flushed < 0) fd.events |= POLLOUT;
    int ready = poll(&fd, 1, timeout);
    if (ready < 0 || (fd.revents & (POLLERR | POLLHUP | POLLNVAL))) {
        wl_display_cancel_read(s->display);
        return ready < 0 && errno == EINTR;
    }
    if (fd.revents & POLLIN) {
        if (wl_display_read_events(s->display) < 0) return false;
    } else wl_display_cancel_read(s->display);
    return wl_display_dispatch_pending(s->display) >= 0;
}
static void sync_done(void *data, struct wl_callback *callback, uint32_t serial) {
    (void)serial; ((struct state *)data)->synced = true; wl_callback_destroy(callback);
}
static const struct wl_callback_listener sync_listener = {.done=sync_done};
static bool sync_bounded(struct state *s, double deadline) {
    s->synced = false;
    struct wl_callback *callback = wl_display_sync(s->display);
    if (!callback) return false;
    if (wl_callback_add_listener(callback, &sync_listener, s) < 0) {
        wl_callback_destroy(callback); return false;
    }
    while (!s->synced && !s->failed && monotonic() < deadline) {
        if (!pump(s, deadline)) break;
    }
    if (!s->synced) wl_callback_destroy(callback);
    return s->synced && !s->failed;
}
static void activate(void *data, struct zwp_input_method_v2 *method) {
    (void)method; ((struct state *)data)->pending_active = true;
}
static void deactivate(void *data, struct zwp_input_method_v2 *method) {
    (void)method; ((struct state *)data)->pending_active = false;
}
static void surrounding(void *d, struct zwp_input_method_v2 *m, const char *t, uint32_t c, uint32_t a) {
    (void)d; (void)m; (void)t; (void)c; (void)a;
}
static void cause(void *d, struct zwp_input_method_v2 *m, uint32_t v) { (void)d;(void)m;(void)v; }
static void content(void *d, struct zwp_input_method_v2 *m, uint32_t h, uint32_t p) { (void)d;(void)m;(void)h;(void)p; }
static void done(void *data, struct zwp_input_method_v2 *method) {
    (void)method; struct state *s = data;
    s->serial++; s->active = s->pending_active;
    printf("done serial=%u active=%d\n", s->serial, s->active); fflush(stdout);
    if (s->preedit && !s->released && !s->active) s->failed = true;
}
static void unavailable(void *data, struct zwp_input_method_v2 *method) {
    (void)method; ((struct state *)data)->unavailable = true;
}
static const struct zwp_input_method_v2_listener method_listener = {
    .activate=activate,.deactivate=deactivate,.surrounding_text=surrounding,
    .text_change_cause=cause,.content_type=content,.done=done,.unavailable=unavailable
};
static void keymap(void *data, struct zwp_input_method_keyboard_grab_v2 *grab,
                   uint32_t format, int32_t fd, uint32_t size) {
    (void)grab; struct state *s = data; struct stat st;
    fprintf(stderr,"keymap received format=%u size=%u\n",format,size);
    void *mapped = MAP_FAILED; struct xkb_keymap *map = NULL; struct xkb_state *keys = NULL;
    if (format != WL_KEYBOARD_KEYMAP_FORMAT_XKB_V1 || !size || size > 1024 * 1024 ||
        fstat(fd, &st) || st.st_size < (off_t)size) goto fail;
    mapped = mmap(NULL, size, PROT_READ, MAP_PRIVATE, fd, 0);
    if (mapped == MAP_FAILED || ((const char *)mapped)[size - 1] != '\0') goto fail;
    map = xkb_keymap_new_from_string(s->xkb, mapped, XKB_KEYMAP_FORMAT_TEXT_V1, XKB_KEYMAP_COMPILE_NO_FLAGS);
    if (!map || !(keys = xkb_state_new(map))) goto fail;
    if (s->keys) xkb_state_unref(s->keys);
    if (s->keymap) xkb_keymap_unref(s->keymap);
    s->keys = keys; s->keymap = map;
    fprintf(stderr,"keymap accepted\n");
    munmap(mapped, size); close(fd); return;
fail:
    if (keys) xkb_state_unref(keys);
    if (map) xkb_keymap_unref(map);
    if (mapped != MAP_FAILED) munmap(mapped, size);
    close(fd); s->failed = true; s->stage = "keymap-validation";
}
static void key(void *data, struct zwp_input_method_keyboard_grab_v2 *grab,
                uint32_t serial, uint32_t time, uint32_t code, uint32_t value) {
    (void)grab;(void)serial;(void)time; struct state *s=data;
    if (!s->keys || code > UINT32_MAX - 8 || !s->preedit || !s->active) {s->failed=true;return;}
    xkb_keysym_t symbol = xkb_state_key_get_one_sym(s->keys, code + 8);
    if (symbol != XKB_KEY_Return && symbol != XKB_KEY_KP_Enter) {s->failed=true;return;}
    if (value == WL_KEYBOARD_KEY_STATE_PRESSED && !s->committed) {
        s->enter_key = code;
        zwp_input_method_v2_set_preedit_string(s->method, "", 0, 0);
        zwp_input_method_v2_commit_string(s->method, "你");
        zwp_input_method_v2_commit(s->method, s->serial);
        s->committed = true;
        puts("native Return press consumed; commit queued"); fflush(stdout);
    } else if (value == WL_KEYBOARD_KEY_STATE_RELEASED && s->committed && code == s->enter_key && !s->released) {
        zwp_input_method_keyboard_grab_v2_release(s->grab); s->grab = NULL;
        s->released = true;
        puts("native Return release consumed; grab released"); fflush(stdout);
    } else s->failed = true;
}
static void modifiers(void *data, struct zwp_input_method_keyboard_grab_v2 *grab,
                     uint32_t serial, uint32_t depressed, uint32_t latched, uint32_t locked, uint32_t group) {
    (void)grab;(void)serial; struct state *s=data;
    if (s->keys) xkb_state_update_mask(s->keys, depressed, latched, locked, 0, 0, group);
}
static void repeat(void *d, struct zwp_input_method_keyboard_grab_v2 *g, int32_t r, int32_t t) {(void)d;(void)g;(void)r;(void)t;}
static const struct zwp_input_method_keyboard_grab_v2_listener grab_listener={.keymap=keymap,.key=key,.modifiers=modifiers,.repeat_info=repeat};
static void seat_caps(void *data, struct wl_seat *seat, uint32_t capabilities) {
    (void)data; (void)seat; (void)capabilities;
}
static void seat_name(void *data, struct wl_seat *seat, const char *name) {
    (void)data; (void)seat; (void)name;
}
static const struct wl_seat_listener seat_listener = {.capabilities=seat_caps,.name=seat_name};
static void global(void *data, struct wl_registry *registry, uint32_t name, const char *interface, uint32_t version) {
    (void)version; struct state *s=data;
    if (!strcmp(interface,"wl_seat") && !s->seat) {
        s->seat=wl_registry_bind(registry,name,&wl_seat_interface,1);
        if (!s->seat || wl_seat_add_listener(s->seat,&seat_listener,s)<0) s->failed=true;
    }
    if (!strcmp(interface,"zwp_input_method_manager_v2") && !s->manager) {
        s->manager=wl_registry_bind(registry,name,&zwp_input_method_manager_v2_interface,1);
        if (!s->manager) s->failed=true;
    }
}
static void removed(void *d, struct wl_registry *r, uint32_t n) {(void)d;(void)r;(void)n;}
static const struct wl_registry_listener registry_listener={.global=global,.global_remove=removed};
int main(int argc,char **argv) {
    if(argc!=2){fprintf(stderr,"usage: native-ime-return CONTROL_DIRECTORY\n");return 2;}
    struct state s={.directory=argv[1],.stage="connect"}; int result=1; double deadline=monotonic()+30;
    s.xkb=xkb_context_new(XKB_CONTEXT_NO_FLAGS);
    s.display=wl_display_connect(NULL);
    if(!s.xkb||!s.display)goto cleanup;
    s.stage="registry";
    s.registry=wl_display_get_registry(s.display);
    if(!s.registry || wl_registry_add_listener(s.registry,&registry_listener,&s)<0)goto cleanup;
    if(!sync_bounded(&s,deadline)||!s.seat||!s.manager)goto cleanup;
    s.stage="input-method-connect";
    s.method=zwp_input_method_manager_v2_get_input_method(s.manager,s.seat);
    if(!s.method || zwp_input_method_v2_add_listener(s.method,&method_listener,&s)<0)goto cleanup;
    if(!sync_bounded(&s,deadline)||s.unavailable||!marker(&s,"connected",true))goto cleanup;
    s.stage="await-start-active";
    while(monotonic()<deadline&&!s.failed&&!s.unavailable){
        if(s.active&&!s.preedit&&marker(&s,"start",false)){
            s.stage="grab-keyboard";
            s.grab=zwp_input_method_v2_grab_keyboard(s.method);
            if(!s.grab || zwp_input_method_keyboard_grab_v2_add_listener(s.grab,&grab_listener,&s)<0)goto cleanup;
            if(!sync_bounded(&s,deadline))goto cleanup;
            s.stage="grab-keymap-ready";
            if(!s.keys)goto cleanup;
            s.stage="send-preedit";
            zwp_input_method_v2_set_preedit_string(s.method,preedit_token,
                (int32_t)strlen(preedit_token),(int32_t)strlen(preedit_token));
            zwp_input_method_v2_commit(s.method,s.serial);s.preedit=true;
            if(!sync_bounded(&s,deadline)||!marker(&s,"preedit",true))goto cleanup;
        }
        if(s.preedit&&!s.committed)s.stage="await-composition-return";
        if(s.released&&!marker(&s,"committed",false)){
            if(!sync_bounded(&s,deadline)||!marker(&s,"committed",true))goto cleanup;
        }
        if(s.released&&marker(&s,"finish",false)){result=0;break;}
        if(!pump(&s,deadline))break;
    }
cleanup:
    if(result){
        int display_error=s.display?wl_display_get_error(s.display):0;
        const struct wl_interface *interface=NULL; uint32_t object=0;
        uint32_t protocol_error=s.display?wl_display_get_protocol_error(s.display,&interface,&object):0;
        fprintf(stderr,"fixture failure stage=%s display_error=%d protocol_error=%u interface=%s object=%u active=%d keymap=%d preedit=%d committed=%d released=%d\n",
            s.stage,display_error,protocol_error,interface?interface->name:"none",object,
            s.active,s.keys!=NULL,s.preedit,s.committed,s.released);
    }
    if(s.grab)zwp_input_method_keyboard_grab_v2_release(s.grab);
    if(s.method)zwp_input_method_v2_destroy(s.method);
    if(s.manager)zwp_input_method_manager_v2_destroy(s.manager);
    if(s.seat)wl_seat_destroy(s.seat);
    if(s.registry)wl_registry_destroy(s.registry);
    if(s.display){wl_display_flush(s.display);wl_display_disconnect(s.display);}
    if(s.keys)xkb_state_unref(s.keys);
    if(s.keymap)xkb_keymap_unref(s.keymap);
    if(s.xkb)xkb_context_unref(s.xkb);
    if(result)fprintf(stderr,"IME Return fixture failed or deadline expired\n");
    return result;
}
