#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Merge a local Vivaldi profile into Dome; passwords never leave memory/KWallet."""
import argparse
import collections
import contextlib
import fcntl
import hashlib
import html
import ipaddress
import json
import os
from pathlib import Path
import shutil
import sqlite3
import tempfile
import time
from urllib.parse import urlsplit
import uuid

LIMIT = 4 * 1024 * 1024
APP = "org.domed.BrowserMigration"


def clean(value):
    return "".join(" " if ord(c) < 32 or 127 <= ord(c) < 160 else c for c in value)


def usable(url):
    try:
        u = urlsplit(url)
        return (not any(c.isspace() or ord(c) < 32 for c in url) and
                u.username is None and u.password is None and
                ((u.scheme in ("http", "https") and bool(u.hostname) and u.port != 0) or
                 u.scheme == "file" or url == "about:blank"))
    except ValueError:
        return False


def origin(url):
    if not usable(url):
        return None
    u = urlsplit(url)
    if u.scheme not in ("https", "http"):
        return None
    host = u.hostname.lower()
    if u.scheme == "http" and host != "localhost":
        try:
            if not ipaddress.ip_address(host).is_loopback:
                return None
        except ValueError:
            return None
    if ":" in host:
        host = "[" + host + "]"
    port = u.port
    return u.scheme + "://" + host + (":" + str(port) if port and port != (443 if u.scheme == "https" else 80) else "")


def read(path):
    if not path.exists():
        return ""
    with path.open("rb") as f:
        data = f.read(LIMIT + 1)
    if len(data) > LIMIT:
        raise ValueError("Existing profile file exceeds 4 MiB")
    return data.decode("utf-8")


def atomic(path, text, limit=LIMIT):
    data = text.encode("utf-8")
    if len(data) > limit:
        raise ValueError("Migration output exceeds file limit")
    fd, temporary = tempfile.mkstemp(prefix="." + path.name + "-", dir=path.parent)
    try:
        with os.fdopen(fd, "wb") as f:
            f.write(data)
            f.flush()
            os.fsync(f.fileno())
        os.replace(temporary, path)
        directory = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
    finally:
        with contextlib.suppress(FileNotFoundError):
            os.unlink(temporary)


@contextlib.contextmanager
def snapshot(source):
    """Copy a stable encrypted SQLite file set, including any pending journal/WAL."""
    with tempfile.TemporaryDirectory(prefix="dome-vivaldi-", dir=os.environ.get("XDG_RUNTIME_DIR")) as directory:
        destination = Path(directory) / source.name
        files = [source.with_name(source.name + suffix) for suffix in ("", "-wal", "-journal")]
        def stamp():
            return [(p.name, p.stat().st_ino, p.stat().st_size, p.stat().st_mtime_ns)
                    for p in files if p.exists()]
        for attempt in range(5):
            db = None
            before = stamp()
            for p in Path(directory).iterdir():
                p.unlink()
            try:
                for p in files:
                    if p.exists():
                        out = Path(directory) / p.name
                        shutil.copyfile(p, out)
                        out.chmod(0o600)
                if before != stamp():
                    continue
                db = sqlite3.connect(destination)
                if db.execute("PRAGMA quick_check").fetchone()[0] != "ok":
                    db.close()
                    continue
                break
            except (FileNotFoundError, sqlite3.DatabaseError):
                if db is not None:
                    db.close()
                if attempt == 4:
                    raise ValueError("Cannot obtain a consistent Vivaldi database snapshot") from None
        else:
            raise ValueError("Vivaldi database changed during every snapshot attempt; retry when idle")
        try:
            yield db
        finally:
            db.close()


def parse_bookmarks(text):
    sections, entries = {}, []
    for line in text.splitlines():
        if not line.strip():
            continue
        if line.startswith(":s"):
            ident, name = line[2:].split(":", 1)
            ident = int(ident)
            if ident in sections:
                raise ValueError("Duplicate Dome bookmark section")
            sections[ident] = clean(name.strip())
        elif line.startswith("s"):
            ident, url, title = line[1:].split(None, 2)
            if not usable(url):
                raise ValueError("Unsupported existing Dome bookmark URL")
            entries.append((int(ident), url, clean(html.unescape(title))))
        else:
            raise ValueError("Invalid existing Dome bookmarks")
    if not sections:
        sections[0] = "Unclassified"
    return [(sections[ident], url, title) for ident, url, title in entries], list(sections.values())


