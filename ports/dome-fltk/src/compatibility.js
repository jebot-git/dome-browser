// Native CSS resize handles are not implemented by this Servo baseline.
// Supply a textarea handle until the engine supports CSS resize itself.
(() => {
  'use strict';
  if (CSS.supports('resize', 'both')) return;
  const install = () => {
    const grip = document.createElement('div');
    grip.setAttribute('aria-hidden', 'true');
    grip.style.cssText = 'display:none;position:fixed;width:12px;height:12px;z-index:2147483647;cursor:se-resize;background:repeating-linear-gradient(135deg,transparent 0 3px,#888 3px 4px);clip-path:polygon(100% 0,100% 100%,0 100%);user-select:none';
    document.documentElement.appendChild(grip);
    let owner = null, drag = null;
    const modes = new WeakMap();
    const mode = box => {
      const raw = /(?:^|;)\s*resize\s*:\s*([^;!]+)/i.exec(box.getAttribute('style') || '');
      if (raw) modes.set(box, raw[1].trim().toLowerCase());
      return modes.get(box) || 'both';
    };
    const show = box => {
      if (!box || !box.isConnected || box.disabled || mode(box) === 'none') {
        owner = null; grip.style.display = 'none'; return;
      }
      owner = box;
      const r = box.getBoundingClientRect();
      grip.style.left = (r.right - 13) + 'px';
      grip.style.top = (r.bottom - 13) + 'px';
      grip.style.display = r.width > 0 && r.height > 0 ? 'block' : 'none';
    };
    const px = value => value.endsWith('px') ? parseFloat(value) : 0;
    document.addEventListener('mousemove', event => {
      if (drag) return;
      if (event.target === grip) return;
      show(event.target instanceof HTMLTextAreaElement ? event.target : null);
    }, true);
    grip.addEventListener('mousedown', event => {
      if (event.button !== 0 || !owner) return;
      event.preventDefault(); event.stopPropagation();
      const r = owner.getBoundingClientRect(), style = getComputedStyle(owner);
      const extraX = px(style.paddingLeft) + px(style.paddingRight) + px(style.borderLeftWidth) + px(style.borderRightWidth);
      const extraY = px(style.paddingTop) + px(style.paddingBottom) + px(style.borderTopWidth) + px(style.borderBottomWidth);
      const content = style.boxSizing !== 'border-box';
      drag = {box:owner, x:event.clientX, y:event.clientY,
        width:r.width - (content ? extraX : 0), height:r.height - (content ? extraY : 0),
        minWidth:Math.max(20, px(style.minWidth)), minHeight:Math.max(20, px(style.minHeight)),
        maxWidth:px(style.maxWidth) || Infinity, maxHeight:px(style.maxHeight) || Infinity,
        mode:mode(owner)};
    });
    window.addEventListener('mousemove', event => {
      if (!drag) return;
      event.preventDefault(); event.stopPropagation();
      const vertical = drag.mode === 'vertical' || drag.mode === 'block';
      const horizontal = drag.mode === 'horizontal' || drag.mode === 'inline';
      if (!vertical) drag.box.style.width = Math.min(drag.maxWidth, Math.max(drag.minWidth, drag.width + event.clientX - drag.x)) + 'px';
      if (!horizontal) drag.box.style.height = Math.min(drag.maxHeight, Math.max(drag.minHeight, drag.height + event.clientY - drag.y)) + 'px';
      show(drag.box);
    }, true);
    const finish = () => { drag = null; };
    window.addEventListener('mouseup', finish, true);
    window.addEventListener('blur', finish);
    document.addEventListener('scroll', () => { if (owner) show(owner); }, true);
    window.addEventListener('resize', () => { if (owner) show(owner); });
  };
  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', install, {once:true});
  else install();
})();
