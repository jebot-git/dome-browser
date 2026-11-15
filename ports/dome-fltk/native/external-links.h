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

// MIME handlers may take local files (%f/%F) or file URIs (%u/%U).
// Resolve the user's selected application, never execute a downloaded path.
static GAppInfo *file_handler(const char *type, int &result) {
  result=1;
  if(!type || !strchr(type,'/') || strlen(type)>255){result=4;return nullptr;}
  std::string path=std::string(g_get_user_config_dir())+"/dome-mimeapps.list";
  auto preferences=g_key_file_new();GError *error=nullptr;
  if(!g_key_file_load_from_file(preferences,path.c_str(),G_KEY_FILE_NONE,&error)){
    bool missing=g_error_matches(error,G_FILE_ERROR,G_FILE_ERROR_NOENT);g_clear_error(&error);
    if(!missing){g_key_file_unref(preferences);result=2;return nullptr;}
  }
  auto usable=[](GAppInfo *app){
    if(!app || (!g_app_info_supports_files(app) && !g_app_info_supports_uris(app)))return false;
    auto id=g_app_info_get_id(app);
    return !id || (strcmp(id,"org.domed.Browser.desktop") && strcmp(id,"dome-browser.desktop"));
  };
  GAppInfo *app=nullptr;
  if(g_key_file_has_key(preferences,"Default Applications",type,nullptr)){
    auto ids=g_key_file_get_string_list(preferences,"Default Applications",type,nullptr,&error);
    if(error){g_clear_error(&error);g_key_file_unref(preferences);result=2;return nullptr;}
    if(ids)for(auto p=ids;*p && !app;++p){
      auto candidate=g_desktop_app_info_new(*p);
      if(candidate){if(usable(G_APP_INFO(candidate)))app=G_APP_INFO(candidate);else g_object_unref(candidate);}
    }
    g_strfreev(ids);
  }else{
    auto content_type=g_content_type_from_mime_type(type);
    app=g_app_info_get_default_for_type(content_type?content_type:type,false);g_free(content_type);
    if(app && !usable(app)){g_object_unref(app);app=nullptr;}
  }
  g_key_file_unref(preferences);
  if(app)result=0;
  return app;
}
extern "C" int browser_launch_default_file(const char *path,const char *mime){
  if(!path || !g_path_is_absolute(path))return 4;
  int result;auto app=file_handler(mime,result);if(!app)return result;
  GError *error=nullptr;auto file=g_file_new_for_path(path);GList item{file,nullptr,nullptr};
  bool launched=g_app_info_launch(app,&item,nullptr,&error);
  g_clear_error(&error);g_object_unref(file);g_object_unref(app);return launched?0:3;
}
