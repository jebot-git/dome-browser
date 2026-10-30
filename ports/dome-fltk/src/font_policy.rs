// SPDX-License-Identifier: GPL-3.0-or-later
//! The browser consumes the native session face; it ships no font data.

#[derive(Clone, Debug, PartialEq, serde::Deserialize)]
pub struct Settings { pub family: String, pub pixels: u32, pub file: String, pub source: String }

impl Settings {
    pub fn read() -> Result<Option<Self>, String> {
        if cfg!(feature = "outline-fonts") { return Ok(None); }
        let text = crate::ui::string(unsafe { crate::ui::browser_ui_font_settings() })
            .ok_or("Cannot resolve the DomeD bitmap font")?;
        let value: Self = serde_json::from_str(&text).map_err(|_| "Invalid native font settings")?;
        if !(4..=128).contains(&value.pixels) || !value.family.starts_with("Dome Browser Bitmap ") {
            return Err("The selected font is not a native DomeD BDF/PCF strike".into());
        }
        Ok(Some(value))
    }

    pub fn preferences(&self, prefs: &mut servo::Preferences) {
        prefs.fonts_default = self.family.clone();
        prefs.fonts_monospace = self.family.clone();
        prefs.fonts_serif = self.family.clone();
        prefs.fonts_sans_serif = self.family.clone();
        prefs.fonts_default_size = self.pixels as i64;
        prefs.fonts_default_monospace_size = self.pixels as i64;
    }

    pub fn update(&self, servo: &servo::Servo) {
        for name in ["fonts_default", "fonts_monospace", "fonts_serif", "fonts_sans_serif"] {
            servo.set_preference(name, servo::PrefValue::Str(self.family.clone()));
        }
        for name in ["fonts_default_size", "fonts_default_monospace_size"] {
            servo.set_preference(name, servo::PrefValue::Int(self.pixels as i64));
        }
    }
}
