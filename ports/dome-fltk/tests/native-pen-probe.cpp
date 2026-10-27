// Test-only native FLTK pen callback observer. No browser input endpoint.
#include <FL/Fl.H>
#include <FL/Fl_Double_Window.H>
#include <FL/Fl_Button.H>
#include <FL/platform.H>
#include <cstdio>
#include <cstring>
class PenWindow : public Fl_Double_Window {
public:
  PenWindow():Fl_Double_Window(480,320,"Native FLTK pen probe"){}
  int handle(int e) override {
    if(e==FL_PUSH||e==FL_RELEASE||e==FL_DRAG){std::printf("PEN_PROBE {\"mouse\":%d,\"x\":%d,\"y\":%d}\n",e,Fl::event_x(),Fl::event_y());std::fflush(stdout);}
    return Fl_Double_Window::handle(e);
  }
};
static int pen(Fl_Window*,const Fl_Wayland_Pen_Event *e,void*) {
  std::printf("PEN_PROBE {\"action\":%d,\"id\":%d,\"primary\":%d,\"x\":%.17g,\"y\":%.17g,\"pressure\":%.17g,\"distance\":%.17g,\"tilt\":[%.17g,%.17g],\"rotation\":%.17g,\"slider\":%.17g,\"wheel\":%.17g,\"wheel_clicks\":%d,\"buttons\":%u,\"capabilities\":%u,\"time\":%u,\"eraser\":%d}\n",
    e->action,e->id,e->primary,e->x,e->y,e->pressure,e->distance,e->tilt_x,e->tilt_y,e->rotation,e->tangential_pressure,e->wheel,e->wheel_clicks,e->buttons,e->capabilities,e->time,e->eraser);std::fflush(stdout);
  if(e->action==0 && e->x>=400){fl_wl_set_pen_handler(nullptr,nullptr);fl_wl_set_pen_handler(pen,nullptr);}
  return e->y>=80;
}
int main(){
  PenWindow w;Fl_Button button(20,15,160,40,"Pen fallback");button.callback([](Fl_Widget*,void*){std::puts("PEN_PROBE {\"clicked\":true}");std::fflush(stdout);});w.end();w.show();fl_wl_set_pen_handler(pen,nullptr);
  Fl::add_fd(0,FL_READ,[](int,void *data){char text[64];if(!std::fgets(text,sizeof text,stdin)){Fl::remove_fd(0);return;}
    text[std::strcspn(text,"\r\n")]=0;auto w=(PenWindow*)data;
    if(!std::strcmp(text,"disable"))fl_wl_set_pen_handler(nullptr,nullptr);
    if(!std::strcmp(text,"enable"))fl_wl_set_pen_handler(pen,nullptr);
    if(!std::strcmp(text,"hide"))w->hide();
    if(!std::strcmp(text,"show"))w->show();
    std::printf("PEN_PROBE {\"ack\":\"%s\"}\n",text);std::fflush(stdout);
  },&w);
  Fl::add_timeout(.25,[](void*){std::puts("PEN_PROBE {\"ready\":true}");std::fflush(stdout);});
  // The test can hide and restore the same window without ending this process.
  while(Fl::first_window() || !w.shown())Fl::wait(.01);
  fl_wl_set_pen_handler(nullptr,nullptr);
}
