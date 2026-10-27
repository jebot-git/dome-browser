// Test-only Wayland tablet protocol observer on a real native FLTK surface.
#include <FL/Fl.H>
#include <FL/Fl_Double_Window.H>
#include <FL/platform.H>
#include <wayland-client.h>
#include "tablet-client.h"
#include <cstdio>
#include <cstring>
#include <set>
#include <dlfcn.h>
struct Fl_Wayland_Pen_Event;

static Fl_Window *window;
static zwp_tablet_manager_v2 *manager;
static zwp_tablet_seat_v2 *tablet_seat;
static wl_seat *seat;
static std::set<zwp_tablet_v2*> tablets;
class ProbeWindow : public Fl_Double_Window {
public:
  ProbeWindow():Fl_Double_Window(480,320,"Native tablet protocol probe"){}
  int handle(int e) override {
    if(e==FL_PUSH||e==FL_RELEASE||e==FL_MOVE||e==FL_DRAG){
      std::printf("TABLET_PROBE {\"mouse\":%d,\"x\":%d,\"y\":%d}\n",e,Fl::event_x(),Fl::event_y());std::fflush(stdout);
      if(e==FL_PUSH){take_focus();return 1;}
      if(e==FL_DRAG||e==FL_RELEASE)return 1;
    }
    return Fl_Double_Window::handle(e);
  }
};
static unsigned id(zwp_tablet_tool_v2 *tool) {return wl_proxy_get_id((wl_proxy*)tool);}
static void event(zwp_tablet_tool_v2 *tool,const char *name) {
  std::printf("TABLET_PROBE {\"tool\":%u,\"event\":\"%s\"}\n",id(tool),name);std::fflush(stdout);
}
static const zwp_tablet_tool_v2_listener tool_listener={
  [](void*,zwp_tablet_tool_v2 *t,uint32_t type){std::printf("TABLET_PROBE {\"tool\":%u,\"event\":\"type\",\"value\":%u}\n",id(t),type);std::fflush(stdout);},
  [](void*,zwp_tablet_tool_v2*,uint32_t,uint32_t){},
  [](void*,zwp_tablet_tool_v2*,uint32_t,uint32_t){},
  [](void*,zwp_tablet_tool_v2 *t,uint32_t value){std::printf("TABLET_PROBE {\"tool\":%u,\"event\":\"capability\",\"value\":%u}\n",id(t),value);std::fflush(stdout);},
  [](void*,zwp_tablet_tool_v2 *t){event(t,"done");},
  [](void*,zwp_tablet_tool_v2 *t){event(t,"removed");zwp_tablet_tool_v2_destroy(t);},
  [](void*,zwp_tablet_tool_v2 *t,uint32_t,zwp_tablet_v2 *tablet,wl_surface *s){std::printf("TABLET_PROBE {\"tool\":%u,\"event\":\"in\",\"own_surface\":%s,\"own_tablet\":%s}\n",id(t),s==fl_wl_surface(fl_wl_xid(window))?"true":"false",tablets.count(tablet)?"true":"false");std::fflush(stdout);},
  [](void*,zwp_tablet_tool_v2 *t){event(t,"out");},
  [](void*,zwp_tablet_tool_v2 *t,uint32_t){event(t,"down");},
  [](void*,zwp_tablet_tool_v2 *t){event(t,"up");},
  [](void*,zwp_tablet_tool_v2 *t,wl_fixed_t x,wl_fixed_t y){std::printf("TABLET_PROBE {\"tool\":%u,\"event\":\"motion\",\"x\":%.17g,\"y\":%.17g}\n",id(t),wl_fixed_to_double(x),wl_fixed_to_double(y));std::fflush(stdout);},
  [](void*,zwp_tablet_tool_v2 *t,uint32_t value){std::printf("TABLET_PROBE {\"tool\":%u,\"event\":\"pressure\",\"value\":%u}\n",id(t),value);std::fflush(stdout);},
  [](void*,zwp_tablet_tool_v2 *t,uint32_t value){std::printf("TABLET_PROBE {\"tool\":%u,\"event\":\"distance\",\"value\":%u}\n",id(t),value);std::fflush(stdout);},
  [](void*,zwp_tablet_tool_v2 *t,wl_fixed_t x,wl_fixed_t y){std::printf("TABLET_PROBE {\"tool\":%u,\"event\":\"tilt\",\"x\":%.17g,\"y\":%.17g}\n",id(t),wl_fixed_to_double(x),wl_fixed_to_double(y));std::fflush(stdout);},
  [](void*,zwp_tablet_tool_v2 *t,wl_fixed_t value){std::printf("TABLET_PROBE {\"tool\":%u,\"event\":\"rotation\",\"value\":%.17g}\n",id(t),wl_fixed_to_double(value));std::fflush(stdout);},
  [](void*,zwp_tablet_tool_v2 *t,int32_t value){std::printf("TABLET_PROBE {\"tool\":%u,\"event\":\"slider\",\"value\":%d}\n",id(t),value);std::fflush(stdout);},
  [](void*,zwp_tablet_tool_v2 *t,wl_fixed_t degrees,int32_t clicks){std::printf("TABLET_PROBE {\"tool\":%u,\"event\":\"wheel\",\"degrees\":%.17g,\"clicks\":%d}\n",id(t),wl_fixed_to_double(degrees),clicks);std::fflush(stdout);},
  [](void*,zwp_tablet_tool_v2 *t,uint32_t,uint32_t button,uint32_t state){std::printf("TABLET_PROBE {\"tool\":%u,\"event\":\"button\",\"button\":%u,\"state\":%u}\n",id(t),button,state);std::fflush(stdout);},
  [](void*,zwp_tablet_tool_v2 *t,uint32_t time){std::printf("TABLET_PROBE {\"tool\":%u,\"event\":\"frame\",\"time\":%u}\n",id(t),time);std::fflush(stdout);}
};
static const zwp_tablet_v2_listener tablet_listener={
  [](void*,zwp_tablet_v2*,const char*){},[](void*,zwp_tablet_v2*,uint32_t,uint32_t){},
  [](void*,zwp_tablet_v2*,const char*){},[](void*,zwp_tablet_v2*){},
  [](void*,zwp_tablet_v2 *t){tablets.erase(t);std::puts("TABLET_PROBE {\"tablet_removed\":true}");std::fflush(stdout);zwp_tablet_v2_destroy(t);}
};
static const zwp_tablet_seat_v2_listener seat_listener={
  [](void*,zwp_tablet_seat_v2*,zwp_tablet_v2 *t){tablets.insert(t);zwp_tablet_v2_add_listener(t,&tablet_listener,nullptr);},
  [](void*,zwp_tablet_seat_v2*,zwp_tablet_tool_v2 *t){zwp_tablet_tool_v2_add_listener(t,&tool_listener,nullptr);},
  [](void*,zwp_tablet_seat_v2*,zwp_tablet_pad_v2 *p){zwp_tablet_pad_v2_destroy(p);}
};
static void bind_tablet_seat(){
  if(manager&&seat&&!tablet_seat){tablet_seat=zwp_tablet_manager_v2_get_tablet_seat(manager,seat);zwp_tablet_seat_v2_add_listener(tablet_seat,&seat_listener,nullptr);}
}
static const wl_registry_listener registry_listener={
  [](void*,wl_registry *r,uint32_t name,const char *interface,uint32_t){
    if(!std::strcmp(interface,"wl_seat")&&!seat)seat=(wl_seat*)wl_registry_bind(r,name,&wl_seat_interface,1);
    if(!std::strcmp(interface,"zwp_tablet_manager_v2")&&!manager)manager=(zwp_tablet_manager_v2*)wl_registry_bind(r,name,&zwp_tablet_manager_v2_interface,1);
    bind_tablet_seat();
  },[](void*,wl_registry*,uint32_t){}
};
int main(){
  ProbeWindow w;window=&w;w.end();w.show();
  // A direct tablet consumer coexists with the toolkit's own tablet binding.
  typedef int (*SetPenHandler)(int (*)(Fl_Window*,const Fl_Wayland_Pen_Event*,void*),void*);
  auto set_pen=(SetPenHandler)dlsym(RTLD_DEFAULT,"fl_wl_set_pen_handler");
  if(set_pen)set_pen([](Fl_Window*,const Fl_Wayland_Pen_Event*,void*){return 1;},nullptr);
  wl_registry *registry=wl_display_get_registry(fl_wl_display());wl_registry_add_listener(registry,&registry_listener,nullptr);
  wl_display_roundtrip(fl_wl_display());wl_display_roundtrip(fl_wl_display());
  Fl::add_timeout(.25,[](void*){std::printf("TABLET_PROBE {\"ready\":%s}\n",tablet_seat?"true":"false");std::fflush(stdout);});
  while(w.shown())Fl::wait(.01);
  if(tablet_seat)zwp_tablet_seat_v2_destroy(tablet_seat);
  if(manager)zwp_tablet_manager_v2_destroy(manager);
  if(seat)wl_seat_destroy(seat);
  wl_registry_destroy(registry);
}
