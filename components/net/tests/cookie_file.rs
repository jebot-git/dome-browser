/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::fs;
use std::time::{Duration, SystemTime};

use net::cookie::ServoCookie;
use net::cookie_storage::CookieStorage;
use net_traits::CookieSource;
use parking_lot::RwLock;
use servo_url::ServoUrl;

fn url() -> ServoUrl { ServoUrl::parse("https://example.org/").unwrap() }
fn cookie(value: &str) -> ServoCookie {
    ServoCookie::from_cookie_string(value, &url(), CookieSource::HTTP).unwrap()
}
fn push(jar: &RwLock<CookieStorage>, value: &str) {
    CookieStorage::access(jar).push(cookie(value), &url(), CookieSource::HTTP);
}
fn values(jar: &RwLock<CookieStorage>) -> String {
    CookieStorage::access(jar).cookies_for_url(&url(), CookieSource::HTTP).unwrap_or_default()
}
fn records(path: &std::path::Path) -> Vec<serde_json::Value> {
    fs::read_to_string(path).unwrap().lines().filter(|line| !line.starts_with('#') && !line.is_empty())
        .map(|line| serde_json::from_str(line).unwrap()).collect()
}
fn write_records(path: &std::path::Path, records: &[serde_json::Value]) {
    fs::write(path, records.iter().map(|record| serde_json::to_string(record).unwrap()+"\n").collect::<String>()).unwrap();
}

#[test]
fn cookie_file_migration_preserves_attributes_and_absolute_expiry() {
    let directory = tempfile::tempdir().unwrap();
    let mut old = CookieStorage::new(150);
    old.push(cookie("__Host-auth=secret-fixture; Secure; HttpOnly; SameSite=Strict; Path=/; Max-Age=3600"), &url(), CookieSource::HTTP);
    let before = serde_json::to_value(&old).unwrap();
    fs::write(directory.path().join("cookie_jar.json"), serde_json::to_vec(&old).unwrap()).unwrap();
    let jar = RwLock::new(CookieStorage::load_from_profile(directory.path(),150));
    assert_eq!(values(&jar), "__Host-auth=secret-fixture");
    let after = records(&directory.path().join("ck.txt"));
    let original = &before["cookies_map"]["example.org"][0];
    assert_eq!(after[0], *original);
    assert!(!directory.path().join("cookie_jar.json").exists());
    assert_eq!(CookieStorage::access(&jar).cookies_for_url(&url(), CookieSource::NonHTTP), None);
    let restarted = RwLock::new(CookieStorage::load_from_profile(directory.path(),150));
    assert_eq!(values(&restarted), "__Host-auth=secret-fixture");
    assert_eq!(records(&directory.path().join("ck.txt")), after);
}

#[test]
fn cookie_file_mutations_persist_without_shutdown_and_user_edits_reload() {
    let directory = tempfile::tempdir().unwrap();
    let jar = RwLock::new(CookieStorage::load_from_profile(directory.path(),150));
    let path = directory.path().join("ck.txt");
    push(&jar,"editable=before; Path=/; Max-Age=3600");
    let mut rows = records(&path);
    assert_eq!(rows.len(),1);
    let expiry = rows[0]["expiry_time"].clone();
    rows[0]["cookie"] = serde_json::Value::String(rows[0]["cookie"].as_str().unwrap().replace("editable=before","editable=after"));
    write_records(&path,&rows);
    assert_eq!(values(&jar),"editable=after");
    assert_eq!(records(&path)[0]["expiry_time"],expiry);
    fs::write(&path,"# User deleted every cookie record\n").unwrap();
    assert_eq!(values(&jar),"");
    assert_eq!(values(&RwLock::new(CookieStorage::load_from_profile(directory.path(),150))),"");
}

#[test]
fn cookie_file_deletion_clears_live_jar_and_never_reimports_legacy_values() {
    let directory = tempfile::tempdir().unwrap();
    let jar = RwLock::new(CookieStorage::load_from_profile(directory.path(),150));
    push(&jar,"old=value; Path=/");
    let legacy = serde_json::to_string(&*jar.read()).unwrap();
    fs::write(directory.path().join("cookie_jar.json"),legacy).unwrap();
    fs::remove_file(directory.path().join("ck.txt")).unwrap();
    assert_eq!(values(&jar),"");
    assert_eq!(values(&RwLock::new(CookieStorage::load_from_profile(directory.path(),150))),"");
}

