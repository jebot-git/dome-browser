// SPDX-License-Identifier: GPL-3.0-or-later
#include "../native/scroll-indicator.h"
#include <cassert>
#include <limits>
int main(){
 ScrollIndicator i;double top,length;
 i.update(1,0,0,200,1000,1);assert(i.opacity(1)==0);
 i.update(1,0,400,200,1000,2);assert(i.opacity(2)==1);assert(i.geometry(212,top,length));assert(length==40 && top==86);
 i.update(1,0,800,200,1000,3);i.geometry(212,top,length);assert(top==166);
 i.update(1,0,0,200,1000,4);i.geometry(212,top,length);assert(top==6);
 assert(i.opacity(4.7)>0 && i.opacity(4.7)<1);assert(i.opacity(5)==0);
 i.update(2,0,400,200,1000,6);assert(i.opacity(6)==0);
 i.update(2,0,500,200,1000,7);assert(i.opacity(7)==1);
 i.update(2,1,500,200,1000,8);assert(i.opacity(8)==0);
 i.update(2,1,0,200,100,9);assert(!i.geometry(212,top,length));
 i.update(2,1,std::numeric_limits<double>::quiet_NaN(),200,1000,10);assert(!i.scrollable);
 i.update(2,1,0,200,1000,11);i.update(2,1,99999,200,1000,12);assert(i.offset==800);
 assert(!i.geometry(5,top,length));
}
