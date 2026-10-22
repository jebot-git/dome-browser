// SPDX-License-Identifier: MIT
#pragma once
#include <FL/Fl_Text_Display.H>
#include <FL/Fl.H>
#include "bdf_font.hpp"
#include <FL/Fl_Browser_.H>
#include <FL/Fl_Choice.H>
#include <FL/Fl_Group.H>
#include <FL/Fl_Image_Surface.H>
#include <FL/Fl_Input_.H>
#include <FL/Fl_Menu_.H>
#include <FL/Fl_RGB_Image.H>
#include <FL/Fl_Tooltip.H>
#include <FL/Fl_Value_Input.H>
#include <FL/Fl_Window.H>
#include <FL/fl_ask.H>
#include <FL/fl_draw.H>
#include <algorithm>
#include <cstdio>
#include <cstdint>
#include <cstdlib>
#include <deque>
#include <filesystem>
#include <fontconfig/fontconfig.h>
#include <fstream>
#include <memory>
#include <nlohmann/json.hpp>
#include <spawn.h>
#include <string>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>
#include <vector>
using Json = nlohmann::json;
extern char **environ;
namespace dx {
inline std::filesystem::path config() {
  auto x = getenv("XDG_CONFIG_HOME");
  auto h = getenv("HOME");
  return std::filesystem::path(x ? x
                                 : (h ? std::string(h) + "/.config" : "/tmp")) /
         "dome/appearance.json";
}
inline bool dome() {
  auto s = getenv("XDG_CURRENT_DESKTOP");
  std::string d = s ? s : "";
  std::transform(d.begin(), d.end(), d.begin(), ::tolower);
  return d.find("dome") != std::string::npos;
}
inline Fl_Font font = FL_HELVETICA;
inline int size = 14;
inline uint64_t font_revision = 0;
inline std::string font_name = "sans";
inline bdf_fonts::Face selected_font;
inline std::deque<std::string> font_names;
inline void style(Fl_Widget *w) {
  w->labelfont(font);
  w->labelsize(size);
  if (auto p = dynamic_cast<Fl_Input_ *>(w)) {
    p->textfont(font);
    p->textsize(size);
  }
  if (auto p = dynamic_cast<Fl_Browser_ *>(w)) {
    p->textfont(font);
    p->textsize(size);
  }
  if (auto p = dynamic_cast<Fl_Value_Input *>(w)) {
    p->textfont(font);
    p->textsize(size);
  }
  if (auto p = dynamic_cast<Fl_Menu_ *>(w)) {
    p->textfont(font);
    p->textsize(size);
  }
  if (auto p = dynamic_cast<Fl_Text_Display *>(w)) { p->textfont(font); p->textsize(size); }
  if (auto g = dynamic_cast<Fl_Group *>(w))
    for (int i = 0; i < g->children(); ++i)
      style(g->child(i));
}
// New standard dialogs are constructed after the last appearance revision.
// Check before FLTK flushes so their first frame already uses the session font.
inline std::vector<std::unique_ptr<Fl_Widget_Tracker>> styled_windows;
inline void style_new_windows(void *) {
  styled_windows.erase(
      std::remove_if(styled_windows.begin(), styled_windows.end(),
                     [](const auto &item) { return item->deleted(); }),
      styled_windows.end());
  for (auto w = Fl::first_window(); w; w = Fl::next_window(w)) {
    bool known =
        std::any_of(styled_windows.begin(), styled_windows.end(),
                    [w](const auto &item) { return item->widget() == w; });
    if (!known) {
      style(w);
      if (getenv("DE_XR_APPEARANCE_TRACE")) fprintf(stderr,"DX_STYLE %s font=%d size=%d color=%u\n",w->label()?w->label():"?",font,size,Fl::get_color(FL_BACKGROUND_COLOR));
      w->redraw();
      styled_windows.push_back(std::make_unique<Fl_Widget_Tracker>(w));
    }
  }
}
inline Fl_Event_Dispatch previous_dispatch = nullptr;
inline int dispatch(int event, Fl_Window *window) {
  if (window && event == FL_SHOW)
    style(window);
  return previous_dispatch ? previous_dispatch(event, window)
                           : Fl::handle_(event, window);
}
inline void enable_menu(Fl_Menu_ &menu, const char *path, bool enabled) {
  int index = menu.find_index(path);
  if (index < 0)
    return;
  int old = menu.mode(index);
  int flags = enabled ? (old & ~FL_MENU_INACTIVE) : (old | FL_MENU_INACTIVE);
  if (old != flags) {
    menu.mode(index, flags);
    menu.redraw();
  }
}
inline bool bitmap(const std::string &family, int pixels, const std::string &style = "") {
  try { bdf_fonts::resolve(family, style, pixels); return true; }
  catch (const std::exception &) { return false; }
}
inline std::string last;
inline std::string font_configuration_revision() {
  const char *file = getenv("FONTCONFIG_FILE");
  if (!file || !*file)
    return {};
  std::error_code error;
  auto modified = std::filesystem::last_write_time(file, error);
  return std::string(file) + (error ? "" : std::to_string(modified.time_since_epoch().count()));
}
inline void appearance(void * = nullptr) {
  try {
      std::ifstream f(config());
      Json j;
      if (f && dome())
        f >> j;
      else if (!dome() || !std::filesystem::exists(config()))
        j = {{"light", false},
             {"accent", "#6babff"},
             {"font", "Unifont / Sans-Serif"},
             {"font_size", 16}};
      else
        return;
      auto snapshot = j.dump() + font_configuration_revision();
      if (snapshot != last) {
        bool light = j.value("light", false);
        auto accent = j.value("accent", std::string("#6babff"));
        auto name = j.value("font", std::string());
        int pixels = j.value("font_size", 14);
        if (accent.size() != 7 || accent[0] != '#' ||
            !std::all_of(accent.begin() + 1, accent.end(),
                         [](unsigned char c) { return std::isxdigit(c); }))
          return;
        unsigned rgb = std::stoul(accent.substr(1), nullptr, 16);
        Fl::background(light ? 229 : 31, light ? 233 : 37, light ? 239 : 45);
        Fl::background2(light ? 255 : 22, light ? 255 : 28, light ? 255 : 35);
        Fl::foreground(light ? 30 : 231, light ? 37 : 235, light ? 46 : 241);
        Fl::set_color(FL_SELECTION_COLOR, rgb >> 16, (rgb >> 8) & 255,
                      rgb & 255);
        auto slash = name.find(" / ");
        auto family = name.substr(0, slash);
        auto fontStyle =
            slash == std::string::npos ? std::string() : name.substr(slash + 3);
        // DomeD can refresh its exported bitmap files during an existing login.
        // Resolve from the refreshed Fontconfig configuration, including faces
        // which were unavailable when this process started.
        static std::string applied_configuration;
        auto configuration = font_configuration_revision();
        if (configuration != applied_configuration) { FcInitReinitialize(); applied_configuration = configuration; }
        bdf_fonts::discover(true);
        bdf_fonts::Face selected;
        int scale = j.value("font_scale", 1);
        try { selected = bdf_fonts::resolve(family, fontStyle, pixels, scale); }
        catch (const std::exception &) {
          try { selected = bdf_fonts::resolve("Unifont", "Sans-Serif", 16, scale); }
          catch (const std::exception &) {
            auto available = bdf_fonts::discover();
            if (available.empty()) throw std::runtime_error("No BDF fonts found in Fontconfig font directories");
            selected = bdf_fonts::scaled(available.front(), scale);
          }
        }
        selected_font = selected;
        font_name = selected.family + " / " + selected.style;
        font_names.push_back(font_name);
        Fl::set_font(FL_FREE_FONT, font_names.back().c_str());
        font = FL_FREE_FONT;
        size = selected.pixels;
        setenv("DE_XR_BDF_PIXELS", std::to_string(size).c_str(), 1);
        FL_NORMAL_SIZE = size;
        ++font_revision;
        fl_message_font(font, size);
        for (auto w = Fl::first_window(); w; w = Fl::next_window(w)) {
          style(w);
          w->redraw();
        }
        Fl_Tooltip::font(font);
        Fl_Tooltip::size(size);
        last = snapshot;
      }
    } catch (const std::exception &) {
    }
}
inline void poll(void *) {
  appearance();
  style_new_windows(nullptr);
  Fl::repeat_timeout(.3, poll);
}
inline void init() {
  Fl::scheme("gtk+");
  Fl::visual(FL_RGB);
  auto font_config = bdf_fonts::export_config();
  setenv("FONTCONFIG_FILE", font_config.c_str(), 1);
  FcInitReinitialize();
  appearance();
  if (dome() && font != FL_FREE_FONT) throw std::runtime_error("de-xr requires an installed Unicode BDF font");
  // Always use native FLTK choosers: GTK, Zenity and KDE are optional.
  Fl::option(Fl::OPTION_FNFC_USES_GTK, false);
  Fl::option(Fl::OPTION_FNFC_USES_ZENITY, false);
  Fl::option(Fl::OPTION_FNFC_USES_KDIALOG, false);
  {
    if (Fl::event_dispatch() != dispatch) {
      previous_dispatch = Fl::event_dispatch();
      Fl::event_dispatch(dispatch);
    }
    Fl::add_check(style_new_windows);
  }
  Fl::add_timeout(.3, poll);
}
inline int spawn(const std::vector<std::string> &args) {
  if (args.empty())
    return -1;
  std::vector<char *> argv;
  for (auto &s : args)
    argv.push_back(const_cast<char *>(s.c_str()));
  argv.push_back(nullptr);
  pid_t pid = -1;
  int error =
      posix_spawnp(&pid, argv[0], nullptr, nullptr, argv.data(), environ);
  return error ? -1 : pid;
}
inline bool screenshot(Fl_Window &w, const char *path) {
  Fl_Image_Surface surface(w.w(), w.h());
  Fl_Surface_Device::push_current(&surface);
  surface.draw(&w);
  auto image = surface.image();
  Fl_Surface_Device::pop_current();
  if (!image)
    return false;
  std::ofstream f(path, std::ios::binary);
  f << "P6\n" << image->w() << " " << image->h() << "\n255\n";
  auto data = image->data()[0];
  int stride = image->ld() ? image->ld() : image->w() * image->d();
  for (int y = 0; y < image->h(); ++y)
    for (int x = 0; x < image->w(); ++x)
      f.write(data + y * stride + x * image->d(), 3);
  delete image;
  return bool(f);
}
inline void smoke(Fl_Window &w, int argc, char **argv) {
  style(&w);
  w.show();
  for (int i = 1; i < argc; ++i)
    if (std::string(argv[i]) == "--smoke-test") {
      Fl::check();
      bool ok = i + 1 < argc && screenshot(w, argv[i + 1]);
      std::printf("{\"toolkit\":\"FLTK\",\"version\":\"%d.%d.%d\",\"font\":\"%"
                  "s\",\"size\":%d}\n",
                  FL_MAJOR_VERSION, FL_MINOR_VERSION, FL_PATCH_VERSION,
                  font_name.c_str(), size);
      std::exit(ok ? 0 : 1);
    }
}
} // namespace dx
