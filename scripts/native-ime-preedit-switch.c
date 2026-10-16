/* Active-preedit switch observer. No keyboard grab, forwarding, or replay.
 * Never commits before explicit post-switch challenge. A deactivated context
 * is abandoned, even if a later activation occurs. */
#include <errno.h>
#include <poll.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>
#include <wayland-client.h>
#include "input-method-unstable-v2-client-protocol.h"

static const char preedit_token[] = "EMBERPREEDITMARKER";

struct state {
    struct wl_display *display;
    struct wl_registry *registry;
    struct wl_seat *seat;
    struct zwp_input_method_manager_v2 *manager;
    struct zwp_input_method_v2 *method;
    const char *directory;
    const char *stage;
    uint32_t serial, origin_serial;
    bool pending_active, active, failed, unavailable, synced;
    bool preedit, committed, released, deactivated;
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
    while (!s->synced && !s->failed && !s->unavailable && monotonic() < deadline) {
        if (!pump(s, deadline)) break;
    }
    if (!s->synced) wl_callback_destroy(callback);
    return s->synced && !s->failed && !s->unavailable;
}
static void activate(void *data, struct zwp_input_method_v2 *method) {
    (void)method; struct state *s=data; s->pending_active = true;
    printf("activate preceding_done_serial=%u\n",s->serial);fflush(stdout);
}
static void deactivate(void *data, struct zwp_input_method_v2 *method) {
    (void)method; struct state *s=data; s->pending_active = false;
    if(s->preedit)s->deactivated=true;
    printf("deactivate preceding_done_serial=%u\n",s->serial);fflush(stdout);
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
    if (s->preedit && !s->active) s->deactivated = true;
}
static void unavailable(void *data, struct zwp_input_method_v2 *method) {
    (void)method; ((struct state *)data)->unavailable = true;
}
static const struct zwp_input_method_v2_listener method_listener = {
    .activate=activate,.deactivate=deactivate,.surrounding_text=surrounding,
    .text_change_cause=cause,.content_type=content,.done=done,.unavailable=unavailable
};
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
    if(argc!=2){fprintf(stderr,"usage: native-ime-preedit-switch CONTROL_DIRECTORY\n");return 2;}
    struct state s={.directory=argv[1],.stage="connect"}; int result=1; double deadline=monotonic()+90;
    s.display=wl_display_connect(NULL);
    if(!s.display)goto cleanup;
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
            s.stage="send-preedit";
            zwp_input_method_v2_set_preedit_string(s.method,preedit_token,
                (int32_t)strlen(preedit_token),(int32_t)strlen(preedit_token));
            zwp_input_method_v2_commit(s.method,s.serial);s.preedit=true;
            s.origin_serial=s.serial;
            printf("preedit origin_serial=%u\n",s.origin_serial);fflush(stdout);
            if(!sync_bounded(&s,deadline)||!marker(&s,"preedit",true))goto cleanup;
        }
        if(s.preedit&&!s.released&&marker(&s,"challenge",false)){
            s.stage="post-switch-challenge";
            if(!sync_bounded(&s,deadline)||s.unavailable)goto cleanup;
            printf("challenge origin_serial=%u current_serial=%u deactivated=%d active=%d\n",s.origin_serial,s.serial,s.deactivated,s.active);fflush(stdout);
            /* A new activation is not permission to replay old composition. */
            if(!s.deactivated && s.active){
                zwp_input_method_v2_set_preedit_string(s.method,"",0,0);
                zwp_input_method_v2_commit_string(s.method,"你");
                zwp_input_method_v2_commit(s.method,s.serial);
                s.committed=true;
            }
            if(!sync_bounded(&s,deadline))goto cleanup;
            if(!marker(&s,s.committed?"late-commit-sent":"context-abandoned",true))goto cleanup;
            s.released=true;
            if(!marker(&s,"challenged",true))goto cleanup;
        }
        if(s.released&&marker(&s,"finish",false)){
            if(!sync_bounded(&s,deadline))goto cleanup;
            result=0;break;
        }
        if(!pump(&s,deadline))break;
    }
cleanup:
    if(result){
        int display_error=s.display?wl_display_get_error(s.display):0;
        const struct wl_interface *interface=NULL; uint32_t object=0;
        uint32_t protocol_error=s.display?wl_display_get_protocol_error(s.display,&interface,&object):0;
        fprintf(stderr,"fixture failure stage=%s display_error=%d protocol_error=%u interface=%s object=%u active=%d preedit=%d committed=%d released=%d\n",
            s.stage,display_error,protocol_error,interface?interface->name:"none",object,
            s.active,s.preedit,s.committed,s.released);
    }
    if(s.method)zwp_input_method_v2_destroy(s.method);
    if(s.manager)zwp_input_method_manager_v2_destroy(s.manager);
    if(s.seat)wl_seat_destroy(s.seat);
    if(s.registry)wl_registry_destroy(s.registry);
    if(s.display){wl_display_flush(s.display);wl_display_disconnect(s.display);}
    if(result)fprintf(stderr,"IME preedit switch fixture failed or deadline expired\n");
    return result;
}
