"""Validate dynamic viewport state against the native submitted pixel regions."""
import math
from PIL import Image


def validate_dynamic(check, events, captures, wait):
    assertions = [e['data'] for e in events if e['type'] == 'assertion']
    for assertion in assertions:
        check(assertion['passed'], assertion['name'])
    phases = sorted((e['data'] for e in events if e['type'] == 'dynamic'), key=lambda p: p['marker'])
    check(len(phases) == 36, 'Both WebGL versions complete two allocations and nine dynamic frames each')
    check(len([e for e in events if e['type'] == 'dynamic-inline']) == 2, 'Both WebGL versions complete inline scaling and frame validation')
    for phase in phases:
        label = f"{phase['version']} allocation {phase['allocation']} step {phase['step']}"
        w, h = phase['size']
        views = phase['views']
        check(phase['initialSize'] == phase['size'] and phase['error'] == 0 and phase['status'] == 0x8CD5,
              label + ': changing viewports preserves the complete GPU allocation')
        scales = ([1, 1] if phase['allocation'] == 1 else [.4, .6]) if phase['step'] == 0 else {
            1: [.5, .75], 2: [.25, .5], 3: [1, 5e-324], 4: [1, 1e-8],
            5: [.73, .31], 6: [.73, .31], 7: [.4, .6], 8: [.4, .6],
        }[phase['step']]
        split = 63 * w // 127
        expected = [dict(x=x, y=0, width=max(1, math.floor(width * scale)), height=max(1, math.floor(h * scale)))
                    for x, width, scale in [(0, split, scales[0]), (split, w-split, scales[1])]]
        check([{k: v[k] for k in ['x', 'y', 'width', 'height']} for v in views] == expected,
              label + ': independent requested scales and preserved eye origins match deterministic rounding')
        capture = wait(lambda: next((c for c in reversed(captures) if c['marker'] == phase['marker'] and
                                      [c['header']['width'], c['header']['height']] == phase['size']), None))
        image = Image.open(capture['path'])
        check([v['rect'] for v in capture['header']['views']] ==
              [dict(x=v['x'], y=h-v['y']-v['height'], width=v['width'], height=v['height']) for v in views],
              label + ': native compositor receives exactly the rendered rectangles')
        for index, v in enumerate(views):
            bottom = [255, 0, phase['marker'], 255] if index == 0 else [0, 255, phase['marker'], 255]
            upper = [255, 100, phase['marker'], 255] if index == 0 else [100, 255, phase['marker'], 255]
            if v['height'] == 1:
                bottom = upper
            x = v['x'] + v['width']//2
            check(phase['samples'][index] == bottom and list(image.getpixel((x, h-v['y']-1))) == bottom and
                  list(image.getpixel((x, h-v['y']-v['height']))) == upper,
                  label + f': eye {index} pixels and orientation survive native submission')
            full_width = split if index == 0 else w-split
            if v['width'] < full_width:
                check(list(image.getpixel((v['x']+v['width'], h-1))) == [9, 11, phase['marker'], 255],
                      label + f': eye {index} does not render into the unused region')
