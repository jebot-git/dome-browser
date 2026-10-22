// SPDX-License-Identifier: GPL-2.0-or-later
#pragma once
#include "desktop-menu.h"
#include <FL/Fl.H>
#include <FL/Fl_SVG_Image.H>
#include <functional>
#include <gio/gio.h>
#include <string>
#include <vector>
// StatusNotifierItem extension for FLTK. The real command menu is shared with
// the global menubar; the core audio process does not depend on this companion.
class StatusIcon {
  GDBusConnection *bus = nullptr;
  GDBusNodeInfo *info = nullptr;
  guint registration = 0, watch = 0;
  DesktopMenu &menu;
  std::string title = "dome-browser";
  bool present = false;
  static GVariant *property(GDBusConnection *, const gchar *, const gchar *,
                            const gchar *, const gchar *name, GError **,
                            gpointer data) {
    auto s = (StatusIcon *)data;
    if (!strcmp(name, "Category"))
      return g_variant_new_string("ApplicationStatus");
    if (!strcmp(name, "Id"))
      return g_variant_new_string("dome-browser");
    if (!strcmp(name, "Title"))
      return g_variant_new_string(s->title.c_str());
    if (!strcmp(name, "Status"))
      return g_variant_new_string("Active");
    if (!strcmp(name, "IconName"))
      return g_variant_new_string("dome-browser");
    if (!strcmp(name, "WindowId"))
      return g_variant_new_uint32(0);
    if (!strcmp(name, "ItemIsMenu"))
      return g_variant_new_boolean(false);
    if (!strcmp(name, "Menu"))
      return g_variant_new_object_path(s->menu.objectPath().c_str());
    if (!strcmp(name, "IconPixmap")) {
      unsigned char r, g, b;
      Fl::get_color(FL_FOREGROUND_COLOR, r, g, b);
      char color[8];
      snprintf(color, sizeof(color), "#%02x%02x%02x", r, g, b);
      auto svg = std::string("<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 24 24\"><g fill=\"none\" stroke=\"")+color+"\" stroke-width=\"1.6\"><circle cx=\"12\" cy=\"12\" r=\"10\"/><ellipse cx=\"12\" cy=\"12\" rx=\"5\" ry=\"10\"/><path d=\"M2 12h20M12 2v20\"/></g></svg>";
      Fl_SVG_Image image(nullptr, svg.c_str());
      image.resize(24, 24);
      image.normalize();
      GVariantBuilder icons;
      g_variant_builder_init(&icons, G_VARIANT_TYPE("a(iiay)"));
      if (!image.fail() && image.data() && image.data()[0]) {
        std::vector<unsigned char> argb(24 * 24 * 4);
        auto data = (const unsigned char *)image.data()[0];
        int stride = image.ld() ? image.ld() : 24 * image.d();
        for (int y = 0; y < 24; ++y)
          for (int x = 0; x < 24; ++x) {
            auto p = data + y * stride + x * image.d();
            auto q = argb.data() + (y * 24 + x) * 4;
            q[0] = image.d() == 4 ? p[3] : 255;
            q[1] = p[0];
            q[2] = p[1];
            q[3] = p[2];
          }
        g_variant_builder_add(&icons, "(ii@ay)", 24, 24,
                              g_variant_new_fixed_array(G_VARIANT_TYPE_BYTE,
                                                        argb.data(),
                                                        argb.size(), 1));
      }
      return g_variant_builder_end(&icons);
    }
    return nullptr;
  }
  static void method(GDBusConnection *, const gchar *, const gchar *,
                     const gchar *, const gchar *name, GVariant *parameters,
                     GDBusMethodInvocation *call, gpointer data) {
    auto s = (StatusIcon *)data;
    if (!strcmp(name, "Scroll")) {
      int delta;
      const char *axis;
      g_variant_get(parameters, "(i&s)", &delta, &axis);
      if (s->scroll && delta && !strcmp(axis, "vertical"))
        s->scroll(delta);
    } else if (s->activate)
      s->activate();
    g_dbus_method_invocation_return_value(call, nullptr);
  }
  void registerItem() {
    auto service = g_dbus_connection_get_unique_name(bus);
    g_dbus_connection_call(
        bus, "org.kde.StatusNotifierWatcher", "/StatusNotifierWatcher",
        "org.kde.StatusNotifierWatcher", "RegisterStatusNotifierItem",
        g_variant_new("(s)", service), nullptr, G_DBUS_CALL_FLAGS_NONE, 2000,
        nullptr, nullptr, nullptr);
  }

public:
  std::function<void()> activate;
  std::function<void(int)> scroll;
  explicit StatusIcon(DesktopMenu &exported) : menu(exported) {
    bus = g_bus_get_sync(G_BUS_TYPE_SESSION, nullptr, nullptr);
    if (!bus)
      return;
    const char *xml =
        R"XML(<node><interface name="org.kde.StatusNotifierItem"><property name="Category" type="s" access="read"/><property name="Id" type="s" access="read"/><property name="Title" type="s" access="read"/><property name="Status" type="s" access="read"/><property name="WindowId" type="u" access="read"/><property name="IconName" type="s" access="read"/><property name="IconPixmap" type="a(iiay)" access="read"/><property name="ItemIsMenu" type="b" access="read"/><property name="Menu" type="o" access="read"/><method name="Activate"><arg type="i" direction="in"/><arg type="i" direction="in"/></method><method name="SecondaryActivate"><arg type="i" direction="in"/><arg type="i" direction="in"/></method><method name="ContextMenu"><arg type="i" direction="in"/><arg type="i" direction="in"/></method><method name="Scroll"><arg type="i" direction="in"/><arg type="s" direction="in"/></method><signal name="NewTitle"/><signal name="NewIcon"/><signal name="NewStatus"><arg type="s"/></signal></interface></node>)XML";
    info = g_dbus_node_info_new_for_xml(xml, nullptr);
    static const GDBusInterfaceVTable vtable = {
        method, property, nullptr, {nullptr}};
    registration = g_dbus_connection_register_object(
        bus, "/StatusNotifierItem", info->interfaces[0], &vtable, this, nullptr,
        nullptr);
    watch = g_bus_watch_name_on_connection(
        bus, "org.kde.StatusNotifierWatcher", G_BUS_NAME_WATCHER_FLAGS_NONE,
        [](GDBusConnection *, const gchar *, const gchar *, gpointer data) {
          auto s = (StatusIcon *)data;
          s->present = true;
          s->registerItem();
        },
        [](GDBusConnection *, const gchar *, gpointer data) {
          ((StatusIcon *)data)->present = false;
        },
        this, nullptr);
  }
  ~StatusIcon() {
    if (watch)
      g_bus_unwatch_name(watch);
    if (bus) {
      if (registration)
        g_dbus_connection_unregister_object(bus, registration);
      g_object_unref(bus);
    }
    if (info)
      g_dbus_node_info_unref(info);
  }
  bool available() const { return present; }
  void setTitle(std::string value) {
    if (title == value)
      return;
    title = std::move(value);
    if (bus)
      g_dbus_connection_emit_signal(bus, nullptr, "/StatusNotifierItem",
                                    "org.kde.StatusNotifierItem", "NewTitle",
                                    nullptr, nullptr);
  }
};
