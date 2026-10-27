"""Small, bounded Source RCON client. Unknown responses never imply zero players."""
import re
import socket
import struct


class RconError(RuntimeError):
    pass


class RconClient:
    def __init__(self, host, port, password, timeout=5):
        self.host, self.port, self.password, self.timeout = host, port, password, timeout

    @staticmethod
    def _read(sock, size):
        data = b''
        while len(data) < size:
            chunk = sock.recv(size - len(data))
            if not chunk:
                raise RconError('RCON connection closed')
            data += chunk
        return data

    def _packet(self, sock):
        size = struct.unpack('<i', self._read(sock, 4))[0]
        if not 10 <= size <= 4 * 1024 * 1024:
            raise RconError('Invalid RCON packet size')
        data = self._read(sock, size)
        if data[-2:] != b'\0\0':
            raise RconError('Invalid RCON packet terminator')
        request, kind = struct.unpack('<ii', data[:8])
        return request, kind, data[8:-2].decode('utf-8', errors='strict')

    @staticmethod
    def _send(sock, request, kind, body):
        payload = struct.pack('<ii', request, kind) + body.encode() + b'\0\0'
        sock.sendall(struct.pack('<i', len(payload)) + payload)

    def command(self, command):
        try:
            with socket.create_connection((self.host, self.port), self.timeout) as sock:
                sock.settimeout(self.timeout)
                self._send(sock, 1, 3, self.password)
                for _ in range(3):
                    request, kind, _ = self._packet(sock)
                    if request == -1:
                        raise RconError('RCON authentication failed')
                    if request == 1 and kind == 2:
                        break
                else:
                    raise RconError('Missing RCON authentication response')
                self._send(sock, 2, 2, command)
                request, kind, body = self._packet(sock)
                if request != 2 or kind != 0:
                    raise RconError('Unexpected RCON command response')
                return body
        except (OSError, UnicodeError, struct.error) as error:
            raise RconError('RCON communication failed') from error

    def online(self):
        response = re.sub(r'§.', '', self.command('minecraft:list'))
        match = re.fullmatch(r'\s*There are (\d+) of a max of (\d+) players online:.*', response, re.S)
        if not match:
            raise RconError('Unrecognized player count response')
        count, maximum = map(int, match.groups())
        if count > maximum:
            raise RconError('Invalid player count')
        return count
