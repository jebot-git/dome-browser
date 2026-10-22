#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Migration fixtures contain synthetic credentials only; no real wallet access."""
import fcntl
import contextlib
import hashlib
import importlib.util
import json
from pathlib import Path
import sqlite3
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("migration", Path(__file__).resolve().parents[1]/"migrate-vivaldi.py")
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)


class Migration(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.source = Path(self.temp.name)/"Vivaldi"
        self.target = Path(self.temp.name)/"Dome"
        self.source.mkdir()
        self.target.mkdir()
        (self.source/"Bookmarks").write_text(json.dumps({"roots": {
            "bookmark_bar": {"name": "Bookmarks", "type": "folder", "children": [
                {"name": "日本語", "type": "folder", "children": [
                    {"type": "url", "url": "https://example.org/", "name": "A & <B> 日本語"},
                    {"type": "url", "url": "javascript:alert(1)", "name": "Unsafe"}]}]},
            "trash": {"type": "folder", "name": "Trash", "children": [
                {"type": "url", "url": "https://deleted.example/", "name": "Deleted"}]}}}))
        with contextlib.closing(sqlite3.connect(self.source/"History")) as db, db:
            db.execute("CREATE TABLE urls (id INTEGER, url TEXT, last_visit_time INTEGER)")
            db.executemany("INSERT INTO urls VALUES (?,?,?)", [(i, f"https://example.org/{i}", i) for i in range(2100)])
            db.execute("INSERT INTO urls VALUES (2101,'https://user:secret@example.org/',2101)")

    def test_bookmarks_history_backups_and_idempotent_merging(self):
        m.atomic(self.target/"bm.txt", ":s0: Existing\ns0 https://kept.example/ Keep\n")
        m.atomic(self.target/"hs.txt", "https://kept.example/\n")
        report = m.migrate(self.source, self.target)
        entries, names = m.parse_bookmarks(m.read(self.target/"bm.txt"))
        self.assertIn(("Vivaldi / Bookmarks / 日本語", "https://example.org/", "A & <B> 日本語"), entries)
        self.assertEqual(len(entries), 2)
        self.assertEqual(report["bookmarks_unsupported"], 1)
        self.assertEqual(report["history_active_total"], 2000)
        self.assertEqual(report["history_source_eligible"], 2100)
        history = m.read(self.target/"hs.txt").splitlines()
        self.assertEqual(history[:2], ["https://kept.example/", "https://example.org/2099"])
        backup = Path(report["backup"])
        self.assertEqual(len((backup/"history-vivaldi-full.txt").read_text().splitlines()), 2100)
        self.assertEqual((backup/"hs.txt").read_text(), "https://kept.example/\n")
        for path in [self.target/"bm.txt", self.target/"hs.txt", backup/"report.json", backup/"history-vivaldi-full.txt"]:
            self.assertEqual(path.stat().st_mode & 0o777, 0o600)
        self.assertEqual(m.migrate(self.source, self.target)["bookmarks_added"], 0)

    def test_active_profile_and_malformed_files_are_preserved(self):
        with (self.target/"browser.lock").open("a") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            with self.assertRaises(ValueError):
                m.migrate(self.source, self.target)
        (self.target/"bm.txt").write_text("invalid bookmark data")
        with self.assertRaises(ValueError):
            m.migrate(self.source, self.target)
        self.assertEqual((self.target/"bm.txt").read_text(), "invalid bookmark data")

    def test_password_decryption_scoping_and_non_overwrite(self):
        from cryptography.hazmat.primitives.ciphers import Cipher, algorithms, modes
        from cryptography.hazmat.primitives.padding import PKCS7
        secret = "test-only-storage-key"
        password = "fixture-only-secret 日本語"
        key = hashlib.pbkdf2_hmac("sha1", secret.encode(), b"saltysalt", 1, 16)
        padder = PKCS7(128).padder()
        padded = padder.update(password.encode()) + padder.finalize()
        encrypt = Cipher(algorithms.AES(key), modes.CBC(b" "*16)).encryptor()
        blob = b"v11" + encrypt.update(padded) + encrypt.finalize()
        with contextlib.closing(sqlite3.connect(self.source/"Login Data")) as db, db:
            db.execute("CREATE TABLE logins(origin_url TEXT,action_url TEXT,username_value TEXT,password_value BLOB,scheme INTEGER,blacklisted_by_user INTEGER,date_password_modified INTEGER,date_created INTEGER)")
            rows = [("https://example.org/signin", "https://example.org/login", "alice", blob, 0, 0, 1, 1),
                    ("https://example.org/signin", "https://other.example/login", "alice", blob, 0, 0, 1, 1),
                    ("http://insecure.example/", "", "alice", blob, 0, 0, 1, 1),
                    ("imap://mail.example/", "", "alice", blob, 3, 0, 1, 1)]
            db.executemany("INSERT INTO logins VALUES (?,?,?,?,?,?,?,?)", rows)
        class Wallet:
            def __init__(self):
                self.values = {("Chrome Keys", "Chrome Safe Storage"): secret}
            def folder(self, name): pass
            def has(self, folder, key): return (folder,key) in self.values
            def get(self, folder, key): return self.values[folder,key]
            def put(self, folder, key, value): self.values[folder,key] = value
        wallet = Wallet()
        report = m.migrate_passwords(self.source, self.target, lambda: wallet)
        self.assertEqual(report, {"imported": 1, "cross_origin_form_action": 1, "insecure_or_unsupported_origin": 1, "non_web_login": 1})
        folder = "Dome Browser " + (self.target/"wallet-id").read_text()
        record = next((key,value) for (f,key),value in wallet.values.items() if f==folder)
        self.assertEqual(json.loads(record[0]), ["html","https://example.org","https://example.org/login","alice"])
        self.assertEqual(json.loads(record[1])["password"], password)
        self.assertEqual(m.migrate_passwords(self.source,self.target,lambda:wallet)["existing_preserved"], 1)
        for path in self.target.rglob("*"):
            if path.is_file(): self.assertNotIn(password.encode(),path.read_bytes())


if __name__ == "__main__":
    unittest.main()
