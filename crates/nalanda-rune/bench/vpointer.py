"""A virtual pointer (zwlr_virtual_pointer_v1) over the raw Wayland wire protocol, no dependencies.

Connects to $WAYLAND_DISPLAY, creates a pointer on the default seat, and can move it to an
absolute position and send wheel notches, each followed by a frame, as a real mouse does.
"""
import os, socket, struct, time


def _string(s):
    b = s.encode() + b"\0"
    return struct.pack("<I", len(b)) + b + b"\0" * (-len(b) % 4)


class VPointer:
    def __init__(self, extent=(1280, 820)):
        path = os.path.join(os.environ["XDG_RUNTIME_DIR"], os.environ["WAYLAND_DISPLAY"])
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.connect(path)
        self.next_id = 2
        self.extent = extent
        registry = self._new()
        self._send(1, 1, struct.pack("<I", registry))  # wl_display.get_registry
        callback = self._new()
        self._send(1, 0, struct.pack("<I", callback))  # wl_display.sync
        globals_ = {}
        buf = b""
        done = False
        while not done:
            buf += self.sock.recv(65536)
            while len(buf) >= 8:
                obj, word = struct.unpack_from("<II", buf)
                size, opcode = word >> 16, word & 0xFFFF
                if len(buf) < size:
                    break
                body, buf = buf[8:size], buf[size:]
                if obj == registry and opcode == 0:
                    name, n = struct.unpack_from("<II", body)
                    iface = body[8:8 + n - 1].decode()
                    globals_[iface] = name
                elif obj == callback:
                    done = True
        name = globals_["zwlr_virtual_pointer_manager_v1"]
        self.manager = self._new()
        iface = "zwlr_virtual_pointer_manager_v1"
        self._send(registry, 0, struct.pack("<I", name) + _string(iface) + struct.pack("<II", 1, self.manager))
        self.ptr = self._new()
        self._send(self.manager, 0, struct.pack("<II", 0, self.ptr))  # create_virtual_pointer(null seat)
        time.sleep(0.2)

    def _new(self):
        i = self.next_id
        self.next_id += 1
        return i

    def _send(self, obj, opcode, args=b""):
        self.sock.sendall(struct.pack("<II", obj, ((8 + len(args)) << 16) | opcode) + args)

    @staticmethod
    def _ms():
        return int(time.monotonic() * 1000) & 0xFFFFFFFF

    def move_to(self, x, y):
        self._send(self.ptr, 1, struct.pack("<IIIII", self._ms(), int(x), int(y), *self.extent))
        self._send(self.ptr, 4)  # frame

    def wheel(self, notches=1):
        """Positive scrolls down, one notch = 15 px of axis value, as libinput reports a wheel."""
        self._send(self.ptr, 5, struct.pack("<I", 0))  # axis_source(wheel)
        self._send(self.ptr, 7, struct.pack("<IIii", self._ms(), 0, int(15 * notches * 256), notches))
        self._send(self.ptr, 4)  # frame

    def close(self):
        self._send(self.ptr, 8)
        self.sock.close()


if __name__ == "__main__":
    import sys
    p = VPointer()
    p.move_to(640, 400)
    time.sleep(0.3)
    for _ in range(int(sys.argv[1]) if len(sys.argv) > 1 else 10):
        p.wheel(1)
        time.sleep(0.05)
    time.sleep(0.3)
    p.close()
