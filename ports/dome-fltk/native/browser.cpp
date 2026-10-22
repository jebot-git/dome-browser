// SPDX-License-Identifier: GPL-3.0-or-later
#include "browser.h"
#include "appearance.h"
#include "desktop-menu.h"
#include "status-icon.h"
#include "notifications.h"
#include "wallet.h"
#include <FL/Fl_Double_Window.H>
#include <FL/Fl_Button.H>
#include <FL/Fl_Input.H>
#include <FL/Fl_Choice.H>
#include <FL/Fl_Box.H>
#include <FL/Fl_Menu_Button.H>
#include <FL/Fl_Native_File_Chooser.H>
#include <FL/Fl_Color_Chooser.H>
#include <FL/Fl_Copy_Surface.H>
#include <FL/fl_draw.H>
#include <algorithm>
#include <cstring>
#include <deque>
#include <memory>
#include <vector>

namespace {
std::deque<BrowserEvent> events;
std::string menuLabel(const std::string &text) {
  std::string label;
  for(char c:text) {
    if(c=='/' || c=='\\')label+='\\';
    if(c=='&')label+='&';
    label+=c;
  }
  return label;
}
void emit(int kind, int a=0, int b=0, int c=0, int d=0, const char *text="") {
  BrowserEvent e{kind,a,b,c,d,{}};
  std::snprintf(e.text,sizeof e.text,"%s",text?text:"");
  if (events.size()>512 && (kind==12 || kind==10)) return;
  events.push_back(e);
}
class LocationInput : public Fl_Input {
  std::string completion;
public:
  using Fl_Input::Fl_Input;
  std::vector<std::string> history;
  const char *target() const { return completion.empty()?value():completion.c_str(); }
  void setUrl(const char *url) { completion.clear(); value(url); }
  int handle(int event) override {
    if(event==FL_KEYDOWN && Fl::event_state(FL_CTRL)) {
      int key=Fl::event_key();
      if(key=='l'){completion.clear();insert_position(0,size());return 1;}
      if(key=='t'||key=='w'||key=='q'||key=='r'||key=='d'||key=='o'||key=='s'||key=='h'||key==FL_Tab)return 0;
    }
    if(event==FL_PASTE)completion.clear();
    const bool edit=event==FL_KEYDOWN && Fl::event_key()!=FL_Enter && Fl::event_key()!=FL_KP_Enter;
    if(edit)completion.clear();
    int result=Fl_Input::handle(event);
    if(edit && !Fl::event_state(FL_CTRL|FL_ALT|FL_META) && Fl::event_length()>0 &&
       Fl::event_key()!=FL_BackSpace && Fl::event_key()!=FL_Delete && position()==size() && mark()==position()) {
      const std::string prefix=value();
      if(!prefix.empty()) {
        // An exact URL wins over a longer prefix match. This also lets users
        // type a site's root URL without being forced onto its previous path.
        for(int pass=0;pass<2;++pass)for(const auto &url:history) {
          std::string shown=url;
          if(prefix.find("://")==std::string::npos && prefix.rfind("file:",0)!=0 && prefix.rfind("about:",0)!=0) {
            if(shown.rfind("https://",0)==0)shown.erase(0,8);
            else if(shown.rfind("http://",0)==0)shown.erase(0,7);
          }
          if(shown.rfind(prefix,0)==0 && ((pass==0)==(shown==prefix))) {
            completion=url;value(shown.c_str());insert_position(prefix.size(),shown.size());return result;
          }
        }
      }
    }
    return result;
  }
};
class Page : public Fl_Widget {
  std::vector<uint8_t> pixels;
  unsigned pw=0,ph=0;
  void draw() override {
    fl_push_clip(x(),y(),w(),h());
    fl_color(FL_BACKGROUND2_COLOR); fl_rectf(x(),y(),w(),h());
    if (!pixels.empty()) { Fl_RGB_Image image(pixels.data(),pw,ph,4); image.draw(x(),y()); }
    fl_pop_clip();
  }
public:
  Page(int x,int y,int w,int h):Fl_Widget(x,y,w,h){}
  void frame(const uint8_t *p,unsigned width,unsigned height) {
    if (!p || width>8192 || height>8192) return;
    pixels.assign(p,p+size_t(width)*height*4); pw=width;ph=height;redraw();
  }
  void resize(int x,int y,int w,int h) override {
    bool changed=w!=this->w() || h!=this->h();
    Fl_Widget::resize(x,y,w,h);
    if (changed) emit(10,std::max(1,w),std::max(1,h));
  }
  int handle(int event) override {
    int px=Fl::event_x()-x(),py=Fl::event_y()-y();
    switch(event) {
      case FL_ENTER: return 1;
      case FL_FOCUS: emit(9,1); return 1;
      case FL_UNFOCUS: emit(9,0); return 1;
      case FL_PUSH: take_focus(); emit(11,1,Fl::event_button(),px,py); return 1;
      case FL_RELEASE: emit(11,0,Fl::event_button(),px,py); return 1;
      case FL_MOVE: case FL_DRAG: emit(12,px,py,Fl::event_state()); return 1;
      case FL_MOUSEWHEEL: emit(13,Fl::event_dx(),Fl::event_dy(),px,py); return 1;
      case FL_KEYDOWN: case FL_KEYUP:
        if (event==FL_KEYDOWN) {
          int key=Fl::event_key();
          if(key==FL_Insert && Fl::event_state(FL_SHIFT)){emit(40);return 1;}
          if ((Fl::event_state(FL_CTRL) && (key=='l'||key=='t'||key=='w'||key=='q'||key=='r'||key=='s'||key=='d'||key=='o'||key=='h'||key==FL_Tab||key=='+'||key=='='||key=='-'||key=='0')) ||
              (Fl::event_state(FL_ALT) && (key==FL_Left||key==FL_Right)) || key==FL_F+11) return 0;
          if(key==FL_Escape)emit(16,0);
        }
        emit(14,event==FL_KEYDOWN,Fl::event_key(),Fl::event_state(),Fl::event_original_key(),Fl::event_text());return 1;
      default:return Fl_Widget::handle(event);
    }
  }
};
class Browser : public Fl_Double_Window {
public:
  DesktopMenuBar *menu;
  LocationInput *address;
  Fl_Choice *tabs;
  Fl_Button *back,*forward,*reload,*newTab,*closeTab;
  Fl_Box *status;
  Page *page;
  std::unique_ptr<DesktopMenu> exported;
  std::unique_ptr<StatusIcon> tray;
  std::unique_ptr<DesktopNotifications> notifications;
  bool fullscreenMode=false;
  Browser():Fl_Double_Window(1280,860,"Dome Browser") {
    xclass("org.domed.Browser");size_range(640,420);
    menu=new DesktopMenuBar(0,0,w(),30);
    auto command=[&](const char *label,int shortcut,int kind,int arg=0) {
      menu->add(label,shortcut,[](Fl_Widget *,void *p){auto value=static_cast<std::pair<int,int> *>(p);emit(value->first,value->second);},new std::pair<int,int>(kind,arg));
    };
    command("File/New Tab",FL_CTRL+'t',5);
    command("File/Close Tab",FL_CTRL+'w',6);
    command("File/Save Page",FL_CTRL+'s',41);
    command("File/Save Session",FL_CTRL+FL_SHIFT+'s',30);
    command("File/Restore Saved Session",FL_CTRL+FL_SHIFT+'r',31);
    command("File/Quit",FL_CTRL+'q',8);
    menu->add("Edit/Location",FL_CTRL+'l',[](Fl_Widget *,void *p){auto b=static_cast<Browser *>(p);b->address->take_focus();b->address->insert_position(0,b->address->size());},this);
    command("Edit/Cut",FL_CTRL+'x',19,0);command("Edit/Copy",FL_CTRL+'c',19,1);command("Edit/Paste",FL_CTRL+'v',19,2);command("Edit/Select All",FL_CTRL+'a',19,3);
    command("View/Back",FL_ALT+FL_Left,2);command("View/Forward",FL_ALT+FL_Right,3);command("View/Reload",FL_CTRL+'r',4);
    command("View/Zoom In",FL_CTRL+'+',17,1);command("View/Zoom Out",FL_CTRL+'-',17,-1);command("View/Actual Size",FL_CTRL+'0',17,0);
    menu->add("View/Full Screen",FL_F+11,[](Fl_Widget *,void *p){auto b=static_cast<Browser *>(p);browser_ui_fullscreen(!b->fullscreenMode);},this);
    command("Bookmarks/Bookmark This Page",FL_CTRL+'d',32);
    command("Bookmarks/Open Bookmark",FL_CTRL+FL_SHIFT+'o',33);
    command("Bookmarks/Import bm.txt",0,34);
    command("Bookmarks/Remove Bookmark",0,35);
    command("History/Open History",FL_CTRL+'h',36);
    command("History/Clear History",0,37);
    command("Privacy/Ad blocking/Enable or disable",0,50);
    command("Privacy/Ad blocking/Toggle exception for this site",0,51);
    command("Privacy/Ad blocking/Reload lists and custom rules",0,52);
    command("Privacy/Ad blocking/Update official lists",0,53);
    command("Privacy/Ad blocking/Status",0,54);
    command("Privacy/Passwords/Enable or disable KWallet",0,60);
    command("Privacy/Passwords/Forget logins for this site",0,61);
    command("Window/Next Tab",FL_CTRL+FL_Tab,7,-1);
    command("Help/WebXR Demo",0,21);
    menu->add("Help/About",0,[](Fl_Widget *,void *){fl_message("Dome Browser\nServo engine with native FLTK 1.4.5 controls\nWebXR shares DomeD's existing XR session.");});
    back=new Fl_Button(8,38,82,34,"Back");forward=new Fl_Button(96,38,98,34,"Forward");reload=new Fl_Button(200,38,90,34,"Reload");
    back->callback([](Fl_Widget *,void *){emit(2);});forward->callback([](Fl_Widget *,void *){emit(3);});reload->callback([](Fl_Widget *,void *){emit(4);});
    address=new LocationInput(298,38,w()-306,34);address->tooltip("Address or search (Ctrl+L)");address->when(FL_WHEN_ENTER_KEY_ALWAYS);address->callback([](Fl_Widget *w,void *p){emit(1,0,0,0,0,static_cast<LocationInput *>(w)->target());static_cast<Browser *>(p)->page->take_focus();},this);
    tabs=new Fl_Choice(8,80,w()-212,32);tabs->tooltip("Open tabs");tabs->callback([](Fl_Widget *w,void *){emit(7,static_cast<Fl_Choice *>(w)->value());});
    newTab=new Fl_Button(w()-196,80,88,32,"New Tab");newTab->callback([](Fl_Widget *,void *){emit(5);});closeTab=new Fl_Button(w()-100,80,92,32,"Close");closeTab->callback([](Fl_Widget *,void *){emit(6);});
    page=new Page(0,120,w(),h()-148);status=new Fl_Box(8,h()-25,w()-16,22,"Starting Servo…");status->align(FL_ALIGN_LEFT|FL_ALIGN_INSIDE|FL_ALIGN_CLIP);
    end();resizable(page);
    callback([](Fl_Widget *,void *){emit(8);});
    menu->layoutChanged=[this]{layout();};
    exported=std::make_unique<DesktopMenu>(*this,*menu);
    tray=std::make_unique<StatusIcon>(*exported);tray->setTitle("Dome Browser");tray->activate=[this]{show();page->take_focus();};
    notifications=std::make_unique<DesktopNotifications>([this](const Json &n,const std::string &){show();emit(22,n.value("tab",0));},[](const Json &){});
    dx::style(this);
  }
  void layout() {
    // FLTK group/fullscreen resizing may change child geometry. Derive the
    // reserved height from host state, never from that mutable geometry.
    int menuHeight=menu->universal()?0:30;
    int top=fullscreenMode?0:menuHeight;
    for(auto widget:std::vector<Fl_Widget *>{address,tabs,back,forward,reload,newTab,closeTab,status}) { if(fullscreenMode)widget->hide();else widget->show(); }
    menu->show();menu->resize(0,0,w(),fullscreenMode?0:menuHeight);
    if(fullscreenMode){page->resize(0,0,w(),h());redraw();return;}
    menu->resize(0,0,w(),menuHeight);
    int xx=8;for(auto button:{back,forward,reload}) {fl_font(button->labelfont(),button->labelsize());int bw=std::max(76,int(fl_width(button->label()))+24);button->resize(xx,top+8,bw,34);xx+=bw+6;}
    address->resize(xx+2,top+8,std::max(100,w()-xx-10),34);
    tabs->resize(8,top+50,w()-212,32);newTab->resize(w()-196,top+50,88,32);closeTab->resize(w()-100,top+50,92,32);
    page->resize(0,top+90,w(),std::max(1,h()-top-118));status->resize(8,h()-25,w()-16,22);redraw();
  }
  void resize(int x,int y,int w,int h) override {Fl_Double_Window::resize(x,y,w,h);layout();}
};
std::unique_ptr<Browser> browser;
GDBusConnection *applicationBus=nullptr;
GDBusNodeInfo *applicationInfo=nullptr;
guint applicationObject=0;
std::string returned;
uint64_t revision=0;
}
extern "C" int browser_ui_open(const char *url) {
  applicationBus=g_bus_get_sync(G_BUS_TYPE_SESSION,nullptr,nullptr);
  if(applicationBus) {
    auto reply=g_dbus_connection_call_sync(applicationBus,"org.freedesktop.DBus","/org/freedesktop/DBus","org.freedesktop.DBus","RequestName",g_variant_new("(su)","org.domed.Browser",4u),G_VARIANT_TYPE("(u)"),G_DBUS_CALL_FLAGS_NONE,2000,nullptr,nullptr);
    guint ownership=0;if(reply){g_variant_get(reply,"(u)",&ownership);g_variant_unref(reply);}
    if(ownership==3) {
      GVariantBuilder options;g_variant_builder_init(&options,G_VARIANT_TYPE("a{sv}"));
      GVariant *parameters=nullptr;const char *method="Activate";
      if(url && *url && strcmp(url,"about:blank")) {method="Open";const char *uris[]={url,nullptr};parameters=g_variant_new("(^asa{sv})",uris,&options);}
      else parameters=g_variant_new("(a{sv})",&options);
      auto result=g_dbus_connection_call_sync(applicationBus,"org.domed.Browser","/org/domed/Browser","org.freedesktop.Application",method,parameters,nullptr,G_DBUS_CALL_FLAGS_NONE,3000,nullptr,nullptr);
      if(result)g_variant_unref(result);
      g_object_unref(applicationBus);applicationBus=nullptr;return 0;
    }
    const char *xml=R"XML(<node><interface name="org.freedesktop.Application"><method name="Activate"><arg type="a{sv}" direction="in"/></method><method name="Open"><arg type="as" direction="in"/><arg type="a{sv}" direction="in"/></method><method name="ActivateAction"><arg type="s" direction="in"/><arg type="av" direction="in"/><arg type="a{sv}" direction="in"/></method></interface></node>)XML";
    applicationInfo=g_dbus_node_info_new_for_xml(xml,nullptr);
    static const GDBusInterfaceVTable vtable={[](GDBusConnection *,const gchar *,const gchar *,const gchar *,const gchar *method,GVariant *args,GDBusMethodInvocation *call,gpointer){
      if(browser){browser->show();if(!strcmp(method,"Open")){auto uris=g_variant_get_child_value(args,0);GVariantIter iter;g_variant_iter_init(&iter,uris);const char *uri;while(g_variant_iter_next(&iter,"&s",&uri))emit(5,0,0,0,0,uri);g_variant_unref(uris);}}
      g_dbus_method_invocation_return_value(call,nullptr);
    },nullptr,nullptr,{nullptr}};
    applicationObject=g_dbus_connection_register_object(applicationBus,"/org/domed/Browser",applicationInfo->interfaces[0],&vtable,nullptr,nullptr,nullptr);
  }
  Fl::lock();dx::init();browser=std::make_unique<Browser>();browser->show();browser->layout();return 1;
}
extern "C" void browser_ui_wait(double seconds) {
  while(g_main_context_iteration(nullptr,false)){}
  Fl::wait(seconds);
  if(revision!=dx::font_revision){revision=dx::font_revision;browser->layout();emit(15,browser_ui_light());}
}
extern "C" int browser_ui_poll(BrowserEvent *e) {if(events.empty())return 0;*e=events.front();events.pop_front();return 1;}
extern "C" void browser_ui_wake(){Fl::awake();}
extern "C" void browser_ui_close(){browser.reset();if(applicationBus){if(applicationObject)g_dbus_connection_unregister_object(applicationBus,applicationObject);g_object_unref(applicationBus);applicationBus=nullptr;}if(applicationInfo){g_dbus_node_info_unref(applicationInfo);applicationInfo=nullptr;}}
extern "C" void browser_ui_frame(const uint8_t *p,unsigned w,unsigned h){browser->page->frame(p,w,h);}
extern "C" void browser_ui_title(const char *title,const char *url,int back,int forward){browser->copy_label((std::string(title)+" — Dome Browser").c_str());if(Fl::focus()!=browser->address)browser->address->setUrl(url);if(back)browser->back->activate();else browser->back->deactivate();if(forward)browser->forward->activate();else browser->forward->deactivate();browser->tray->setTitle(title);}
extern "C" void browser_ui_tabs(const char *text,int active){try{auto data=Json::parse(text);browser->tabs->clear();int index=0;for(const auto &t:data){auto label=menuLabel(std::to_string(++index)+". "+t.get<std::string>());browser->tabs->add(label.c_str(),0,nullptr);}browser->tabs->value(active);}catch(...){} }
extern "C" void browser_ui_status(const char *text){browser->status->copy_label(text);}
extern "C" void browser_ui_cursor(int cursor){browser->cursor(cursor==1?FL_CURSOR_HAND:cursor==2?FL_CURSOR_INSERT:FL_CURSOR_DEFAULT);}
extern "C" void browser_ui_fullscreen(int enabled){if(browser->fullscreenMode==bool(enabled))return;std::string title=browser->label()?browser->label():"Dome Browser";browser->fullscreenMode=enabled;if(enabled)browser->fullscreen();else browser->fullscreen_off();browser->copy_label(title.c_str());browser->layout();}
extern "C" int browser_ui_ask(const char *title,const char *message){fl_message_title(title);return fl_choice("%s","Deny","Allow",nullptr,message)==1;}
extern "C" const char *browser_ui_prompt(const char *message,const char *value){fl_message_title("Dome Browser");const char *answer=fl_input("%s",value,message);if(!answer)return nullptr;returned=answer;return returned.c_str();}
extern "C" const char *browser_ui_password(const char *message){fl_message_title("Dome Browser");const char *answer=fl_password("%s","",message);if(!answer)return nullptr;returned=answer;return returned.c_str();}
extern "C" int browser_ui_color(unsigned char *red,unsigned char *green,unsigned char *blue){return fl_color_chooser("Choose a color",*red,*green,*blue);}
extern "C" void browser_ui_copy_image(const uint8_t *pixels,int width,int height){
  if(!pixels || width<=0 || height<=0)return;
  Fl_Copy_Surface surface(width,height);
  Fl_Surface_Device::push_current(&surface);
  fl_color(FL_WHITE);fl_rectf(0,0,width,height);
  Fl_RGB_Image image(pixels,width,height,4);image.draw(0,0);
  Fl_Surface_Device::pop_current();
}
extern "C" void browser_ui_copy(const char *text){Fl::copy(text,strlen(text),1);}
extern "C" void browser_ui_copy_primary(const char *text){Fl::copy(text,strlen(text),0);}
static const char *selection_text(int source){
  class Receiver:public Fl_Widget {public:bool ready=false;std::string value;Receiver():Fl_Widget(0,0,1,1){}void draw()override{}int handle(int event)override{if(event==FL_PASTE){value.assign(Fl::event_text(),Fl::event_length());ready=true;return 1;}return 0;}};
  static Receiver receiver;receiver.ready=false;receiver.value.clear();Fl::paste(receiver,source);
  auto deadline=g_get_monotonic_time()+1000000;while(!receiver.ready && g_get_monotonic_time()<deadline)Fl::wait(.01);
  if(!receiver.ready)return nullptr;
  returned=receiver.value;return returned.c_str();
}
extern "C" const char *browser_ui_paste(){return selection_text(1);}
extern "C" const char *browser_ui_primary_selection(){return selection_text(0);}
extern "C" const char *browser_ui_file(int multiple){Fl_Native_File_Chooser picker;picker.title("Choose a file");picker.type(multiple?Fl_Native_File_Chooser::BROWSE_MULTI_FILE:Fl_Native_File_Chooser::BROWSE_FILE);if(picker.show()!=0)return nullptr;Json paths=Json::array();for(int i=0;i<picker.count();++i)paths.push_back(picker.filename(i));returned=paths.dump();return returned.c_str();}
extern "C" int browser_ui_select(const char *text,int x,int y){try{auto items=Json::parse(text);Fl_Menu_Button popup(x+browser->x(),y+browser->y(),1,1);for(auto &item:items){auto label=menuLabel(item.get<std::string>());popup.add(label.c_str(),0,nullptr);}const Fl_Menu_Item *chosen=popup.popup();return chosen?int(chosen-popup.menu()):-1;}catch(...){return -1;}}
extern "C" void browser_ui_notification(const char *text){try{browser->notifications->post(Json::parse(text));}catch(...){} }
extern "C" int browser_ui_width(){return browser->page->w();}
extern "C" int browser_ui_height(){return browser->page->h();}
extern "C" int browser_ui_light(){return Fl::get_color(FL_BACKGROUND_COLOR)>0x80808000u;}
extern "C" int browser_ui_screenshot(const char *path){return dx::screenshot(*browser,path);}

extern "C" void browser_ui_history(const char *text){try{browser->address->history=Json::parse(text).get<std::vector<std::string>>();}catch(...){}}
