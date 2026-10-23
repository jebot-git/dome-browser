// SPDX-License-Identifier: GPL-3.0-or-later
(() => {
    if (location.protocol !== "https:" ||
        !(location.hostname === "discord.com" || location.hostname.endsWith(".discord.com"))) return;
    if (typeof globalThis.requestIdleCallback !== "function") {
        const pending = new Map();
        let next = 1;
        globalThis.requestIdleCallback = function(callback, options = {}) {
            if (typeof callback !== "function") throw new TypeError("callback must be a function");
            const id = next++;
            const queued = performance.now();
            const timeout = options.timeout === undefined ? Infinity : Math.max(0, Number(options.timeout) || 0);
            const timer = setTimeout(() => {
                pending.delete(id);
                const start = performance.now();
                callback({didTimeout: start - queued >= timeout,
                    timeRemaining: () => Math.max(0, 8 - (performance.now() - start))});
            }, 1);
            pending.set(id, timer);
            return id;
        };
        globalThis.cancelIdleCallback = function(id) {
            if (!pending.has(id)) return;
            clearTimeout(pending.get(id));
            pending.delete(id);
        };
    }
    // Discord's WASM loader assumes CacheStorage exists. Servo's Cache API
    // currently has keys() only, so enabling ServiceWorker is insufficient.
    // Supply a deliberately empty cache: assets still use normal fetch/CSP,
    // and every lookup misses. This provides no offline or persistent cache.
    if (typeof globalThis.caches === "undefined" && typeof globalThis.Cache === "undefined") {
        class Cache {
            async match() { return undefined; }
            async matchAll() { return []; }
            async put() {}
            async delete() { return false; }
            async keys() { return []; }
        }
        class CacheStorage {
            #names = new Map();
            async open(name) {
                name = String(name);
                if (!this.#names.has(name)) this.#names.set(name, new Cache());
                return this.#names.get(name);
            }
            async has(name) { return this.#names.has(String(name)); }
            async delete(name) { return this.#names.delete(String(name)); }
            async keys() { return [...this.#names.keys()]; }
            async match() { return undefined; }
        }
        globalThis.Cache = Cache;
        globalThis.CacheStorage = CacheStorage;
        globalThis.caches = new CacheStorage();
    }

})();
