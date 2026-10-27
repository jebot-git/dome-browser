/* Synthetic MSE playback with all application-owned objects released before
 * gst_deinit(), allowing the built-in GstSample leak tracer to inspect owners. */
#include <gst/gst.h>
#include <gst/mse/mse.h>
#include <stdio.h>
#include <stdlib.h>

typedef struct {
  GMainLoop *loop;
  GstMediaSource *source;
  GstSourceBuffer *buffer;
  GstElement *pipeline;
  GstElement *sink;
  gchar *data;
  gsize size;
  guint frames;
  gboolean opened, ended, failed;
} Probe;

static void fail (Probe *p, const gchar *message) {
  g_printerr ("probe error: %s\n", message);
  p->failed = TRUE;
  g_main_loop_quit (p->loop);
}
static gboolean end_append (gpointer data) {
  Probe *p = data;
  if (!p->ended) {
    p->ended = TRUE;
    GError *error = NULL;
    if (!gst_media_source_end_of_stream (p->source,
        GST_MEDIA_SOURCE_EOS_ERROR_NONE, &error)) {
      fail (p, error->message);
      g_clear_error (&error);
    }
  }
  return G_SOURCE_REMOVE;
}
static void updated (GstSourceBuffer *buffer, Probe *p) {
  g_idle_add (end_append, p);
}
static void append_error (GstSourceBuffer *buffer, Probe *p) {
  p->failed = TRUE;
  g_main_loop_quit (p->loop);
}
static gboolean open_source (gpointer data) {
  Probe *p = data;
  if (p->opened) return G_SOURCE_REMOVE;
  p->opened = TRUE;
  GError *error = NULL;
  p->buffer = gst_media_source_add_source_buffer (p->source,
      "video/mp4; codecs=\"avc1.42E01E\"", &error);
  if (!p->buffer) {
    fail (p, error->message);
    g_clear_error (&error);
    return G_SOURCE_REMOVE;
  }
  g_signal_connect (p->buffer, "on-update-end", G_CALLBACK (updated), p);
  g_signal_connect (p->buffer, "on-error", G_CALLBACK (append_error), p);
  GstBuffer *buffer = gst_buffer_new_allocate (NULL, p->size, NULL);
  gst_buffer_fill (buffer, 0, p->data, p->size);
  if (!gst_source_buffer_append_buffer (p->buffer, buffer, &error)) {
    fail (p, error->message);
    g_clear_error (&error);
  }
  return G_SOURCE_REMOVE;
}
static void opened (GstMediaSource *source, Probe *p) {
  g_idle_add (open_source, p);
}
static void source_setup (GstElement *pipeline, GstElement *element, Probe *p) {
  gst_media_source_attach (p->source, GST_MSE_SRC (element));
}
static void handoff (GstElement *sink, GstBuffer *buffer, GstPad *pad, Probe *p) {
  p->frames++;
}
static gboolean bus_message (GstBus *bus, GstMessage *message, gpointer data) {
  Probe *p = data;
  if (GST_MESSAGE_TYPE (message) == GST_MESSAGE_EOS) g_main_loop_quit (p->loop);
  if (GST_MESSAGE_TYPE (message) == GST_MESSAGE_ERROR) {
    GError *error = NULL;
    gchar *debug = NULL;
    gst_message_parse_error (message, &error, &debug);
    fail (p, error->message);
    g_clear_error (&error);
    g_free (debug);
  }
  return G_SOURCE_CONTINUE;
}
static gboolean timeout_cb (gpointer data) {
  fail (data, "timeout");
  return G_SOURCE_CONTINUE;
}
int main (int argc, char **argv) {
  gst_init (&argc, &argv);
  if (argc < 2 || argc > 3) return 2;
  guint repeats = argc == 3 ? (guint) atoi (argv[2]) : 1;
  gboolean overall_passed = TRUE;
  for (guint cycle = 0; cycle < repeats; cycle++) {
  Probe p = {0};
  GError *error = NULL;
  if (!g_file_get_contents (argv[1], &p.data, &p.size, &error)) return 2;
  p.loop = g_main_loop_new (NULL, FALSE);
  p.source = gst_media_source_new ();
  p.pipeline = gst_element_factory_make ("playbin3", NULL);
  p.sink = gst_element_factory_make ("fakesink", NULL);
  gst_object_ref_sink (p.sink);
  g_object_set (p.sink, "sync", FALSE, "signal-handoffs", TRUE, NULL);
  g_signal_connect (p.sink, "handoff", G_CALLBACK (handoff), &p);
  g_object_set (p.pipeline, "video-sink", p.sink, "uri", "mse://ownership", NULL);
  g_signal_connect (p.source, "on-source-open", G_CALLBACK (opened), &p);
  g_signal_connect (p.pipeline, "source-setup", G_CALLBACK (source_setup), &p);
  GstBus *bus = gst_element_get_bus (p.pipeline);
  guint watch = gst_bus_add_watch (bus, bus_message, &p);
  guint timeout = g_timeout_add_seconds (10, timeout_cb, &p);
  gst_element_set_state (p.pipeline, GST_STATE_PLAYING);
  g_printerr ("phase: pipeline started\n");
  g_main_loop_run (p.loop);
  g_printerr ("phase: loop done frames=%u\n", p.frames);
  g_source_remove (timeout);
  g_source_remove (watch);
  gst_element_set_state (p.pipeline, GST_STATE_NULL);
  g_printerr ("phase: pipeline NULL\n");
  gst_media_source_detach (p.source);
  g_printerr ("phase: detached\n");
  gst_clear_object (&p.pipeline);
  gst_clear_object (&bus);
  gst_clear_object (&p.sink);
  g_printerr ("phase: releasing buffer\n");
  gst_clear_object (&p.buffer);
  g_printerr ("phase: buffer released\n");
  gst_clear_object (&p.source);
  g_free (p.data);
  g_main_loop_unref (p.loop);
  gboolean passed = !p.failed && p.frames == 30;
  printf ("{\"playback_passed\":%s,\"frames\":%u}\n", passed ? "true" : "false", p.frames);
  fflush (stdout);
  overall_passed &= passed;
  }
  gst_deinit ();
  return overall_passed ? 0 : 1;
}