def vivaldi_bookmarks(path):
    result, sections = [], []
    skipped = 0
    def walk(node, folders):
        nonlocal skipped
        if node.get("type") == "url":
            url = node.get("url", "")
            if usable(url):
                result.append(("Vivaldi / " + " / ".join(folders), url,
                               clean(node.get("name") or url)))
            else:
                skipped += 1
            return
        name = clean(node.get("name") or "Unclassified")
        folders = folders + [name]
        section = "Vivaldi / " + " / ".join(folders)
        if section not in sections:
            sections.append(section)
        for child in node.get("children", []):
            walk(child, folders)
    for key, root in json.loads(path.read_text())["roots"].items():
        if key != "trash" and isinstance(root, dict):
            walk(root, [])
    return result, sections, skipped


def merge_bookmarks(existing, imported, sections):
    entries, names = parse_bookmarks(existing)
    seen = {(section, url) for section, url, _ in entries}
    added = 0
    for name in sections:
        if name not in names:
            names.append(name)
    for section, url, title in imported:
        if (section, url) not in seen:
            entries.append((section, url, title))
            seen.add((section, url))
            added += 1
    ids = {name: i for i, name in enumerate(names)}
    text = "".join(f":s{i}: {name}\n" for i, name in enumerate(names))
    text += "".join(f"s{ids[section]} {url} {html.escape(title, quote=False)}\n"
                    for section, url, title in entries)
    return text, added


def merge_history(existing, imported):
    urls = existing.splitlines()
    if any(not usable(url) or url == "about:blank" for url in urls):
        raise ValueError("Invalid existing Dome history")
    merged = list(dict.fromkeys(urls + imported))[:2000]
    size = 0
    bounded = []
    for url in merged:
        size += len(url.encode("utf-8")) + 1
        if size > LIMIT:
            break
        bounded.append(url)
    return "".join(url + "\n" for url in bounded), len(set(bounded) - set(urls))


class Wallet:
    def __init__(self):
        from gi.repository import Gio, GLib
        self.Gio, self.GLib = Gio, GLib
        self.bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
        name = self.call("networkWallet", "()", ())
        self.handle = self.call("open", "(sxs)", (name, 0, APP))
        if self.handle < 0:
            raise ValueError("KWallet access was declined")

    def call(self, method, signature, args):
        return self.bus.call_sync("org.kde.kwalletd6", "/modules/kwalletd6", "org.kde.KWallet",
                                  method, self.GLib.Variant(signature, args), None,
                                  self.Gio.DBusCallFlags.NONE, 120000, None).unpack()[0]

    def folder(self, name):
        if not self.call("hasFolder", "(iss)", (self.handle, name, APP)):
            if not self.call("createFolder", "(iss)", (self.handle, name, APP)):
                raise ValueError("Cannot create Dome wallet folder")

    def has(self, folder, key):
        return self.call("hasEntry", "(isss)", (self.handle, folder, key, APP))

    def get(self, folder, key):
        return self.call("readPassword", "(isss)", (self.handle, folder, key, APP))

    def put(self, folder, key, value):
        if self.call("writePassword", "(issss)", (self.handle, folder, key, value, APP)) != 0:
            raise ValueError("KWallet rejected a migrated login")
        # Verify without printing, logging or exporting the value.
        if self.get(folder, key) != value:
            raise ValueError("Migrated wallet entry failed verification")


def decrypt(blob, key):
    from cryptography.hazmat.primitives.ciphers import Cipher, algorithms, modes
    from cryptography.hazmat.primitives.padding import PKCS7
    cipher = Cipher(algorithms.AES(key), modes.CBC(b" " * 16)).decryptor()
    padded = cipher.update(blob[3:]) + cipher.finalize()
    unpad = PKCS7(128).unpadder()
    return (unpad.update(padded) + unpad.finalize()).decode("utf-8")


def login_key(row):
    # Preserve Dome's secure origin and exact form-action scoping policy.
    if row["scheme"] != 0:
        return None, "non_web_login"
    site = origin(row["origin_url"])
    if not site:
        return None, "insecure_or_unsupported_origin"
    action = row["action_url"] or row["origin_url"]
    if origin(action) != site:
        return None, "cross_origin_form_action"
    scope = site + (urlsplit(action).path or "/")
    return json.dumps(["html", site, scope, row["username_value"]], ensure_ascii=False, separators=(",", ":")), None


