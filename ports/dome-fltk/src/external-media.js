(() => {
    const found = [];
    const seen = new Set();
    const visit = doc => {
        for (const media of doc.querySelectorAll('video,audio')) {
            const failed = media.error?.code === 3 || media.error?.code === 4;
            const sources = [...media.querySelectorAll('source')];
            if (media.currentSrc || media.src) sources.unshift(media);
            for (const source of sources) {
                const raw = source === media ? (media.currentSrc || media.getAttribute('src')) : source.getAttribute('src');
                if (!raw) continue;
                const mime = source.getAttribute('type') || '';
                const unsupported = failed || (!!mime && media.canPlayType(mime) === '');
                if (!unsupported) continue;
                try {
                    const url = new URL(raw, doc.baseURI);
                    if (!['http:', 'https:'].includes(url.protocol) || seen.has(url.href)) continue;
                    seen.add(url.href);
                    found.push({url:url.href, mime, reason:failed ? 'Playback unsupported' : 'Unsupported media type'});
                } catch (_) {}
                if (found.length >= 24) return;
            }
        }
        for (const frame of doc.querySelectorAll('iframe,frame')) {
            try { if (frame.contentDocument) visit(frame.contentDocument); } catch (_) {}
            if (found.length >= 24) return;
        }
    };
    visit(document);
    return JSON.stringify(found.slice(0,24));
})()
