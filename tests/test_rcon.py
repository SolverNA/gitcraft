import struct
import unittest
from unittest.mock import patch
from minecraft_gitops.rcon import RconClient, RconError


class RconTests(unittest.TestCase):
    def test_unknown_online_fails_closed(self):
        client = RconClient('localhost', 25575, 'secret')
        with patch.object(client, 'command', return_value='unknown command'):
            with self.assertRaises(RconError):
                client.online()

    def test_online_parses_vanilla(self):
        client = RconClient('localhost', 25575, 'secret')
        with patch.object(client, 'command', return_value='There are 2 of a max of 20 players online: A, B'):
            self.assertEqual(client.online(), 2)

    def test_truncated_packet_rejected(self):
        class Socket:
            chunks = iter([struct.pack('<i', 10), b'abc', b''])
            def recv(self, size):
                return next(self.chunks)
        with self.assertRaises(RconError):
            RconClient('localhost', 1, '')._packet(Socket())
