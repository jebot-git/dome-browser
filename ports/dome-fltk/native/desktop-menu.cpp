// SPDX-License-Identifier: MIT
#include "desktop-menu.h"
#include "appearance.h"
#include "appmenu-client.h"
#include <FL/Fl.H>
#include <FL/Fl_Menu_Item.H>
#include <FL/fl_draw.H>
#include <FL/platform.H>
#include <algorithm>
#include <cstring>
#include <gio/gio.h>
#include <map>
#include <string>
#include <vector>
#include <wayland-client.h>
namespace {
constexpr const char *iface = "com.canonical.dbusmenu";
const char *xml = R"XML(<node><interface name="com.canonical.dbusmenu">
<property name="Version" type="u" access="read"/><property name="TextDirection" type="s" access="read"/><property name="Status" type="s" access="read"/><property name="IconThemePath" type="as" access="read"/>
<method name="GetLayout"><arg type="i" direction="in"/><arg type="i" direction="in"/><arg type="as" direction="in"/><arg type="u" direction="out"/><arg type="(ia{sv}av)" direction="out"/></method>
<method name="GetGroupProperties"><arg type="ai" direction="in"/><arg type="as" direction="in"/><arg type="a(ia{sv})" direction="out"/></method>
<method name="GetProperty"><arg type="i" direction="in"/><arg type="s" direction="in"/><arg type="v" direction="out"/></method>
<method name="Event"><arg type="i" direction="in"/><arg type="s" direction="in"/><arg type="v" direction="in"/><arg type="u" direction="in"/></method>
<method name="EventGroup"><arg type="a(isvu)" direction="in"/><arg type="ai" direction="out"/></method>
<method name="AboutToShow"><arg type="i" direction="in"/><arg type="b" direction="out"/></method>
<method name="AboutToShowGroup"><arg type="ai" direction="in"/><arg type="ai" direction="out"/><arg type="ai" direction="out"/></method>
<signal name="LayoutUpdated"><arg type="u"/><arg type="i"/></signal>
<signal name="ItemsPropertiesUpdated"><arg type="a(ia{sv})"/><arg type="a(ias)"/></signal>
</interface></node>)XML";
std::string label(const char *text) {
  std::string result;
  for (size_t i = 0; text && text[i]; ++i) {
    if (text[i] == '&') {
      if (text[i + 1] == '&') {
        result += '&';
        ++i;
      } else
        result += '_';
    } else {
      if (text[i] == '_')
        result += '_';
      result += text[i];
    }
  }
  return result;
}
struct Node {
  int id = 0;
  const Fl_Menu_Item *item = nullptr;
  bool separator = false;
  int parent = 0;
  std::vector<int> children;
};
} // namespace
struct DesktopMenu::Impl {
  Fl_Window &window;
  DesktopMenuBar &menu;
  GDBusConnection *bus = nullptr;
  GDBusNodeInfo *info = nullptr;
  guint registration = 0;
  guint registrarWatch = 0;
  bool registrarAvailable = false;
  uint32_t revision = 1;
  std::string path, signature;
  const Fl_Menu_Item *menuStorage = nullptr;
  std::map<int, Node> nodes;
  wl_registry *registry = nullptr;
  org_kde_kwin_appmenu_manager *manager = nullptr;
  org_kde_kwin_appmenu *surface = nullptr;
  bool attached = false;
  wl_surface *attachedSurface = nullptr;
  unsigned long attachedXid = 0;
  int nextId = 1;
  Impl(Fl_Window &w, DesktopMenuBar &m) : window(w), menu(m) {
    static int count = 0;
    path = "/org/de_xr/FLTK/Menu" + std::to_string(++count);
    GError *error = nullptr;
    bus = g_bus_get_sync(G_BUS_TYPE_SESSION, nullptr, &error);
    if (error)
      g_error_free(error);
    if (!bus)
      return;
    info = g_dbus_node_info_new_for_xml(xml, nullptr);
    static const GDBusInterfaceVTable vtable = {
        method, property, nullptr, {nullptr}};
    registration = g_dbus_connection_register_object(
        bus, path.c_str(), info->interfaces[0], &vtable, this, nullptr,
        nullptr);
    registrarWatch = g_bus_watch_name_on_connection(
        bus, "com.canonical.AppMenu.Registrar", G_BUS_NAME_WATCHER_FLAGS_NONE,
        [](GDBusConnection *, const gchar *, const gchar *, gpointer data) {
          auto self = static_cast<Impl *>(data);
          self->registrarAvailable = true;
          self->updateVisibility();
        },
        [](GDBusConnection *, const gchar *, gpointer data) {
          auto self = static_cast<Impl *>(data);
          self->registrarAvailable = false;
          self->updateVisibility();
        }, this, nullptr);
    rebuild();
    Fl::add_timeout(.05, tick, this);
  }
  ~Impl() {
    Fl::remove_timeout(tick, this);
    if (registrarWatch) g_bus_unwatch_name(registrarWatch);
    if (surface)
      org_kde_kwin_appmenu_release(surface);
    if (manager) {
      if (org_kde_kwin_appmenu_manager_get_version(manager) >= 2)
        org_kde_kwin_appmenu_manager_release(manager);
      else
        org_kde_kwin_appmenu_manager_destroy(manager);
    }
    if (registry)
      wl_registry_destroy(registry);
    if (bus) {
      if (registration)
        g_dbus_connection_unregister_object(bus, registration);
      g_object_unref(bus);
    }
    if (info)
      g_dbus_node_info_unref(info);
  }
  static void tick(void *p) {
    auto self = (Impl *)p;
    for (int n = 0; n < 16 && g_main_context_pending(nullptr); ++n)
      g_main_context_iteration(nullptr, FALSE);
    self->attach();
    self->updateVisibility();
    self->rebuild();
    Fl::repeat_timeout(.1, tick, p);
  }
  void updateVisibility() {
    // A desktop name alone is insufficient: a working menu host and window
    // association must be present before the local commands leave the window.
    bool exported = registration && (attached || (!window.shown() && menu.universal()));
    menu.setUniversal(dx::dome() && registrarAvailable && exported);
  }
  void attach() {
    if (!window.shown()) {
      if (surface) {
        org_kde_kwin_appmenu_release(surface);
        surface = nullptr;
      }
      attached = false;
      attachedSurface = nullptr;
      attachedXid = 0;
      return;
    }
    const char *service = g_dbus_connection_get_unique_name(bus);
    if (!service)
      return;
#ifdef FLTK_USE_WAYLAND
    if (auto display = fl_wl_display()) {
      if (!registry) {
        registry = wl_display_get_registry(display);
        static const wl_registry_listener listener = {
            [](void *data, wl_registry *registry, uint32_t name,
               const char *interface, uint32_t version) {
              auto s = (Impl *)data;
              if (!strcmp(interface, "org_kde_kwin_appmenu_manager"))
                s->manager = (org_kde_kwin_appmenu_manager *)wl_registry_bind(
                    registry, name, &org_kde_kwin_appmenu_manager_interface,
                    std::min(version, 2u));
            },
            [](void *, wl_registry *, uint32_t) {}};
        wl_registry_add_listener(registry, &listener, this);
        wl_display_roundtrip(display);
      }
      auto xid = fl_wl_xid(&window);
      if (manager && xid) {
        auto wl_surface = fl_wl_surface(xid);
        if (wl_surface && wl_surface != attachedSurface) {
          if (surface)
            org_kde_kwin_appmenu_release(surface);
          surface = org_kde_kwin_appmenu_manager_create(manager, wl_surface);
          attachedSurface = wl_surface;
          org_kde_kwin_appmenu_set_address(surface, service, path.c_str());
          wl_display_flush(display);
          attached = true;
        }
      }
      return;
    }
#endif
#ifdef FLTK_USE_X11
    auto display = fl_x11_display();
    auto xid = fl_x11_xid(&window);
    if (display && xid && xid != attachedXid) {
      attachedXid = xid;
      auto utf8 = XInternAtom(display, "UTF8_STRING", False);
      for (auto value : std::vector<std::pair<const char *, std::string>>{
               {"_KDE_NET_WM_APPMENU_SERVICE_NAME", service},
               {"_KDE_NET_WM_APPMENU_OBJECT_PATH", path}}) {
        auto atom = XInternAtom(display, value.first, False);
        XChangeProperty(display, xid, atom, utf8, 8, PropModeReplace,
                        (const unsigned char *)value.second.data(),
                        value.second.size());
      }
      XFlush(display);
      attached = true;
    }
#endif
  }
  std::vector<int> parse(const Fl_Menu_Item *items, int &position, int bound,
                         int parent = 0) {
    std::vector<int> result;
    while (position < bound && items[position].text) {
      auto item = &items[position++];
      int id = nextId++;
      Node node;
      node.id = id;
      node.item = item;
      node.parent = parent;
      if (item->flags & FL_SUBMENU) {
        node.children = parse(items, position, bound, id);
      } else if (item->flags & FL_SUBMENU_POINTER) {
        int sub = 0;
        auto pointer = (const Fl_Menu_Item *)item->user_data();
        if (pointer)
          node.children = parse(pointer, sub, 4096, id);
      }
      nodes[id] = node;
      result.push_back(id);
      if (item->flags & FL_MENU_DIVIDER) {
        Node separator;
        separator.id = nextId++;
        separator.separator = true;
        nodes[separator.id] = separator;
        result.push_back(separator.id);
      }
    }
    if (position < bound)
      ++position;
    return result;
  }
  void rebuild() {
    std::string next;
    auto items = menu.menu();
    int count = menu.size();
    for (int i = 0; items && i < count; ++i) {
      auto &item = items[i];
      next += (item.text ? item.text : "");
      next += std::to_string(item.flags) + ":" +
              std::to_string(item.shortcut_) + ";";
    }
    if (next == signature && items == menuStorage && !nodes.empty())
      return;
    signature = next;
    menuStorage = items;
    nodes.clear();
    nextId = 1;
    Node root;
    root.id = 0;
    int i = 0;
    if (items)
      root.children = parse(items, i, count);
    nodes[0] = root;
    ++revision;
    if (registration)
      g_dbus_connection_emit_signal(
          bus, nullptr, path.c_str(), iface, "LayoutUpdated",
          g_variant_new("(ui)", revision, 0), nullptr);
  }
  bool usable(int id, bool visibleOnly = false) const {
    for (int depth = 0; id && depth < 64; ++depth) {
      auto it = nodes.find(id);
      if (it == nodes.end() || !it->second.item)
        return false;
      auto item = it->second.item;
      if (!item->visible() || (!visibleOnly && !item->active()))
        return false;
      id = it->second.parent;
    }
    return id == 0;
  }
  GVariant *props(int id) {
    GVariantBuilder b;
    g_variant_builder_init(&b, G_VARIANT_TYPE("a{sv}"));
    auto it = nodes.find(id);
    if (it == nodes.end())
      return g_variant_builder_end(&b);
    auto &node = it->second;
    if (id == 0) {
      g_variant_builder_add(&b, "{sv}", "children-display",
                            g_variant_new_string("submenu"));
      return g_variant_builder_end(&b);
    }
    if (node.separator) {
      g_variant_builder_add(&b, "{sv}", "type",
                            g_variant_new_string("separator"));
      return g_variant_builder_end(&b);
    }
    auto item = node.item;
    g_variant_builder_add(&b, "{sv}", "label",
                          g_variant_new_string(label(item->text).c_str()));
    g_variant_builder_add(&b, "{sv}", "enabled",
                          g_variant_new_boolean(usable(id)));
    g_variant_builder_add(&b, "{sv}", "visible",
                          g_variant_new_boolean(usable(id, true)));
    if (!node.children.empty())
      g_variant_builder_add(&b, "{sv}", "children-display",
                            g_variant_new_string("submenu"));
    if (item->flags & (FL_MENU_TOGGLE | FL_MENU_RADIO)) {
      g_variant_builder_add(&b, "{sv}", "toggle-type",
                            g_variant_new_string(item->flags & FL_MENU_RADIO
                                                     ? "radio"
                                                     : "checkmark"));
      g_variant_builder_add(&b, "{sv}", "toggle-state",
                            g_variant_new_int32(item->value() ? 1 : 0));
    }
    if (item->shortcut_) {
      GVariantBuilder groups, keys;
      g_variant_builder_init(&groups, G_VARIANT_TYPE("aas"));
      g_variant_builder_init(&keys, G_VARIANT_TYPE("as"));
      int key = item->shortcut_;
      if (key & FL_CTRL)
        g_variant_builder_add(&keys, "s", "Control");
      if (key & FL_ALT)
        g_variant_builder_add(&keys, "s", "Alt");
      if (key & FL_SHIFT)
        g_variant_builder_add(&keys, "s", "Shift");
      if (key & FL_META)
        g_variant_builder_add(&keys, "s", "Super");
      auto name = std::string(fl_shortcut_label(key & 0xffff));
      if (name == "Enter")
        name = "Return";
      g_variant_builder_add(&keys, "s", name.c_str());
      g_variant_builder_add_value(&groups, g_variant_builder_end(&keys));
      g_variant_builder_add(&b, "{sv}", "shortcut",
                            g_variant_builder_end(&groups));
    }
    return g_variant_builder_end(&b);
  }
  GVariant *layout(int id, int depth) {
    GVariantBuilder children;
    g_variant_builder_init(&children, G_VARIANT_TYPE("av"));
    auto it = nodes.find(id);
    if (it != nodes.end() && depth != 0)
      for (int child : it->second.children)
        g_variant_builder_add(&children, "v",
                              layout(child, depth < 0 ? -1 : depth - 1));
    return g_variant_new("(i@a{sv}@av)", id, props(id),
                         g_variant_builder_end(&children));
  }
  void click(int id, const char *event) {
    auto it = nodes.find(id);
    if (strcmp(event, "clicked") || it == nodes.end() || !it->second.item ||
        !it->second.children.empty())
      return;
    auto item = it->second.item;
    if (usable(id))
      menu.picked(item);
  }
  static GVariant *property(GDBusConnection *, const gchar *, const gchar *,
                            const gchar *, const gchar *name, GError **,
                            gpointer) {
    if (!strcmp(name, "Version"))
      return g_variant_new_uint32(4);
    if (!strcmp(name, "TextDirection"))
      return g_variant_new_string("ltr");
    if (!strcmp(name, "Status"))
      return g_variant_new_string("normal");
    if (!strcmp(name, "IconThemePath"))
      return g_variant_new_strv(nullptr, 0);
    return nullptr;
  }
  static void method(GDBusConnection *, const gchar *, const gchar *,
                     const gchar *, const gchar *name, GVariant *params,
                     GDBusMethodInvocation *call, gpointer data) {
    auto s = (Impl *)data;
    s->rebuild();
    if (!strcmp(name, "GetLayout")) {
      int id, depth;
      GVariant *names;
      g_variant_get(params, "(ii@as)", &id, &depth, &names);
      g_variant_unref(names);
      g_dbus_method_invocation_return_value(
          call,
          g_variant_new("(u@(ia{sv}av))", s->revision, s->layout(id, depth)));
    } else if (!strcmp(name, "GetGroupProperties")) {
      GVariant *ids;
      GVariant *names;
      g_variant_get(params, "(@ai@as)", &ids, &names);
      GVariantBuilder b;
      g_variant_builder_init(&b, G_VARIANT_TYPE("a(ia{sv})"));
      if (!g_variant_n_children(ids)) {
        for (auto &kv : s->nodes)
          g_variant_builder_add(&b, "(i@a{sv})", kv.first, s->props(kv.first));
      } else {
        GVariantIter it;
        g_variant_iter_init(&it, ids);
        int id;
        while (g_variant_iter_next(&it, "i", &id))
          if (s->nodes.count(id))
            g_variant_builder_add(&b, "(i@a{sv})", id, s->props(id));
      }
      g_variant_unref(ids);
      g_variant_unref(names);
      g_dbus_method_invocation_return_value(
          call, g_variant_new("(@a(ia{sv}))", g_variant_builder_end(&b)));
    } else if (!strcmp(name, "GetProperty")) {
      int id;
      const char *key;
      g_variant_get(params, "(i&s)", &id, &key);
      auto props = g_variant_ref_sink(s->props(id));
      auto value = g_variant_lookup_value(props, key, nullptr);
      g_variant_unref(props);
      if (value) {
        g_dbus_method_invocation_return_value(call,
                                              g_variant_new("(v)", value));
        g_variant_unref(value);
      } else
        g_dbus_method_invocation_return_dbus_error(
            call, "com.canonical.dbusmenu.Error.InvalidProperty",
            "No such menu property");
    } else if (!strcmp(name, "Event")) {
      int id;
      const char *event;
      GVariant *payload;
      uint32_t stamp;
      g_variant_get(params, "(i&s@vu)", &id, &event, &payload, &stamp);
      s->click(id, event);
      g_variant_unref(payload);
      g_dbus_method_invocation_return_value(call, nullptr);
    } else if (!strcmp(name, "EventGroup")) {
      GVariant *events;
      g_variant_get(params, "(@a(isvu))", &events);
      GVariantIter it;
      g_variant_iter_init(&it, events);
      GVariant *row;
      while ((row = g_variant_iter_next_value(&it))) {
        int id;
        const char *event;
        GVariant *value;
        uint32_t stamp;
        g_variant_get(row, "(i&s@vu)", &id, &event, &value, &stamp);
        s->click(id, event);
        g_variant_unref(value);
        g_variant_unref(row);
      }
      g_variant_unref(events);
      g_dbus_method_invocation_return_value(
          call, g_variant_new("(@ai)", g_variant_new_array(G_VARIANT_TYPE_INT32,
                                                           nullptr, 0)));
    } else if (!strcmp(name, "AboutToShow"))
      g_dbus_method_invocation_return_value(call, g_variant_new("(b)", false));
    else if (!strcmp(name, "AboutToShowGroup"))
      g_dbus_method_invocation_return_value(
          call,
          g_variant_new("(@ai@ai)",
                        g_variant_new_array(G_VARIANT_TYPE_INT32, nullptr, 0),
                        g_variant_new_array(G_VARIANT_TYPE_INT32, nullptr, 0)));
    else
      g_dbus_method_invocation_return_dbus_error(
          call, "org.freedesktop.DBus.Error.UnknownMethod",
          "Unknown menu method");
  }
};
DesktopMenu::DesktopMenu(Fl_Window &w, DesktopMenuBar &m)
    : impl(std::make_unique<Impl>(w, m)) {}
DesktopMenu::~DesktopMenu() = default;

std::string DesktopMenu::objectPath() const { return impl->path; }
std::string DesktopMenu::serviceName() const {
  auto service =
      impl->bus ? g_dbus_connection_get_unique_name(impl->bus) : nullptr;
  return service ? service : "";
}
