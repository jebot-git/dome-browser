/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

fn main() {
    #[cfg(feature = "native-mse")]
    check_native_mse();
}

#[cfg(feature = "native-mse")]
fn check_native_mse() {
    use sha2::{Digest, Sha256};

    println!("cargo:rerun-if-changed=mse-native/dome-mse.patch");
    println!("cargo:rerun-if-env-changed=PKG_CONFIG_PATH");
    pkg_config::Config::new()
        .cargo_metadata(false)
        .exactly_version("1.28.7")
        .probe("gstreamer-mse-1.0")
        .expect("native-mse requires the staged Dome MSE backend; see mse-native/README.md");
    let digest = Sha256::digest(std::fs::read("mse-native/dome-mse.patch").unwrap());
    let expected: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    let actual = pkg_config::get_variable("gstreamer-mse-1.0", "dome_mse_patch_sha256")
        .expect("MSE pkg-config metadata must identify the Dome patch");
    assert_eq!(actual, expected,
        "native-mse selected an unpatched or stale library; rebuild mse-native and set PKG_CONFIG_PATH");
    println!("cargo:rustc-env=DOME_MSE_PATCH_SHA256={expected}");
}
