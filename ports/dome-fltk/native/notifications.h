// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once
#include "appearance.h"
#include <gio/gio.h>
#include <functional>
#include <map>
#include <memory>
#include <set>

// DomeD owns the freedesktop service on the session bus. Keep its returned IDs
// and offered actions private; a signal from another bus peer cannot act on a page.
class DesktopNotifications {
  static constexpr const char *service = "org.freedesktop.Notifications";
  static constexpr const char *path = "/org/freedesktop/Notifications";
  struct Notice {
    Json data;
    guint id = 0;
    bool pending = false;
    uint64_t attempt = 0;
    gint64 retryAt = 0;
  };
  struct State {
    GDBusConnection *bus = nullptr;
    GCancellable *cancel = g_cancellable_new();
    guint watch = 0, actionSignal = 0, closedSignal = 0;
    uint64_t generation = 0, nextAttempt = 0;
    std::string owner;
    std::map<std::string, Notice> notices;
    std::function<void(const Json &, const std::string &)> chosen;
    std::function<void(const Json &)> delivered;
    ~State() { g_object_unref(cancel); if (bus) g_object_unref(bus); }
  };
  struct Call {
    std::weak_ptr<State> state;
    std::string key;
    uint64_t generation, attempt;
  };
  std::shared_ptr<State> state = std::make_shared<State>();
  static void send(const std::shared_ptr<State> &s, const std::string &key) {
    auto found = s->notices.find(key);
    if (s->owner.empty() || found == s->notices.end() || found->second.pending)
      return;
    auto &n = found->second;
    if (g_get_monotonic_time() < n.retryAt) return;
    if (n.data.value("expires_at", int64_t(0)) > 0 &&
        n.data["expires_at"].get<int64_t>() <= g_get_real_time() / 1000) {
      s->notices.erase(found); return;
    }
    GVariantBuilder actions, hints;
    g_variant_builder_init(&actions, G_VARIANT_TYPE("as"));
    for (const auto &action : n.data["actions"]) {
      g_variant_builder_add(&actions, "s", action[0].get<std::string>().c_str());
      g_variant_builder_add(&actions, "s", action[1].get<std::string>().c_str());
    }
    g_variant_builder_init(&hints, G_VARIANT_TYPE("a{sv}"));
    g_variant_builder_add(&hints, "{sv}", "desktop-entry",
                         g_variant_new_string("org.domed.Browser"));
    g_variant_builder_add(&hints, "{sv}", "category", g_variant_new_string(
        n.data.value("kind", "") == "calendar" ? "x-penguin-mail.calendar" : "browser"));
    g_variant_builder_add(&hints, "{sv}", "urgency", g_variant_new_byte(1));
    n.pending = true;
    n.retryAt = g_get_monotonic_time() + 3 * G_USEC_PER_SEC;
    n.attempt = ++s->nextAttempt;
    auto call = new Call{s, key, s->generation, n.attempt};
    // Address the unique owner so a restart cannot return an ID from the old
    // service that is accidentally interpreted as one from the new server.
    g_dbus_connection_call(s->bus, s->owner.c_str(), path, service, "Notify",
        g_variant_new("(susssasa{sv}i)", "Dome Browser", n.id,
          "org.domed.Browser", n.data.value("title", "").c_str(),
          n.data.value("body", "").c_str(), &actions, &hints, -1),
        G_VARIANT_TYPE("(u)"), G_DBUS_CALL_FLAGS_NONE, 3000, s->cancel,
        [](GObject *source, GAsyncResult *result, gpointer data) {
          std::unique_ptr<Call> c(static_cast<Call *>(data));
          GError *error = nullptr;
          GVariant *reply = g_dbus_connection_call_finish(G_DBUS_CONNECTION(source), result, &error);
          auto s = c->state.lock();
          if (s && s->generation == c->generation) {
            auto f = s->notices.find(c->key);
            if (f != s->notices.end() && f->second.attempt == c->attempt) {
              f->second.pending = false;
              if (reply) {
                guint id = 0; g_variant_get(reply, "(u)", &id);
                if (id) {
                  f->second.id = id;
                  if (s->delivered) s->delivered(f->second.data);
                }
              }
            } else if (reply) {
              guint id = 0; g_variant_get(reply, "(u)", &id);
              if (id && !s->owner.empty())
                g_dbus_connection_call(s->bus, s->owner.c_str(), path, service,
                  "CloseNotification", g_variant_new("(u)", id), nullptr,
                  G_DBUS_CALL_FLAGS_NONE, 2000, s->cancel, nullptr, nullptr);
            }
          }
          if (reply) g_variant_unref(reply);
          g_clear_error(&error);
        }, call);
  }
  static void retry(void *data) {
    auto self = static_cast<DesktopNotifications *>(data);
    std::vector<std::string> keys;
    for (const auto &n : self->state->notices)
      if (!n.second.id) keys.push_back(n.first);
    for (const auto &key : keys) send(self->state, key);
    Fl::repeat_timeout(3, retry, data);
  }
public:
  DesktopNotifications(std::function<void(const Json &, const std::string &)> chosen,
                       std::function<void(const Json &)> delivered) {
    auto s = state;
    s->chosen = std::move(chosen); s->delivered = std::move(delivered);
    s->bus = g_bus_get_sync(G_BUS_TYPE_SESSION, nullptr, nullptr);
    if (!s->bus) return;
    Fl::add_timeout(3, retry, this);
    s->actionSignal = g_dbus_connection_signal_subscribe(s->bus, service, service,
      "ActionInvoked", path, nullptr, G_DBUS_SIGNAL_FLAGS_NONE,
      [](GDBusConnection *, const gchar *sender, const gchar *, const gchar *,
         const gchar *, GVariant *v, gpointer data) {
        auto s = static_cast<State *>(data);
        if (s->owner != sender || !g_variant_is_of_type(v, G_VARIANT_TYPE("(us)"))) return;
        guint id; const char *action; g_variant_get(v, "(u&s)", &id, &action);
        for (const auto &entry : s->notices) {
          if (entry.second.id != id) continue;
          for (const auto &offered : entry.second.data["actions"])
            if (offered[0] == action) {
              auto notice = entry.second.data;
              auto callback = s->chosen;
              if (callback) callback(notice, action);
              return;
            }
        }
      }, s.get(), nullptr);
    s->closedSignal = g_dbus_connection_signal_subscribe(s->bus, service, service,
      "NotificationClosed", path, nullptr, G_DBUS_SIGNAL_FLAGS_NONE,
      [](GDBusConnection *, const gchar *sender, const gchar *, const gchar *,
         const gchar *, GVariant *v, gpointer data) {
        auto s = static_cast<State *>(data);
        if (s->owner != sender || !g_variant_is_of_type(v, G_VARIANT_TYPE("(uu)"))) return;
        guint id, reason; g_variant_get(v, "(uu)", &id, &reason);
        for (auto it = s->notices.begin(); it != s->notices.end(); ++it)
          if (it->second.id == id) { s->notices.erase(it); break; }
      }, s.get(), nullptr);
    s->watch = g_bus_watch_name_on_connection(s->bus, service, G_BUS_NAME_WATCHER_FLAGS_NONE,
      [](GDBusConnection *, const gchar *, const gchar *owner, gpointer data) {
        auto s = static_cast<std::shared_ptr<State> *>(data);
        (*s)->owner = owner;
        std::vector<std::string> keys;
        for (const auto &n : (*s)->notices) keys.push_back(n.first);
        for (const auto &key : keys) send(*s, key);
      }, [](GDBusConnection *, const gchar *, gpointer data) {
        auto &s = *static_cast<std::shared_ptr<State> *>(data);
        s->owner.clear(); ++s->generation;
        // Only unaccepted deliveries survive a server restart. Old notifications
        // never reappear as a backlog, and stale IDs cannot activate new mail.
        for (auto it = s->notices.begin(); it != s->notices.end();)
          if (it->second.id) it = s->notices.erase(it);
          else { it->second.pending = false; it->second.retryAt = 0; ++it; }
      }, new std::shared_ptr<State>(s), [](gpointer p) { delete static_cast<std::shared_ptr<State> *>(p); });
  }
  ~DesktopNotifications() {
    Fl::remove_timeout(retry, this);
    auto s = state;
    s->chosen = {}; s->delivered = {};
    g_cancellable_cancel(s->cancel);
    if (s->watch) g_bus_unwatch_name(s->watch);
    if (s->actionSignal) g_dbus_connection_signal_unsubscribe(s->bus, s->actionSignal);
    if (s->closedSignal) g_dbus_connection_signal_unsubscribe(s->bus, s->closedSignal);
  }
  void withdraw(const std::string &key) {
    auto &s = state;
    auto it = s->notices.find(key);
    if (it == s->notices.end()) return;
    if (it->second.id && !s->owner.empty())
      g_dbus_connection_call(s->bus, s->owner.c_str(), path, service, "CloseNotification",
        g_variant_new("(u)", it->second.id), nullptr, G_DBUS_CALL_FLAGS_NONE, 2000,
        s->cancel, nullptr, nullptr);
    s->notices.erase(it);
  }
  void withdrawKind(const std::string &kind) {
    std::vector<std::string> keys;
    for (const auto &n : state->notices)
      if (n.second.data.value("kind", "") == kind) keys.push_back(n.first);
    for (const auto &key : keys) withdraw(key);
  }
  void post(const Json &notice) {
    auto key = notice.value("key", "");
    if (key.empty() || !notice.contains("actions")) return;
    auto &s = state;
    auto f = s->notices.find(key);
    if (f != s->notices.end()) {
      // Repeated scheduler checks retry failed delivery, never repost an
      // accepted reminder while its persistence acknowledgement is in flight.
      if (!f->second.id) send(s, key);
      else if (s->delivered) s->delivered(f->second.data);
      return;
    }
    if (s->notices.size() >= 128) return;
    s->notices.emplace(key, Notice{notice});
    send(s, key);
  }
};
