"""Read a bounded root-window strip through a retained public X11 connection.

No application hooks or input injection. Keeping the connection and limiting
XGetImage to the observed strip avoids reopening XCB and copying a full screen
between frames of a 100 ms animation.
"""
import ctypes as C
from PIL import Image


class XImage(C.Structure):
    _fields_ = [(name, C.c_int) for name in
                ('width', 'height', 'xoffset', 'format')] + [
        ('data', C.c_void_p)] + [(name, C.c_int) for name in
                ('byte_order', 'bitmap_unit', 'bitmap_bit_order', 'bitmap_pad',
                 'depth', 'bytes_per_line', 'bits_per_pixel')] + [
        (name, C.c_ulong) for name in ('red_mask', 'green_mask', 'blue_mask')]


def decode(image, width, height):
    # The CI Xvfb screen is explicitly 24-bit, little-endian BGRX. Reject an
    # unexpected layout rather than guessing and accepting unrelated colors.
    assert (image.width, image.height, image.xoffset) == (width, height, 0)
    assert (image.format, image.depth, image.bits_per_pixel, image.byte_order) == (2, 24, 32, 0)
    assert (image.red_mask, image.green_mask, image.blue_mask) == (0xff0000, 0xff00, 0xff)
    assert width > 0 and height > 0 and image.data
    assert width * 4 <= image.bytes_per_line <= width * 4 + 64
    size = image.bytes_per_line * height
    assert size <= 64 * 1024 * 1024
    return Image.frombytes('RGB', (width, height), C.string_at(image.data, size),
                           'raw', 'BGRX', image.bytes_per_line, 1)


class X11StripCapture:
    def __init__(self, display, box):
        x, y, right, bottom = box
        assert 0 <= x < right and 0 <= y < bottom
        self.box = box
        self.lib = C.CDLL('libX11.so.6')
        self.lib.XOpenDisplay.argtypes = [C.c_char_p]
        self.lib.XOpenDisplay.restype = C.c_void_p
        self.lib.XDefaultRootWindow.argtypes = [C.c_void_p]
        self.lib.XDefaultRootWindow.restype = C.c_ulong
        self.lib.XGetImage.argtypes = [C.c_void_p, C.c_ulong, C.c_int, C.c_int,
                                      C.c_uint, C.c_uint, C.c_ulong, C.c_int]
        self.lib.XGetImage.restype = C.POINTER(XImage)
        self.lib.XDestroyImage.argtypes = [C.POINTER(XImage)]
        self.lib.XDestroyImage.restype = C.c_int
        self.lib.XCloseDisplay.argtypes = [C.c_void_p]
        self.lib.XCloseDisplay.restype = C.c_int
        self.display = self.lib.XOpenDisplay(display.encode())
        assert self.display, 'Could not open the observed X11 display'
        self.root = self.lib.XDefaultRootWindow(self.display)

    def capture(self):
        assert self.display, 'Capture connection is closed'
        x, y, right, bottom = self.box
        pointer = self.lib.XGetImage(self.display, self.root, x, y, right - x, bottom - y,
                                    C.c_ulong(-1).value, 2)
        assert pointer, 'XGetImage did not return a native screen frame'
        try:
            return decode(pointer.contents, right - x, bottom - y)
        finally:
            self.lib.XDestroyImage(pointer)

    def close(self):
        if self.display:
            self.lib.XCloseDisplay(self.display)
            self.display = None
