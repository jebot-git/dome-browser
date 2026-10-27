// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once
#include <gio/gdesktopappinfo.h>
#include <cstring>
#include <string>

namespace external_links {
static bool usable(GAppInfo *app, const char *type) {
  if (!app || !g_app_info_supports_uris(app)) return false;
  const char *id = g_app_info_get_id(app);
  if (id && (!strcmp(id,"org.domed.Browser.desktop") || !strcmp(id,"dome-browser.desktop"))) return false;
  auto types = g_app_info_get_supported_types(app);
  if (types) for (auto p=types;*p;++p) if (!strcmp(*p,type)) return true;
  return false;
}
}

// Called on a worker, or before the browser event loop starts. GIO expands the
// selected desktop entry and receives the complete URI as one argument.
extern "C" int browser_launch_default_uri(const char *uri) {
  const char *scheme=uri?g_uri_peek_scheme(uri):nullptr;
  if (!scheme || (g_ascii_strcasecmp(scheme,"mailto") && g_ascii_strcasecmp(scheme,"misfin") && g_ascii_strcasecmp(scheme,"magnet")) || strlen(uri)>65536) return 4;
  const char *selected_scheme=g_ascii_strcasecmp(scheme,"mailto")==0?"mailto":g_ascii_strcasecmp(scheme,"misfin")==0?"misfin":"magnet";
  std::string type=std::string("x-scheme-handler/")+selected_scheme;
  std::string path=std::string(g_get_user_config_dir())+"/dome-mimeapps.list";
  auto preferences=g_key_file_new();GError *error=nullptr;
  if (!g_key_file_load_from_file(preferences,path.c_str(),G_KEY_FILE_NONE,&error)) {
    bool missing=g_error_matches(error,G_FILE_ERROR,G_FILE_ERROR_NOENT);
    g_clear_error(&error);
    if (!missing) {g_key_file_unref(preferences);return 2;}
  }
  GAppInfo *app=nullptr;
  bool configured=g_key_file_has_key(preferences,"Default Applications",type.c_str(),nullptr);
  if (configured) {
    auto ids=g_key_file_get_string_list(preferences,"Default Applications",type.c_str(),nullptr,&error);
    if (error) {g_clear_error(&error);g_key_file_unref(preferences);return 2;}
    if (ids) for (auto p=ids;*p && !app;++p) {
      auto candidate=g_desktop_app_info_new(*p);
      if (candidate) {
        if (external_links::usable(G_APP_INFO(candidate),type.c_str())) app=G_APP_INFO(candidate);
        else g_object_unref(candidate);
      }
    }
    g_strfreev(ids);
  } else {
    app=g_app_info_get_default_for_uri_scheme(selected_scheme);
    if (app && !external_links::usable(app,type.c_str())) {g_object_unref(app);app=nullptr;}
  }
  g_key_file_unref(preferences);
  if (!app) return 1;
  GList item{const_cast<char *>(uri),nullptr,nullptr};
  bool launched=g_app_info_launch_uris(app,&item,nullptr,&error);
  g_clear_error(&error);g_object_unref(app);
  return launched?0:3;
}
