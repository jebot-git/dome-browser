/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::fs::File;
use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;

use app_units::Au;
use fonts::{
    Font, FontData, FontDescriptor, FontIdentifier, FontTemplate, FontTemplateDescriptor,
    FontTemplateRef, ShapingFlags, ShapingOptions,
};
use servo_url::ServoUrl;
use skrifa::Font as SkrifaFont;
use skrifa::font::Blob;
use style::Zero;
use style::computed_values::font_optical_sizing::T as FontOpticalSizing;
use style::computed_values::font_variant_position::T as FontVariantPosition;
use style::properties::longhands::font_variant_caps::computed_value::T as FontVariantCaps;
use style::values::computed::{
    FontFeatureSettings, FontStyle, FontSynthesis, FontVariantEastAsian, FontVariantLigatures,
    FontVariantNumeric, FontWeight, FontWidth,
};
use stylo_atoms::atom;
use unicode_script::Script;

fn make_font(path: PathBuf) -> Font {
    let mut bytes = Vec::new();
    File::open(path.clone())
        .expect("Couldn't open font file!")
        .read_to_end(&mut bytes)
        .unwrap();
    let data = FontData::from_bytes(&bytes);

    let identifier = FontIdentifier::Web(ServoUrl::from_file_path(path).unwrap());
    let skrifa_font = SkrifaFont::new(Blob::Shared(Arc::new(data.clone())), 0).unwrap();
    let descriptor = FontTemplateDescriptor::from_skrifa_font(&skrifa_font);
    let template = FontTemplate::new(identifier, descriptor, None);
    let descriptor = FontDescriptor {
        weight: FontWeight::normal(),
        width: FontWidth::hundred(),
        style: FontStyle::normal(),
        variant: FontVariantCaps::Normal,
        pt_size: Au::from_px(24),
        variation_settings: vec![],
        synthesis_weight: FontSynthesis::Auto,
        optical_sizing: FontOpticalSizing::Auto,
    };
    Font::new(
        FontTemplateRef::new(template),
        descriptor,
        Some(data),
        None,
        &Default::default(),
    )
    .unwrap()
}

#[test]
fn test_font_can_do_fast_shaping() {
    let dejavu_sans = make_font(
        [
            env!("CARGO_MANIFEST_DIR"),
            "tests",
            "support",
            "dejavu-fonts-ttf-2.37",
            "ttf",
            "DejaVuSans.ttf",
        ]
        .iter()
        .collect(),
    );

    let dejavu_sans_fast_shapeable = make_font(
        [
            env!("CARGO_MANIFEST_DIR"),
            "tests",
            "support",
            "dejavu-fonts-ttf-2.37",
            "ttf",
            "DejaVuSansNoGSUBNoGPOS.ttf",
        ]
        .iter()
        .collect(),
    );

    // Fast shaping requires a font with a kern table and no GPOS or GSUB tables.
    let shaping_options = ShapingOptions {
        letter_spacing: Au::zero(),
        word_spacing: Au::zero(),
        script: Script::Latin,
        language: atom!(""),
        language_override: None,
        flags: ShapingFlags::empty(),
        ligatures: FontVariantLigatures::NORMAL,
        numeric: FontVariantNumeric::NORMAL,
        east_asian: FontVariantEastAsian::NORMAL,
        feature_settings: FontFeatureSettings::normal(),
        position: FontVariantPosition::Normal,
        alternates: Default::default(),
    };
    assert!(!dejavu_sans.can_do_fast_shaping("WAVE", &shaping_options));
    assert!(dejavu_sans_fast_shapeable.can_do_fast_shaping("WAVE", &shaping_options));

    // Non-Latin script should never have fast shaping.
    let shaping_options = ShapingOptions {
        letter_spacing: Au::zero(),
        word_spacing: Au::zero(),
        script: Script::Cherokee,
        language: atom!(""),
        language_override: None,
        flags: ShapingFlags::empty(),
        ligatures: FontVariantLigatures::NORMAL,
        numeric: FontVariantNumeric::NORMAL,
        east_asian: FontVariantEastAsian::NORMAL,
        feature_settings: FontFeatureSettings::normal(),
        position: FontVariantPosition::Normal,
        alternates: Default::default(),
    };
    assert!(!dejavu_sans.can_do_fast_shaping("WAVE", &shaping_options));
    assert!(!dejavu_sans_fast_shapeable.can_do_fast_shaping("WAVE", &shaping_options));

    // Right-to-left text should never use fast shaping.
    let shaping_options = ShapingOptions {
        letter_spacing: Au::zero(),
        word_spacing: Au::zero(),
        script: Script::Latin,
        language: atom!(""),
        language_override: None,
        flags: ShapingFlags::RTL_FLAG,
        ligatures: FontVariantLigatures::NORMAL,
        numeric: FontVariantNumeric::NORMAL,
        east_asian: FontVariantEastAsian::NORMAL,
        feature_settings: FontFeatureSettings::normal(),
        position: FontVariantPosition::Normal,
        alternates: Default::default(),
    };
    assert!(!dejavu_sans.can_do_fast_shaping("WAVE", &shaping_options));
    assert!(!dejavu_sans_fast_shapeable.can_do_fast_shaping("WAVE", &shaping_options));
}

/// BDF strikes have no SFNT tables; constructing and shaping one must not
/// require a Skrifa/OpenType face or silently synthesize an outline font.
#[cfg(all(target_os = "linux", not(feature = "outline-fonts")))]
#[test]
fn native_bitmap_face_without_opentype_tables() {
    let data = FontData::from_bytes(include_bytes!("support/native-bitmap.bdf"));
    let identifier =
        FontIdentifier::Web(ServoUrl::parse("https://bitmap.invalid/native.bdf").unwrap());
    let descriptor = FontTemplateDescriptor::new(
        FontWeight::normal(),
        FontWidth::hundred(),
        FontStyle::normal(),
    );
    let template = FontTemplateRef::new(FontTemplate::new(identifier, descriptor, None));
    let font = Font::new(
        template,
        FontDescriptor {
            weight: FontWeight::normal(),
            width: FontWidth::hundred(),
            style: FontStyle::normal(),
            variant: FontVariantCaps::Normal,
            pt_size: Au::from_px(24),
            variation_settings: vec![],
            synthesis_weight: FontSynthesis::Auto,
            optical_sizing: FontOpticalSizing::Auto,
        },
        Some(data),
        None,
        &Default::default(),
    )
    .expect("native bitmap face must load without SFNT tables");
    assert_eq!(font.descriptor.pt_size, Au::from_px(7));
    assert!(font.baseline().is_none());
    let options = ShapingOptions {
        letter_spacing: Au::zero(),
        word_spacing: Au::zero(),
        script: Script::Latin,
        language: atom!(""),
        language_override: None,
        // Exercise HarfBuzz rather than Servo's ASCII fast path.
        flags: ShapingFlags::RTL_FLAG,
        ligatures: FontVariantLigatures::NORMAL,
        numeric: FontVariantNumeric::NORMAL,
        east_asian: FontVariantEastAsian::NORMAL,
        feature_settings: FontFeatureSettings::normal(),
        position: FontVariantPosition::Normal,
        alternates: Default::default(),
    };
    let shaped = font.shape_text("A", &options);
    assert_eq!(shaped.glyph_count(), 1);
    assert_eq!(shaped.glyphs().next().unwrap().advance(), Au::from_px(6));
}
