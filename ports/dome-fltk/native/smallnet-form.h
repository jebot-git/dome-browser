// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once
#include <FL/Fl_Scroll.H>
#include <FL/Fl_Secret_Input.H>
#include <FL/Fl_Multiline_Input.H>
#include <FL/Fl_Multiline_Output.H>
#include <FL/Fl_Check_Button.H>
#include <FL/Fl_Output.H>
#include <FL/Fl_Return_Button.H>

// Form answers never use the application's global returned-string buffer.
class SmallnetForm : public Fl_Double_Window {
  struct Field {
    std::string kind, suggestion, fileTitle;
    bool required=false;
    Fl_Input *input=nullptr;
    Fl_Choice *choice=nullptr;
    Fl_Check_Button *check=nullptr;
  };
  std::vector<std::unique_ptr<Field>> fields;
  bool accepted=false;
  Fl_Box *status=nullptr;
  static void pick(Fl_Widget *,void *data) {
    auto field=static_cast<Field *>(data);
    Fl_Native_File_Chooser picker;
    const bool save=field->kind=="save";
    picker.title(field->fileTitle.c_str());
    picker.type(save?Fl_Native_File_Chooser::BROWSE_SAVE_FILE:Fl_Native_File_Chooser::BROWSE_FILE);
    if(save){picker.options(Fl_Native_File_Chooser::SAVEAS_CONFIRM);picker.preset_file(field->suggestion.c_str());}
    if(picker.show()==0 && picker.filename())field->input->value(picker.filename());
  }
  static std::string labelText(const std::string &text) {
    std::string result;
    for(char c:text){result+=c;if(c=='@')result+='@';}
    return result;
  }
  static std::string menuText(const std::string &text) {
    std::string result;
    for(char c:text){if(c=='/'||c=='\\')result+='\\';if(c=='&')result+='&';if(c=='@')result+='@';result+=c;}
    return result;
  }
public:
  explicit SmallnetForm(const Json &spec):Fl_Double_Window(760,650,"Gopher+ form") {
    copy_label(spec.value("title",std::string("Gopher+ form")).c_str());
    auto origin=new Fl_Box(16,10,728,54);
    origin->copy_label(labelText(spec.at("origin").get<std::string>()).c_str());
    origin->align(FL_ALIGN_LEFT|FL_ALIGN_INSIDE|FL_ALIGN_WRAP);
    auto scroll=new Fl_Scroll(12,72,736,500);scroll->type(Fl_Scroll::VERTICAL_ALWAYS);
    int y=80;
    for(const auto &item:spec.at("fields")) {
      auto field=std::make_unique<Field>();
      field->kind=item.at("kind").get<std::string>();
      field->required=item.value("required",false);
      field->fileTitle=item.value("file_title",std::string(field->kind=="save"?"Save Gopher+ response":"Choose a file to send"));
      const auto label=labelText(item.at("label").get<std::string>());
      const auto value=item.at("value").get<std::string>();
      if(field->kind=="note") {
        auto note=new Fl_Box(24,y,692,46);note->copy_label(label.c_str());note->align(FL_ALIGN_LEFT|FL_ALIGN_INSIDE|FL_ALIGN_WRAP);y+=54;
      } else if(field->kind=="check") {
        field->check=new Fl_Check_Button(24,y,692,36);field->check->copy_label(label.c_str());field->check->value(value=="1");y+=44;
      } else {
        auto prompt=new Fl_Box(24,y,692,32);prompt->copy_label(label.c_str());prompt->align(FL_ALIGN_LEFT|FL_ALIGN_INSIDE|FL_ALIGN_WRAP);y+=34;
        if(field->kind=="choice") {
          field->choice=new Fl_Choice(24,y,692,34);
          for(const auto &option:item.at("options")){auto text=menuText(option.get<std::string>());field->choice->add(text.c_str());}
          field->choice->value(0);y+=42;
        } else if(field->kind=="file"||field->kind=="save") {
          field->input=new Fl_Output(24,y,540,34);field->suggestion=value;
          auto choose=new Fl_Button(576,y,140,34,field->kind=="save"?"Save as...":"Choose file...");
          choose->callback(pick,field.get());y+=42;
        } else {
          if(field->kind=="password")field->input=new Fl_Secret_Input(24,y,692,34);
          else if(field->kind=="multiline")field->input=new Fl_Multiline_Input(24,y,692,110);
          else if(field->kind=="preview")field->input=new Fl_Multiline_Output(24,y,692,220);
          else field->input=new Fl_Input(24,y,692,34);
          field->input->maximum_size(65536);field->input->value(value.c_str());y+=field->input->h()+8;
        }
      }
      fields.push_back(std::move(field));
    }
    scroll->end();
    status=new Fl_Box(16,580,448,54);status->copy_label(labelText(spec.value("description",std::string("Submit sends these answers to the displayed server."))).c_str());status->align(FL_ALIGN_LEFT|FL_ALIGN_INSIDE|FL_ALIGN_WRAP);
    auto cancel=new Fl_Button(476,596,120,34,"Cancel");cancel->callback([](Fl_Widget *w,void *){w->window()->hide();});
    auto submit=new Fl_Return_Button(608,596,136,34);submit->copy_label(spec.value("submit_label",std::string("Submit")).c_str());
    submit->callback([](Fl_Widget *w,void *data){auto form=static_cast<SmallnetForm *>(data);for(const auto &field:form->fields){if((field->required||field->kind=="file"||field->kind=="save")&&field->input&&(!field->input->value()||!*field->input->value())){form->status->copy_label("Complete the required fields before continuing.");return;}}form->accepted=true;w->window()->hide();},this);
    // Keep the prompt above its browser without blocking replacement navigation.
    end();set_non_modal();dx::style(this);show();
    bool focused=false;
    for(const auto &field:fields)if(field->input&&field->kind!="file"&&field->kind!="save"&&field->kind!="preview"){field->input->take_focus();focused=true;break;}
    if(!focused)for(const auto &field:fields)if(field->input&&field->kind=="preview"){field->input->take_focus();break;}
  }
  int poll(char *output,size_t capacity) {
    if(shown())return 0;
    if(!accepted)return -1;
    Json answers=Json::array();
    for(const auto &field:fields){Json answer=Json::object();if(field->input)answer["value"]=field->input->value();if(field->choice)answer["choice"]=field->choice->value();if(field->check)answer["checked"]=bool(field->check->value());answers.push_back(std::move(answer));}
    auto text=answers.dump();
    if(text.size()+1>capacity)return -1;
    std::memcpy(output,text.c_str(),text.size()+1);return 1;
  }
};
static std::unique_ptr<SmallnetForm> smallnetForm;
extern "C" int browser_ui_smallnet_form_open(const char *text) {
  try {smallnetForm.reset();auto spec=Json::parse(text);if(!spec.at("fields").is_array()||spec.at("fields").size()>64)return 0;smallnetForm=std::make_unique<SmallnetForm>(spec);return 1;}
  catch(...){smallnetForm.reset();return 0;}
}
extern "C" int browser_ui_smallnet_form_poll(char *output,size_t capacity) {
  if(!smallnetForm)return -1;
  try {int result=smallnetForm->poll(output,capacity);if(result)smallnetForm.reset();return result;}
  catch(...){smallnetForm.reset();return -1;}
}
extern "C" void browser_ui_smallnet_form_close(){smallnetForm.reset();}
