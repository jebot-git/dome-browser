/* Test-only interposer: keep real backend initialization in flight while closing. */
#define _GNU_SOURCE
#include <gst/gst.h>
#include <dlfcn.h>
#include <fcntl.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

static void mark(const char *suffix) {
    const char *base = getenv("DOME_TEST_MEDIA_INIT_MARKER");
    if (!base) return;
    char *path = g_strconcat(base, suffix, NULL);
    int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0600);
    if (fd >= 0) { write(fd, "1", 1); close(fd); }
    g_free(path);
}

GstPluginFeature *gst_plugin_feature_load(GstPluginFeature *feature) {
    typedef GstPluginFeature *(*Load)(GstPluginFeature *);
    Load real_load = (Load)dlsym(RTLD_NEXT, "gst_plugin_feature_load");
    if (!real_load) _exit(97);
    const char *name = gst_plugin_feature_get_plugin_name(feature);
    int delayed = name && strcmp(name, "pulseaudio") == 0;
    if (delayed) { mark(".started"); g_usleep(4000000); }
    GstPluginFeature *loaded = real_load(feature);
    if (delayed) mark(".finished");
    return loaded;
}
