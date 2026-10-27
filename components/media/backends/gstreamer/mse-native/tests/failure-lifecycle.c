/* Parser failures and source-buffer removal must emit one ordered completion,
 * reject subsequent operations, and release all native tasks on shutdown. */
#include <gst/gst.h>
#include <gst/mse/mse.h>
#include <stdio.h>
#include <string.h>

typedef struct {
  GMainLoop *loop;
  GstMediaSource *source;
  GstSourceBuffer *buffer;
  GstElement *pipeline;
  GString *events;
  const gchar *mode;
  gchar *data;
  gsize size;
  guint finish_id, bus_errors;
  gboolean opened, failed, removed;
} Probe;
typedef struct { Probe *probe; gchar kind; } Event;

static void fail (Probe *p, const gchar *message) {
  g_printerr ("probe error: %s\n", message);
  p->failed = TRUE;
  g_main_loop_quit (p->loop);
}
static gboolean remove_buffer (Probe *p) {
  GError *error = NULL;
  if (!gst_media_source_remove_source_buffer (p->source, p->buffer, &error)) {
    fail (p, error->message);
    g_clear_error (&error);
    return FALSE;
  }
  p->removed = TRUE;
  return TRUE;
}
static gboolean finish (gpointer data) {
  Probe *p = data;
  p->finish_id = 0;
  GError *error = NULL;
  if (gst_source_buffer_get_updating (p->buffer)) fail (p, "still updating");
  if (!strcmp (p->mode, "malformed")) {
    if (strcmp (p->events->str, "SEF")) fail (p, "incorrect error events");
    if (p->bus_errors != 1 || gst_media_source_get_ready_state (p->source) !=
        GST_MEDIA_SOURCE_READY_STATE_ENDED) fail (p, "missing decode failure");
    if (gst_source_buffer_append_buffer (p->buffer,
        gst_buffer_new_allocate (NULL, 1, NULL), &error) || !error ||
        error->code != GST_MEDIA_SOURCE_ERROR_INVALID_STATE)
      fail (p, "errored buffer accepted append");
    g_clear_error (&error);
    remove_buffer (p);
  } else {
    const gchar *expected = !strcmp (p->mode, "active-remove") ? "SAF" : "SUF";
    if (strcmp (p->events->str, expected) || p->bus_errors)
      fail (p, "incorrect removal events or unexpected bus error");
  }
  if (!p->removed) fail (p, "buffer was not removed");
  if (gst_media_source_remove_source_buffer (p->source, p->buffer, &error) ||
      !error || error->code != GST_MEDIA_SOURCE_ERROR_NOT_FOUND)
    fail (p, "second removal was not rejected");
  g_clear_error (&error);
  if (gst_source_buffer_append_buffer (p->buffer,
      gst_buffer_new_allocate (NULL, 1, NULL), &error) || !error ||
      error->code != GST_MEDIA_SOURCE_ERROR_INVALID_STATE)
    fail (p, "removed buffer accepted append");
  g_clear_error (&error);
  GstSourceBufferList *list = gst_media_source_get_source_buffers (p->source);
  if (gst_source_buffer_list_get_length (list)) fail (p, "removed buffer remains listed");
  gst_object_unref (list);
  g_main_loop_quit (p->loop);
  return G_SOURCE_REMOVE;
}
static gboolean record_event (gpointer data) {
  Event *event = data;
  Probe *p = event->probe;
  g_string_append_c (p->events, event->kind);
  if (event->kind == 'F' && !p->finish_id) {
    if (!strcmp (p->mode, "idle-remove")) remove_buffer (p);
    p->finish_id = g_timeout_add (2200, finish, p);
  }
  return G_SOURCE_REMOVE;
}
static void queue_event (Probe *p, gchar kind) {
  Event *event = g_new (Event, 1);
  *event = (Event) {p, kind};
  g_idle_add_full (G_PRIORITY_DEFAULT, record_event, event, g_free);
}
static void start_event (GstSourceBuffer *b, Probe *p) { queue_event (p, 'S'); }
static void update_event (GstSourceBuffer *b, Probe *p) { queue_event (p, 'U'); }
static void error_event (GstSourceBuffer *b, Probe *p) { queue_event (p, 'E'); }
static void abort_event (GstSourceBuffer *b, Probe *p) { queue_event (p, 'A'); }
static void end_event (GstSourceBuffer *b, Probe *p) { queue_event (p, 'F'); }
static gboolean open_source (gpointer data) {
  Probe *p = data;
  if (p->opened) return G_SOURCE_REMOVE;
  p->opened = TRUE;
  GError *error = NULL;
  p->buffer = gst_media_source_add_source_buffer (p->source,
      "video/mp4; codecs=\"avc1.42E01E\"", &error);
  if (!p->buffer) { fail (p, error->message); g_clear_error (&error); return G_SOURCE_REMOVE; }
  g_signal_connect (p->buffer, "on-update-start", G_CALLBACK (start_event), p);
  g_signal_connect (p->buffer, "on-update", G_CALLBACK (update_event), p);
  g_signal_connect (p->buffer, "on-error", G_CALLBACK (error_event), p);
  g_signal_connect (p->buffer, "on-abort", G_CALLBACK (abort_event), p);
  g_signal_connect (p->buffer, "on-update-end", G_CALLBACK (end_event), p);
  GstBuffer *bytes;
  if (!strcmp (p->mode, "malformed")) {
    /* Valid ftyp, then an invalid four-byte moov box. Repeat to exceed the
     * demuxer's initial buffering threshold and force an actual parser error. */
    const guint8 invalid[] = {0,0,0,24,'f','t','y','p','i','s','o','5',0,0,2,0,
        'i','s','o','5','i','s','o','6',0,0,0,4,'m','o','o','v'};
    bytes = gst_buffer_new_allocate (NULL, sizeof invalid * 512, NULL);
    for (guint i = 0; i < 512; i++) gst_buffer_fill (bytes, sizeof invalid * i, invalid, sizeof invalid);
  } else {
    bytes = gst_buffer_new_allocate (NULL, p->size, NULL);
    gst_buffer_fill (bytes, 0, p->data, p->size);
  }
  if (!gst_source_buffer_append_buffer (p->buffer, bytes, &error)) {
    fail (p, error->message); g_clear_error (&error);
  } else if (!strcmp (p->mode, "active-remove")) {
    if (!gst_source_buffer_get_updating (p->buffer)) fail (p, "append completed before active removal");
    remove_buffer (p);
  }
  return G_SOURCE_REMOVE;
}
static void opened (GstMediaSource *s, Probe *p) { g_idle_add (open_source, p); }
static void source_setup (GstElement *pipeline, GstElement *element, Probe *p) {
  gst_media_source_attach (p->source, GST_MSE_SRC (element));
}
static gboolean bus_message (GstBus *bus, GstMessage *message, gpointer data) {
  Probe *p = data;
  if (GST_MESSAGE_TYPE (message) == GST_MESSAGE_ERROR) {
    p->bus_errors++;
    GError *error = NULL;
    gst_message_parse_error (message, &error, NULL);
    if (strcmp (p->mode, "malformed") || error->domain != GST_STREAM_ERROR ||
        error->code != GST_STREAM_ERROR_DECODE) fail (p, error->message);
    g_clear_error (&error);
  }
  return G_SOURCE_CONTINUE;
}
static gboolean watchdog (gpointer data) { fail (data, "timeout"); return G_SOURCE_CONTINUE; }
int main (int argc, char **argv) {
  gst_init (&argc, &argv);
  if (argc != 3 || (strcmp (argv[1], "malformed") && strcmp (argv[1], "active-remove") &&
      strcmp (argv[1], "idle-remove"))) return 2;
  Probe p = {.mode = argv[1]};
  if (!g_file_get_contents (argv[2], &p.data, &p.size, NULL)) return 2;
  p.events = g_string_new (NULL);
  p.loop = g_main_loop_new (NULL, FALSE);
  p.source = gst_media_source_new ();
  p.pipeline = gst_element_factory_make ("playbin3", NULL);
  GstElement *sink = gst_element_factory_make ("fakesink", NULL);
  gst_object_ref_sink (sink);
  g_object_set (sink, "sync", FALSE, NULL);
  g_object_set (p.pipeline, "video-sink", sink, "uri", "mse://failure-lifecycle", NULL);
  g_signal_connect (p.source, "on-source-open", G_CALLBACK (opened), &p);
  g_signal_connect (p.pipeline, "source-setup", G_CALLBACK (source_setup), &p);
  GstBus *bus = gst_element_get_bus (p.pipeline);
  guint watch = gst_bus_add_watch (bus, bus_message, &p);
  guint timeout = g_timeout_add_seconds (8, watchdog, &p);
  gst_element_set_state (p.pipeline, GST_STATE_PAUSED);
  g_main_loop_run (p.loop);
  g_source_remove (timeout);
  g_source_remove (watch);
  if (p.finish_id) g_source_remove (p.finish_id);
  gst_element_set_state (p.pipeline, GST_STATE_NULL);
  gst_media_source_detach (p.source);
  gst_clear_object (&p.pipeline);
  gst_clear_object (&bus);
  gst_clear_object (&sink);
  gst_clear_object (&p.buffer);
  gst_clear_object (&p.source);
  while (g_main_context_iteration (NULL, FALSE));
  g_main_loop_unref (p.loop);
  g_free (p.data);
  printf ("{\"case\":\"%s\",\"events\":\"%s\",\"bus_errors\":%u,\"passed\":%s}\n",
      p.mode, p.events->str, p.bus_errors, p.failed ? "false" : "true");
  g_string_free (p.events, TRUE);
  fflush (stdout);
  gst_deinit ();
  return p.failed ? 1 : 0;
}
