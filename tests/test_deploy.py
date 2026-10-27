import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
from minecraft_gitops import deploy
from minecraft_gitops.artifacts import Artifact
from minecraft_gitops.rcon import RconError


class DeployTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.repo = self.root / 'snapshot'
        (self.repo / 'server-settings').mkdir(parents=True)
        (self.repo / 'server-settings/server.properties').write_text('motd=new')
        self.jar = self.root / 'new.jar'
        self.jar.write_bytes(b'new')
        self.config = {'paths':{key:str(self.root/key) for key in ('server','state','cache','staging','backups','repository')}, 'repository':{'url':'https://example.org/repo.git','branch':'main'}, 'deployment':{'service':'minecraft.service','empty_grace_seconds':0,'health_timeout_seconds':0}, 'rcon':{'password_env':'TEST_RCON'}}
        for target, value in [('minecraft_gitops.deploy.gitrepo.snapshot', ('abc',self.repo)), ('minecraft_gitops.deploy.artifacts.load_manifest', [Artifact('engine','server.jar','https://example.org/x.jar','0'*64,'1')]), ('minecraft_gitops.deploy.artifacts.download',self.jar)]:
            mock = patch(target, return_value=value)
            mock.start()
            self.addCleanup(mock.stop)
        env = patch.dict(os.environ, {'TEST_RCON':'secret'})
        env.start(); self.addCleanup(env.stop)
        self.rcon_patch = patch('minecraft_gitops.deploy.RconClient')
        self.client = self.rcon_patch.start().return_value
        self.addCleanup(self.rcon_patch.stop)
        self.client.online.return_value = 0
        self.service_patch = patch('minecraft_gitops.deploy.systemctl')
        self.service = self.service_patch.start()
        self.addCleanup(self.service_patch.stop)

    def test_online_defers_without_stopping(self):
        self.client.online.return_value = 1
        self.assertEqual(deploy.run(self.config)['status'], 'waiting_for_players')
        self.service.assert_not_called()

    def test_unknown_online_never_stops(self):
        self.client.online.side_effect = RconError('unknown')
        with self.assertRaises(RconError): deploy.run(self.config)
        self.service.assert_not_called()

    def test_success_tracks_owned_files_only(self):
        server = self.root / 'server'
        (server / 'world').mkdir(parents=True)
        (server / 'world/level.dat').write_bytes(b'world')
        result = deploy.run(self.config)
        self.assertEqual(result['status'], 'deployed')
        self.assertEqual((server / 'world/level.dat').read_bytes(), b'world')
        self.assertEqual((server / 'server.jar').read_bytes(), b'new')
        state = json.loads((self.root / 'state/deployment.json').read_text())
        self.assertEqual(state['commit'], 'abc')

    def test_readiness_failure_restores_old_files_and_no_commit(self):
        server = self.root / 'server'
        server.mkdir()
        (server / 'server.jar').write_bytes(b'old')
        with patch('minecraft_gitops.deploy.wait_healthy', side_effect=RuntimeError('failed')):
            with self.assertRaises(RuntimeError): deploy.run(self.config)
        self.assertEqual((server / 'server.jar').read_bytes(), b'old')
        self.assertFalse((server / 'server.properties').exists())
        self.assertFalse((self.root / 'state/deployment.json').exists())

    def test_symlink_target_never_stops(self):
        server = self.root / 'server'
        server.mkdir()
        (server / 'server.properties').symlink_to(self.jar)
        with self.assertRaises(ValueError): deploy.run(self.config)
        self.service.assert_not_called()

    def test_backup_failure_keeps_original_files(self):
        server = self.root / 'server'
        server.mkdir()
        (server / 'server.jar').write_bytes(b'old')
        with patch('minecraft_gitops.deploy.shutil.copy2', side_effect=OSError('disk full')):
            with self.assertRaises(OSError): deploy.run(self.config)
        self.assertEqual((server / 'server.jar').read_bytes(), b'old')

    def test_owned_removed_plugin_deleted_but_foreign_preserved(self):
        server = self.root / 'server/plugins'
        server.mkdir(parents=True)
        (server / 'old.jar').write_bytes(b'owned')
        (server / 'foreign.jar').write_bytes(b'foreign')
        state = self.root / 'state'
        state.mkdir()
        (state / 'deployment.json').write_text(json.dumps({'commit':'old','managed_files':['plugins/old.jar']}))
        deploy.run(self.config)
        self.assertFalse((server / 'old.jar').exists())
        self.assertEqual((server / 'foreign.jar').read_bytes(), b'foreign')
