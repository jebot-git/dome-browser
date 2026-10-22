// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once
#include <gio/gio.h>
#include <FL/Fl.H>
#include <nlohmann/json.hpp>
#include <algorithm>
#include <string>

namespace browser_wallet {
using Json = nlohmann::json;
inline GDBusConnection *bus = nullptr;
inline int handle = -1;
constexpr const char *service = "org.kde.kwalletd6";
constexpr const char *object = "/modules/kwalletd6";
constexpr const char *app = "org.domed.Browser";
inline std::string result;
struct Pending { bool done=false; GVariant *value=nullptr; };
inline GVariant *call(const char *method, GVariant *parameters) {
  if (!bus) bus=g_bus_get_sync(G_BUS_TYPE_SESSION,nullptr,nullptr);
  if (!bus) return nullptr;
  Pending pending;
  g_dbus_connection_call(bus,service,object,"org.kde.KWallet",method,parameters,
    nullptr,G_DBUS_CALL_FLAGS_NONE,120000,nullptr,
    [](GObject *connection,GAsyncResult *reply,void *data) {
      auto pending=static_cast<Pending *>(data);
      pending->value=g_dbus_connection_call_finish(G_DBUS_CONNECTION(connection),reply,nullptr);
      pending->done=true;
    }, &pending);
  // Keep the FLTK window responsive while KWallet displays its own unlock prompt.
  while (!pending.done) { while(g_main_context_iteration(nullptr,false)){} Fl::wait(.01); }
  return pending.value;
}
inline bool open() {
  if(handle>=0) {
    if(auto reply=call("isOpen",g_variant_new("(i)",handle))) {
      gboolean yes=false;g_variant_get(reply,"(b)",&yes);g_variant_unref(reply);
      if(yes)return true;
    }
    handle=-1;
  }
  auto reply=call("networkWallet",g_variant_new("()"));
  if(!reply)return false;
  const char *name=nullptr;g_variant_get(reply,"(&s)",&name);std::string wallet=name;g_variant_unref(reply);
  reply=call("open",g_variant_new("(sxs)",wallet.c_str(),gint64(0),app));
  if(!reply)return false;
  g_variant_get(reply,"(i)",&handle);g_variant_unref(reply);return handle>=0;
}
inline Json request(const Json &request) {
  std::string folder=request.at("folder"),operation=request.at("operation");
  if(folder.rfind("Dome Browser ",0)!=0 || !open())return {{"error","KWallet is unavailable or locked"}};
  auto reply=call("hasFolder",g_variant_new("(iss)",handle,folder.c_str(),app));
  gboolean exists=false;
  if(reply){g_variant_get(reply,"(b)",&exists);g_variant_unref(reply);}
  if(!exists && operation!="write")return operation=="list"?Json{{"keys",Json::array()}}:Json{{"value",""}};
  if(!exists) {
    reply=call("createFolder",g_variant_new("(iss)",handle,folder.c_str(),app));
    gboolean created=false;if(reply){g_variant_get(reply,"(b)",&created);g_variant_unref(reply);}
    if(!created)return {{"error","Cannot create the browser wallet folder"}};
  }
  if(operation=="list") {
    reply=call("entryList",g_variant_new("(iss)",handle,folder.c_str(),app));
    if(!reply)return {{"error","Cannot list saved logins"}};
    char **keys=nullptr;g_variant_get(reply,"(^as)",&keys);Json list=Json::array();
    for(char **key=keys;key && *key;++key)list.push_back(*key);
    g_strfreev(keys);g_variant_unref(reply);return {{"keys",list}};
  }
  std::string key=request.at("key");
  if(operation=="read") {
    reply=call("readPassword",g_variant_new("(isss)",handle,folder.c_str(),key.c_str(),app));
    if(!reply)return {{"error","Cannot read the saved login"}};
    const char *value=nullptr;g_variant_get(reply,"(&s)",&value);Json out={{"value",value}};g_variant_unref(reply);return out;
  }
  if(operation=="write" || operation=="remove") {
    std::string value=request.value("value",std::string());
    reply=operation=="write"?call("writePassword",g_variant_new("(issss)",handle,folder.c_str(),key.c_str(),value.c_str(),app)):
      call("removeEntry",g_variant_new("(isss)",handle,folder.c_str(),key.c_str(),app));
    std::fill(value.begin(),value.end(),'\0');
    if(!reply)return {{"error","Cannot update the saved login"}};
    int status=-1;g_variant_get(reply,"(i)",&status);g_variant_unref(reply);
    return status==0?Json{{"ok",true}}:Json{{"error","KWallet rejected the update"}};
  }
  return {{"error","Unknown wallet operation"}};
}
}
extern "C" const char *browser_ui_wallet(const char *text) {
  using namespace browser_wallet;
  std::fill(result.begin(),result.end(),'\0');result.clear();
  try { result=request(Json::parse(text)).dump(); }
  catch(...) { result=R"({"error":"KWallet operation failed"})"; }
  return result.c_str();
}
extern "C" void browser_ui_wallet_clear() {
  std::fill(browser_wallet::result.begin(),browser_wallet::result.end(),'\0');browser_wallet::result.clear();
}
