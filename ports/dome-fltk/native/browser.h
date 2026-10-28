// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once
#include <stdint.h>
#include <stddef.h>
extern "C" {
struct BrowserEvent { int kind, a, b, c, d; char text[8192]; };
int browser_ui_open(const char *url);
void browser_ui_wait(double seconds);
int browser_ui_poll(BrowserEvent *event);
int browser_ui_page_focused();
int browser_ui_visible();
void browser_ui_wake();
void browser_ui_close();
void browser_ui_scroll_metrics(uint64_t tab, uint64_t generation, float offset, float visible, float total);
void browser_ui_frame(const uint8_t *pixels, unsigned width, unsigned height);
void browser_ui_clear_frame();
void browser_ui_title(const char *title, const char *url, int back, int forward);
void browser_ui_tabs(const char *json, int active);
void browser_ui_status(const char *text);
void browser_ui_cursor(int cursor);
void browser_ui_fullscreen(int enabled);
int browser_ui_ask(const char *title, const char *message);
void *browser_ui_consent_open(const char *title, const char *message);
int browser_ui_consent_poll(void *dialog);
void browser_ui_consent_close(void *dialog);
const char *browser_ui_prompt(const char *message, const char *value);
int browser_ui_smallnet_form_open(const char *json);
int browser_ui_smallnet_form_poll(char *output, size_t capacity);
void browser_ui_smallnet_form_close();
const char *browser_ui_password(const char *message);
int browser_ui_color(unsigned char *red, unsigned char *green, unsigned char *blue);
void browser_ui_copy_image(const uint8_t *pixels, int width, int height);
void browser_ui_copy(const char *text);
const char *browser_ui_paste();
const char *browser_ui_primary_selection();
void browser_ui_copy_primary(const char *text);
const char *browser_ui_file(int multiple);
int browser_ui_select(const char *json, int x, int y);
void browser_ui_notification(const char *json);
int browser_ui_width();
int browser_ui_height();
int browser_ui_light();
const char *browser_ui_wallet(const char *json);
void browser_ui_wallet_clear();
void browser_ui_history(const char *json);
const char *browser_ui_edit_cookies(const char *text);
void browser_ui_cookie_policy(int blocked);
int browser_ui_screenshot(const char *path);
}