#[test]
fn cookie_file_malformed_edits_are_preserved_and_repair_resumes() {
    let directory = tempfile::tempdir().unwrap();
    let jar = RwLock::new(CookieStorage::load_from_profile(directory.path(),150));
    let path = directory.path().join("ck.txt");
    push(&jar,"old=value; Path=/");
    let good = fs::read_to_string(&path).unwrap();
    fs::write(&path,"{malformed-private-fixture").unwrap();
    assert_eq!(values(&jar),"");
    push(&jar,"new=memory-only; Path=/");
    assert_eq!(fs::read_to_string(&path).unwrap(),"{malformed-private-fixture");
    fs::write(&path,good).unwrap();
    assert_eq!(values(&jar),"old=value");
    push(&jar,"later=saved; Path=/");
    assert_eq!(records(&path).len(),2);
}

#[test]
fn cookie_file_external_edit_wins_over_in_flight_network_mutation() {
    let directory = tempfile::tempdir().unwrap();
    let jar = RwLock::new(CookieStorage::load_from_profile(directory.path(),150));
    let path = directory.path().join("ck.txt");
    push(&jar,"old=value; Path=/");
    {
        let mut access = CookieStorage::access(&jar);
        access.push(cookie("network=value; Path=/"), &url(), CookieSource::HTTP);
        fs::write(&path,"# User cleared cookies during the request\n").unwrap();
    }
    assert_eq!(values(&jar),"");
    assert!(fs::read_to_string(&path).unwrap().contains("User cleared"));
}

#[test]
fn cookie_file_invalid_records_fail_closed_without_panicking_or_overwriting() {
    let directory = tempfile::tempdir().unwrap();
    let jar = RwLock::new(CookieStorage::load_from_profile(directory.path(),150));
    let path = directory.path().join("ck.txt");
    push(&jar,"valid=value; Path=/");
    let original = records(&path)[0].clone();
    let mut missing_domain = original.clone();
    missing_domain["cookie"] = "name=value; Path=/".into();
    let mut bad_host_prefix = original.clone();
    bad_host_prefix["cookie"] = "__Host-unsafe=value; Domain=example.org; Path=/; Secure".into();
    bad_host_prefix["host_only"] = false.into();
    for rows in [vec![missing_domain], vec![bad_host_prefix],vec![original.clone(),original]] {
        write_records(&path,&rows);
        let contents = fs::read_to_string(&path).unwrap();
        assert_eq!(values(&jar),"");
        push(&jar,"not=saved; Path=/");
        assert_eq!(fs::read_to_string(&path).unwrap(),contents);
    }
}

#[test]
fn cookie_file_expired_records_do_not_return_or_restart_max_age() {
    let directory = tempfile::tempdir().unwrap();
    let jar = RwLock::new(CookieStorage::load_from_profile(directory.path(),150));
    let path = directory.path().join("ck.txt");
    push(&jar,"expires=value; Path=/; Max-Age=3600");
    let mut rows = records(&path);
    rows[0]["expiry_time"] = serde_json::to_value(SystemTime::now()-Duration::from_secs(1)).unwrap();
    write_records(&path,&rows);
    assert_eq!(values(&jar),"");
    assert_eq!(values(&RwLock::new(CookieStorage::load_from_profile(directory.path(),150))),"");
}

#[test]
fn cookie_file_accepts_case_insensitive_domain_names() {
    let directory = tempfile::tempdir().unwrap();
    let mut old = CookieStorage::new(150);
    old.push(cookie("domain=value; Domain=EXAMPLE.ORG; Path=/"), &url(), CookieSource::HTTP);
    fs::write(directory.path().join("cookie_jar.json"), serde_json::to_vec(&old).unwrap()).unwrap();
    let jar = RwLock::new(CookieStorage::load_from_profile(directory.path(),150));
    assert_eq!(values(&jar),"domain=value");
    assert!(records(&directory.path().join("ck.txt"))[0]["cookie"].as_str().unwrap().contains("example.org"));
}

#[cfg(unix)]
#[test]
fn cookie_file_is_private_and_refuses_symlinks() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let directory = tempfile::tempdir().unwrap();
    let jar = RwLock::new(CookieStorage::load_from_profile(directory.path(),150));
    let path = directory.path().join("ck.txt");
    push(&jar,"private=value; Path=/");
    assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777,0o600);
    let target = directory.path().join("do-not-touch");
    fs::write(&target,"local-file-fixture").unwrap();
    fs::remove_file(&path).unwrap();
    symlink(&target,&path).unwrap();
    assert_eq!(values(&jar),"");
    push(&jar,"not=written; Path=/");
    assert_eq!(fs::read_to_string(&target).unwrap(),"local-file-fixture");
    assert!(fs::symlink_metadata(&path).unwrap().file_type().is_symlink());
}
