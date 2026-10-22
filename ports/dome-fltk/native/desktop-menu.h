// SPDX-License-Identifier: MIT
#pragma once
#include <FL/Fl_Menu_Bar.H>
#include <FL/Fl_Window.H>
#include <functional>
#include <memory>
#include <string>
// Keep the widget active for shortcuts while the desktop draws its menu.
class DesktopMenuBar : public Fl_Menu_Bar {
  bool external = false;
  int localHeight;
  void draw() override {
    if (!external) Fl_Menu_Bar::draw();
  }
public:
  DesktopMenuBar(int x, int y, int w, int h)
      : Fl_Menu_Bar(x, y, w, h), localHeight(h) {}
  std::function<void()> layoutChanged;
  bool universal() const { return external; }
  void setUniversal(bool value) {
    if (external == value) return;
    external = value;
    resize(x(), y(), w(), value ? 0 : localHeight);
    if (layoutChanged) layoutChanged();
    if (window()) window()->redraw();
  }
  int handle(int event) override {
    if (external) {
      if (event == FL_SHORTCUT) return test_shortcut() != nullptr;
      return 0;
    }
    return Fl_Menu_Bar::handle(event);
  }
};
// Export real FLTK commands over DBusMenu and associate the owning window via
// KDE's native Wayland appmenu protocol (X11 window properties otherwise).
class DesktopMenu {
  struct Impl;
  std::unique_ptr<Impl> impl;

public:
  DesktopMenu(Fl_Window &, DesktopMenuBar &);
  ~DesktopMenu();
  std::string objectPath() const;
  std::string serviceName() const;
  DesktopMenu(const DesktopMenu &) = delete;
  DesktopMenu &operator=(const DesktopMenu &) = delete;
};
