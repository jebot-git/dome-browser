"""Assertions for native XR buffer allocation, frame clearing and submission."""
from PIL import Image


def validate_attributes(check, events, captures, wait):
    inline = [e['data'] for e in events if e['type'] == 'inline']
    check(len(inline) == 4, 'Both inline WebGL versions exercise both antialias requests')
    for item in inline:
        label = item['version'] + ' inline AA request ' + str(item['requested'])
        check(item['antialias'] == item['actual']['antialias'], label + ': layer reports actual default-buffer antialiasing')
        check(item['ignoreDepthValues'] and item['framebuffer'] is None and item['size'] == [80, 40], label + ': layer uses the existing canvas and has no compositor depth')

    allocations = [e['data'] for e in events if e['type'] == 'allocation']
    check(len(allocations) == 32, 'All layer allocations report preserved binding state')
    for item in allocations:
        check(item['texturePreserved'] and item['unitPreserved'] and item['unpackPreserved'] and item['error'] == 0, item['version'] + ' marker ' + str(item['marker']) + ': allocation preserves texture unit, texture and pixel-unpack buffer')

    frames = sorted([e['data'] for e in events if e['type'] == 'buffer'], key=lambda v: (v['marker'], v['index']))
    after = {(e['data']['marker'], e['data']['index']): e['data'] for e in events if e['type'] == 'after'}
    check(len(frames) == 96 and len(after) == 96, 'All 32 layer configurations complete three frames')
    for item in frames:
        label = item['version'] + ' marker ' + str(item['marker']) + ' frame ' + str(item['index'])
        options = item['options']
        check(item['status'] == 0x8CD5 and item['error'] == 0 and item['size'] == item['initialSize'], label + ': complete allocation retains its dimensions without GL errors')
        check(item['antialias'] == (item['samples'] > 0), label + ': antialias property reflects actual sample storage')
        check(item['ignoreDepthValues'], label + ': layer reports that compositor depth is unused')
        alpha, depth, stencil = item['bits']
        check((alpha > 0) == options['alpha'], label + ': color storage has alpha exactly when requested')
        check((not options['depth'] or depth > 0) and (not options['stencil'] or stencil > 0) and (options['depth'] or options['stencil'] or (depth == 0 and stencil == 0)), label + ': requested depth/stencil storage is available')
        check(item['initial'] == [0, 0, 0, 0 if options['alpha'] else 255], label + ': frame starts with cleared RGB/RGBA contents')
        state = item['state']
        check(state['colorMask'] == [False] * 4 and not state['depthMask'] and state['frontMask'] == 0 and state['backMask'] == 3 and state['scissor'] and state['scissorBox'] == [0] * 4 and state['clearDepth'] == 0 and state['clearStencil'] == 99 and all(abs(a-b) < 0.00001 for a, b in zip(state['clearColor'], [.2, .3, .4, .5])), label + ': implicit clearing restores independent application masks and clear state')
        value = 64 if options['alpha'] else 255
        expected = [[value, 0, item['marker'], 64 if options['alpha'] else 255], [0, value, item['marker'], 64 if options['alpha'] else 255]]
        check(item['pixels'] == expected, label + ': real drawing passes cleared depth/stencil and uses the requested color format')
        completed = after[(item['marker'], item['index'])]
        check(completed['size'] == item['initialSize'] and completed['error'] == 0, label + ': frame completion preserves allocation metadata without errors')

    for phase in [v for v in frames if v['index'] == 2]:
        label = phase['version'] + ' marker ' + str(phase['marker'])
        capture = wait(lambda: next((c for c in reversed(captures) if c['marker'] == phase['marker']), None))
        header = capture['header']
        check(header['layer_flags'] == (2 if phase['options']['alpha'] else 0), label + ': native compositor blend flags match the color format')
        image = Image.open(capture['path'])
        check([image.width, image.height] == phase['size'], label + ': native surface dimensions match the rendered allocation')
        for index, view in enumerate(phase['views']):
            pixel = image.getpixel((view['x'] + view['width'] // 2, image.height - view['y'] - 1))
            check(list(pixel) == phase['pixels'][index], label + ': ' + view['eye'] + ' submitted color and alpha match WebGL pixels')
