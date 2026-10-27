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
})();