def migrate_passwords(source, target, wallet_factory=Wallet):
    counts = collections.Counter()
    with snapshot(source / "Login Data") as db:
        db.row_factory = sqlite3.Row
        rows = list(db.execute("SELECT * FROM logins WHERE blacklisted_by_user=0 AND length(password_value)>0 ORDER BY date_password_modified DESC, date_created DESC"))
    wallet = wallet_factory()
    keys = {b"v10": hashlib.pbkdf2_hmac("sha1", b"peanuts", b"saltysalt", 1, 16)}
    v11 = [row["password_value"] for row in rows if row["password_value"][:3] == b"v11"]
    if v11:
        candidates = []
        for product in ("Vivaldi", "Chrome", "Chromium"):
            folder, entry = product + " Keys", product + " Safe Storage"
            if wallet.has(folder, entry):
                secret = wallet.get(folder, entry)
                key = hashlib.pbkdf2_hmac("sha1", secret.encode(), b"saltysalt", 1, 16)
                try:
                    for blob in v11:
                        decrypt(blob, key)
                except (ValueError, UnicodeError):
                    continue
                if key not in candidates:
                    candidates.append(key)
        if len(candidates) != 1:
            raise ValueError("Cannot identify a single working Vivaldi KWallet encryption key")
        keys[b"v11"] = candidates[0]
    id_path = target / "wallet-id"
    identity = read(id_path) if id_path.exists() else uuid.uuid4().hex
    if len(identity) != 32 or any(c not in "0123456789abcdefABCDEF" for c in identity):
        raise ValueError("Invalid Dome wallet profile identity")
    if not id_path.exists():
        atomic(id_path, identity)
    folder = "Dome Browser " + identity
    wallet.folder(folder)
    for row in rows:
        key, reason = login_key(row)
        if reason:
            counts[reason] += 1
            continue
        if wallet.has(folder, key):
            counts["existing_preserved"] += 1
            continue
        blob = row["password_value"]
        if blob[:3] not in keys:
            counts["unsupported_encryption"] += 1
            continue
        try:
            password = decrypt(blob, keys[blob[:3]])
        except (ValueError, UnicodeError):
            counts["decryption_failed"] += 1
            continue
        value = json.dumps({"username": row["username_value"], "password": password}, ensure_ascii=False)
        wallet.put(folder, key, value)
        counts["imported"] += 1
    return dict(counts)


def migrate(source, target, passwords=False):
    target.mkdir(parents=True, exist_ok=True, mode=0o700)
    with (target / "browser.lock").open("a") as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            raise ValueError("Close Dome Browser before migrating its profile") from None
        imported, sections, skipped = vivaldi_bookmarks(source / "Bookmarks")
        bookmark_text, added = merge_bookmarks(read(target / "bm.txt"), imported, sections)
        with snapshot(source / "History") as db:
            history = [r[0] for r in db.execute("SELECT url FROM urls ORDER BY last_visit_time DESC, id DESC")
                       if usable(r[0]) and r[0] != "about:blank"]
        history = list(dict.fromkeys(history))
        history_text, history_added = merge_history(read(target / "hs.txt"), history)
        if len(bookmark_text.encode()) > LIMIT:
            raise ValueError("Imported bookmarks exceed Dome's file limit")
        backup = target / "migration-backups" / (time.strftime("%Y%m%d-%H%M%S") + "-" + uuid.uuid4().hex[:8])
        backup.mkdir(parents=True, mode=0o700)
        backup.parent.chmod(0o700)
        for name in ("bm.txt", "hs.txt", "wallet-id"):
            if (target / name).exists():
                shutil.copyfile(target / name, backup / name)
                (backup / name).chmod(0o600)
        atomic(backup / "history-vivaldi-full.txt", "".join(u + "\n" for u in history), limit=64*1024*1024)
        atomic(target / "bm.txt", bookmark_text)
        atomic(target / "hs.txt", history_text)
        report = {"bookmarks_added": added, "bookmarks_unsupported": skipped,
                  "history_source_eligible": len(history), "history_added": history_added,
                  "history_active_total": len(history_text.splitlines()), "backup": str(backup)}
        if passwords:
            try:
                report["passwords"] = migrate_passwords(source, target)
            except Exception:
                # Exception text from dependencies could contain request arguments.
                report["password_error"] = "Password migration did not finish; no plaintext export was created. Existing entries are preserved; rerunning is safe."
        atomic(backup / "report.json", json.dumps(report, indent=2) + "\n")
        return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, default=Path.home()/".config/vivaldi/Default")
    parser.add_argument("--target", type=Path, default=Path(os.environ.get("XDG_DATA_HOME", Path.home()/".local/share"))/"dome-browser")
    parser.add_argument("--passwords", action="store_true", help="Import supported web passwords directly via KWallet (requires PyGObject and cryptography)")
    args = parser.parse_args()
    try:
        report = migrate(args.source, args.target, args.passwords)
        print(json.dumps(report, indent=2))
        if "password_error" in report:
            parser.exit(2)
    except Exception:
        parser.exit(1, "Migration stopped. Check profile availability and input file validity; Vivaldi was not modified.\n")


if __name__ == "__main__":
    main()
