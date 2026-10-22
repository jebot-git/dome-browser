// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once
#include <stdint.h>
extern "C" {
struct BrowserEvent { int kind, a, b, c, d; char text[8192]; };
int browser_ui_open(const char *url);
void browser_ui_wait(double seconds);
int browser_ui_poll(BrowserEvent *event);
void browser_ui_wake();
void browser_ui_close();
void browser_ui_frame(const uint8_t *pixels, unsigned width, unsigned height);
void browser_ui_title(const char *title, const char *url, int back, int forward);
void browser_ui_tabs(const char *json, int active);
void browser_ui_status(const char *text);
void browser_ui_cursor(int cursor);
void browser_ui_fullscreen(int enabled);
int browser_ui_ask(const char *title, const char *message);
const char *browser_ui_prompt(const char *message, const char *value);
const char *browser_ui_password(const char *message);
int browser_ui_color(unsigned char *red, unsigned char *green, unsigned char *blue);
void browser_ui_copy(const char *text);
const char *browser_ui_paste();
const char *browser_ui_file(int multiple);
int browser_ui_select(const char *json, int x, int y);
void browser_ui_notification(const char *json);
int browser_ui_width();
int browser_ui_height();
int browser_ui_light();
int browser_ui_screenshot(const char *path);
}
