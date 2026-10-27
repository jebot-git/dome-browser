#!/usr/bin/env python3
"""Read-only loopback wrapper around the author's unmodified inetd daemon.

Supply a local copy of the pinned upstream source. Only the upstream-supported
configuration file and a fixture document generator are created here.
"""
import hashlib
import json
from pathlib import Path
import socketserver
import subprocess
import threading

REVISION = '2847eb5f07d1a6e74afe24754829d5e16bcd4fdc'
SOURCE_HASHES = {
    'scorpiond.c': 'df90fe694de59fa7e771dbd3550a0adecfc2319042862ddd7794094dbee78cc7',
    'scorpionlib.c': '2085686719650fbd0015d68501c9c70ee544ea6c77340ff1589588f5b7d0ac21',
    'scorpionlib.h': '8237e024db2e73fa52bff6023cd15a1d6ff57ca0fe87f0324ac07258a16fc26c',
}


class ReferenceFixture(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True

    def __init__(self, source, root, output):
        source, root, output = Path(source), Path(root), Path(output)
        self.root = root / 'reference'
        self.root.mkdir(mode=0o700)
        build = self.root / 'build'
        build.mkdir()
        hashes = {}
        for name in ['scorpiond.c', 'scorpionlib.c', 'scorpionlib.h']:
            data = (source / name).read_bytes()
            hashes[name] = hashlib.sha256(data).hexdigest()
            if hashes[name] != SOURCE_HASHES[name]:
                raise ValueError('Source differs from the pinned reference revision: ' + name)
            (build / name).write_bytes(data)
        (build / 'scorpiond.config').write_text('''
#define CONFIG_MAX_REQUEST 0x800
#define CONFIG_MAX_NAME 0x100
#define CONFIG_ROOT "."
#define CONFIG_TIMELIMIT 10
#define CONFIG_UNEXPECTED_ERROR_TIME "?"
#define CONFIG_DEFAULT_MIMETYPE ":"
#define CONFIG_DEFAULT_FILENAME "index.scorp"
#define CONFIG_DIRLIST 1
#define CONFIG_FILENAME_CHARSET 0x10
#define CONFIG_SAFE_FILENAMES 1
#define CONFIG_FILEMODE_ACCESS (S_IRGRP|S_IROTH)
#define CONFIG_BUF_SIZE 0x2000
static const MimeDef mimetypes[]={
  {"scorp","document:scorpion"},
  {"txt","text/plain"},
};
''')
        (build / 'document.c').write_text('''
#include <stdio.h>
#include <string.h>
#include "scorpionlib.h"
static void block(FILE *out, int kind, const char *attribute, const char *body) {
  scorpionlib_write_block(out, SCORPIONLIB_CHARSET_PC | kind,
      attribute, strlen(attribute), body, strlen(body));
}
int main(int argc, char **argv) {
  if(argc != 3) return 2;
  FILE *out=fopen(argv[1],"wb");
  if(!out) return 3;
  if(strcmp(argv[2],"child")==0) {
    block(out,SCORPIONLIB_BLOCK_HEAD1,"","Child fixture");
    block(out,SCORPIONLIB_BLOCK_HYPERLINK,"/index.scorp#start","Return");
  } else {
    block(out,SCORPIONLIB_BLOCK_HEAD1,"start","Scorpion fixture");
    block(out,SCORPIONLIB_BLOCK_NORMAL,"","Reference C document: Caf\\202 <literal> & text");
    block(out,SCORPIONLIB_BLOCK_PREFORMATTED,"","left\\tright\\nA  B");
    block(out,SCORPIONLIB_BLOCK_HYPERLINK,"/child.scorp","Open child");
  }
  return fclose(out) ? 4 : 0;
}
''')
        commands = [
            ['gcc', '-O2', '-march=native', '-fwrapv', '-o', 'scorpiond', 'scorpiond.c'],
            ['gcc', '-O2', '-march=native', '-fwrapv', '-o', 'document', 'document.c', 'scorpionlib.c'],
        ]
        with (output / 'reference-build.log').open('w') as log:
            for command in commands:
                subprocess.run(command, cwd=build, check=True, stdout=log, stderr=log)
        self.data = self.root / 'data'
        host = self.data / 'localhost'
        host.mkdir(parents=True)
        for filename, mode in [('index.scorp', 'index'), ('child.scorp', 'child')]:
            subprocess.run([str(build / 'document'), str(host / filename), mode], check=True)
        (host / 'plain.txt').write_text('Independent reference daemon plain text.\n')
        for file in host.iterdir():
            file.chmod(0o644)
        self.document = (host / 'index.scorp').read_bytes()
        self.executable = build / 'scorpiond'
        self.records = []
        self.errors = []
        self.mode = 'normal'
        self.processes = set()
        self.lock = threading.Lock()
        self.started = threading.Event()
        self.closed = threading.Event()
        metadata = {'upstream': 'https://github.com/zzo38/scorpion',
                    'source_revision': REVISION, 'source_sha256': hashes,
                    'commands': commands, 'source_modified': False,
                    'configuration': (build / 'scorpiond.config').read_text(),
                    'generator': (build / 'document.c').read_text(),
                    'document_sha256': hashlib.sha256(self.document).hexdigest(),
                    'scope': 'Read-only R retrieval on loopback; no TLS or authentication claim'}
        (output / 'reference-provenance.json').write_text(json.dumps(metadata, indent=2) + '\n')
        super().__init__(('127.0.0.1', 0), ReferenceHandler)
        self.thread = threading.Thread(target=self.serve_forever, daemon=True)
        self.thread.start()

    @property
    def port(self):
        return self.server_address[1]

    def close(self):
        self.shutdown()
        with self.lock:
            children = list(self.processes)
        for child in children:
            if child.poll() is None:
                child.terminate()
        self.server_close()
        self.thread.join(timeout=3)


class ReferenceHandler(socketserver.BaseRequestHandler):
    def handle(self):
        fixture = self.server
        # Pass the connected socket directly to the inetd daemon. No request
        # parsing, response framing or document generation occurs in Python.
        with self.request:
            child = subprocess.Popen([str(fixture.executable), str(fixture.data)],
                                     stdin=self.request, stdout=self.request,
                                     stderr=subprocess.DEVNULL)
            with fixture.lock:
                fixture.processes.add(child)
                fixture.records.append({'reference_daemon': True})
            try:
                result = child.wait(timeout=12)
                if result:
                    fixture.errors.append('reference daemon exit ' + str(result))
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait()
                fixture.errors.append('reference daemon exceeded fixture timeout')
            finally:
                with fixture.lock:
                    fixture.processes.discard(child)
