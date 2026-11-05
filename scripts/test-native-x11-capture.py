#!/usr/bin/env python3
"""Pixel byte-layout negatives. Actual X11 reads are exercised by the Vim smoke."""
import ctypes as C
from native_x11_capture import XImage, decode

pixels = C.create_string_buffer(bytes([0x50, 0x40, 0x30, 0, 0, 0, 0, 0,
                                      0xa7, 0xc3, 0x19, 0, 0, 0, 0, 0]))
image = XImage(width=1, height=2, xoffset=0, format=2,
               data=C.addressof(pixels), byte_order=0, depth=24,
               bytes_per_line=8, bits_per_pixel=32,
               red_mask=0xff0000, green_mask=0xff00, blue_mask=0xff)
decoded = decode(image, 1, 2)
assert [decoded.getpixel((0, y)) for y in range(2)] == [(0x30, 0x40, 0x50), (0x19, 0xc3, 0xa7)]
for field, wrong in [('byte_order', 1), ('bits_per_pixel', 24), ('depth', 32),
                     ('red_mask', 0xff), ('width', 2), ('bytes_per_line', 3), ('data', 0)]:
    old = getattr(image, field)
    setattr(image, field, wrong)
    try:
        decode(image, 1, 2)
    except AssertionError:
        pass
    else:
        raise AssertionError('Unsupported image layout accepted: ' + field)
    setattr(image, field, old)
print('Native X11 strip pixel decoding and seven invalid-layout controls passed')
