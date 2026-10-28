// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once
#include <algorithm>
#include <cmath>
#include <cstdint>
struct ScrollIndicator {
  uint64_t owner=0,generation=0;
  bool initialized=false,scrollable=false;
  double offset=0,visible=0,total=0,last_motion=-1e9;
  bool update(uint64_t tab,uint64_t navigation,double position,double viewport,double document,double now) {
    bool reset=!initialized || owner!=tab || generation!=navigation;
    bool valid=std::isfinite(position)&&std::isfinite(viewport)&&std::isfinite(document)&&viewport>0&&document>viewport+0.5;
    double next=valid?std::clamp(position,0.0,document-viewport):0;
    bool moved=!reset && valid && scrollable && std::abs(next-offset)>0.25;
    bool changed=reset || next!=offset || viewport!=visible || document!=total || valid!=scrollable;
    owner=tab;generation=navigation;initialized=true;scrollable=valid;
    offset=next;visible=viewport;total=document;
    if(reset || !valid) last_motion=-1e9;
    else if(moved) last_motion=now;
    return changed;
  }
  double opacity(double now)const {return scrollable?std::clamp((0.85-(now-last_motion))/0.25,0.0,1.0):0;}
  bool geometry(double height,double &top,double &length)const {
    if(!scrollable || height<=12)return false;
    double track=height-12;
    length=std::clamp(track*visible/total,std::min(18.0,track),track);
    top=6+(track-length)*offset/(total-visible);
    return true;
  }
};
