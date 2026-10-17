#!/usr/bin/env python3
"""Run the actual Armbian helper against isolated files and synthetic commands."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

REPO = Path(__file__).resolve().parents[1]
MOCK = r'''#!/usr/bin/env python3
import configparser, json, os, sys
from pathlib import Path
root = Path(os.environ['AP_TEST_ROOT'])
name, args = Path(sys.argv[0]).name, sys.argv[1:]
if name == 'timeout': os.execvp(args[1], args[1:])
if name in ('sleep', 'flock', 'hostapd'): sys.exit(0)
state = json.loads((root / 'state.json').read_text())
with (root / 'calls').open('a') as log: log.write(name + ' ' + ' '.join(args) + '\n')
call = name + ' ' + ' '.join(args)
if state.get('fail') == call:
    print('synthetic failure: ' + call, file=sys.stderr)
    sys.exit(2)
def save(): (root / 'state.json').write_text(json.dumps(state))
if name == 'systemctl':
    op, unit = args[0], args[-1]
    if op in ('is-active', 'is-enabled'):
        key = {'NetworkManager.service': 'nm_active' if op == 'is-active' else 'nm_enabled',
               'systemd-networkd.service': 'networkd', 'netplan-wpa-wlan0.service': 'wpa' if op == 'is-active' else 'wpa_enabled',
               'sentryusb-networkd-ap.service': 'hostapd'}.get(unit)
        sys.exit(0 if key and state.get(key) else 1)
    if args == ['show', '-p', 'MainPID', '--value', 'systemd-networkd.service']:
        print(state.get('networkd_pid', 375) if state['networkd'] else 0)
    elif op == 'list-unit-files':
        for interface in state.get('netplan', []): print('netplan-wpa-' + interface + '.service generated -')
    elif op in ('start', 'stop'):
        if unit == 'netplan-wpa-wlan0.service': state['wpa'] = op == 'start'
        elif unit == 'sentryusb-networkd-ap.service': state['hostapd'] = op == 'start'
        elif unit != 'sentryusb-ap-resurrect.service': raise Exception('unexpected unit ' + unit)
        save()
    elif op != 'daemon-reload': raise Exception(call)
elif name == 'networkctl':
    if args == ['down', 'wlan0']: state['station_up'] = False
    elif args == ['up', 'wlan0']: state['station_up'] = True
    elif args == ['reconfigure', 'ap0']:
        assert state['ap']
        state['address'] = not state.get('no_address')
    elif args not in (['reload'], ['reconfigure', 'wlan0']): raise Exception(call)
    save()
elif name == 'iw':
    if args == ['dev', 'ap0', 'info']: sys.exit(0 if state['ap'] else 1)
    elif args == ['dev', 'wlan0', 'info']: print('Interface wlan0')
    elif args == ['dev', 'wlan0', 'interface', 'add', 'ap0', 'type', '__ap']:
        assert not state['wpa'], 'the station/P2P owner must release the radio first'
        state['ap'] = True
    elif args == ['dev', 'ap0', 'del']: state['ap'] = state['address'] = False
    else: raise Exception(call)
    save()
elif name == 'hostapd_cli':
    print('state=ENABLED' if state['hostapd'] and state['ap'] and not state.get('not_ready') else 'state=DISABLED')
elif name == 'ip':
    if state['address']:
        address = (root / 'etc/sentryusb/networkd-ap/ip').read_text().strip()
        print('4: ap0 inet ' + address + '/24 scope global ap0')
elif name == 'ss':
    assert args == ['-4', '-H', '-u', '-l', '-n', '-p', 'sport = :67']
    config = configparser.ConfigParser()
    config.read([root / 'etc/systemd/network/05-sentryusb-ap.network',
                 root / 'run/systemd/network/05-sentryusb-ap.network.d/90-sentryusb-dhcp.conf'])
    # Model systemd 256+: DHCP can be configured while startup waits for
    # persistent storage. A carrier and static address do not prove readiness.
    persist = config.getboolean('DHCPServer', 'PersistLeases', fallback=True)
    storage_ready = state.get('persistent_storage_ready', True) or not persist
    if state['networkd'] and state['ap'] and state['hostapd'] and state['address'] and storage_ready and not state.get('no_dhcp'):
        interface = state.get('dhcp_interface', 'ap0')
        pid = state.get('dhcp_pid', 375)
        print('UNCONN 0 0 0.0.0.0%' + interface + ':67 0.0.0.0:* users:(("systemd-network",pid=' + str(pid) + ',fd=45))')
elif name == 'journalctl':
    if 'systemd-networkd-persistent-storage.service' in args:
        print('systemd-networkd: The persistent storage is on read-only filesystem.')
    else:
        print('hostapd journal sample')
else: raise Exception(call)
'''


class NetworkdAP(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        for directory in ('etc/netplan', 'run', 'bin'):
            (self.root / directory).mkdir(parents=True)
        (self.root / 'etc/armbian-release').write_text('BOARD=radxa-zero3\n')
        self.home = self.root / 'etc/netplan/10-wifi.yaml'
        self.home.write_text('original Netplan home WiFi configuration\n')
        self.original = self.home.read_bytes()
        script = (REPO / 'setup/pi/networkd-ap.sh').read_text()
        for prefix in ('/etc', '/run', '/root'):
            script = script.replace(prefix + '/', str(self.root) + prefix + '/')
        self.script = self.root / 'helper.sh'
        self.script.write_text(script)
        for command in ('timeout', 'systemctl', 'networkctl', 'iw', 'ip', 'ss', 'hostapd', 'hostapd_cli', 'journalctl', 'sleep', 'flock'):
            path = self.root / 'bin' / command
            path.write_text(MOCK)
            path.chmod(0o755)
        self.env = dict(os.environ, AP_TEST_ROOT=str(self.root), PATH=str(self.root / 'bin') + ':' + os.environ['PATH'],
                        AP_SSID='Road: Wi\\Fi', AP_PASS='example-psk-only-for-tests', AP_IP='192.168.66.1')
        self.dhcp_override = self.root / 'run/systemd/network/05-sentryusb-ap.network.d/90-sentryusb-dhcp.conf'
        self.write_state(nm_active=False, nm_enabled=False, networkd=True, netplan=['wlan0'], wpa=True,
                         wpa_enabled=True, station_up=True, ap=False, hostapd=False, address=False)

    def state(self): return json.loads((self.root / 'state.json').read_text())
    def write_state(self, **changes):
        state = self.state() if (self.root / 'state.json').exists() else {}
        state.update(changes)
        (self.root / 'state.json').write_text(json.dumps(state))
    def run_helper(self, action, success=True, **env):
        result = subprocess.run(['bash', str(self.script), action], env=dict(self.env, **env), text=True, capture_output=True, timeout=30)
        if success: self.assertEqual(result.returncode, 0, result.stderr)
        else: self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertEqual(self.home.read_bytes(), self.original)
        self.assertNotIn(self.env['AP_PASS'], result.stderr)
        return result
    def assert_restored(self):
        state = self.state()
        self.assertTrue(state['wpa'])
        self.assertTrue(state['station_up'])
        self.assertFalse(state['ap'])
        self.assertFalse(state['hostapd'])
        self.assertFalse((self.root / 'run/sentryusb-networkd-ap-owner').exists())

    def test_excludes_unaffected_platforms_and_backends(self):
        release = self.root / 'etc/armbian-release'
        release.unlink()
        self.run_helper('configure', success=False)
        release.touch()
        for excluded in ({'nm_active': True}, {'nm_enabled': True}, {'networkd': False}, {'netplan': []}):
            before = self.state()
            self.write_state(**excluded)
            self.run_helper('configure', success=False)
            self.assertFalse((self.root / 'etc/sentryusb').exists())
            self.assertFalse(self.dhcp_override.exists())
            self.write_state(**before)
        self.assert_restored()

    def test_setup_is_inactive_private_and_idempotent(self):
        self.run_helper('configure')
        self.run_helper('configure')
        self.assertEqual(self.run_helper('info').stdout, 'Road: Wi\\Fi\n192.168.66.1\n')
        config = self.root / 'etc/sentryusb/networkd-ap/hostapd.conf'
        self.assertEqual(config.stat().st_mode & 0o777, 0o600)
        self.assertIn('ssid2=' + self.env['AP_SSID'].encode().hex(), config.read_text())
        network = (self.root / 'etc/systemd/network/05-sentryusb-ap.network').read_text()
        self.assertIn('Name=ap0', network)
        self.assertIn('DHCPServer=yes', network)
        self.assertNotIn('wlan0', network)
        self.assert_restored()

    def test_start_and_stop_restore_netplan_without_global_service_changes(self):
        self.run_helper('configure')
        self.run_helper('start')
        self.assertTrue(self.state()['ap'] and self.state()['hostapd'] and self.state()['address'])
        self.assertFalse(self.state()['wpa'])
        self.run_helper('check')
        self.run_helper('start')  # idempotent, same owner
        self.run_helper('stop')
        self.run_helper('stop')
        self.assert_restored()
        calls = (self.root / 'calls').read_text()
        for forbidden in ('netplan apply', 'systemctl stop systemd-networkd', 'systemctl start NetworkManager', 'systemctl stop hostapd.service'):
            self.assertNotIn(forbidden, calls)
        self.run_helper('remove')
        self.assertFalse((self.root / 'etc/sentryusb/networkd-ap/station').exists())
        self.assertFalse((self.root / 'etc/sentryusb/networkd-ap').exists())
        self.assertFalse(self.dhcp_override.exists())
        self.assertFalse(self.dhcp_override.parent.exists())

    def test_late_hostapd_failure_is_detectable_and_recoverable(self):
        self.run_helper('configure')
        self.run_helper('start')
        self.write_state(hostapd=False)
        self.run_helper('check', success=False)
        self.run_helper('stop')
        self.assert_restored()

    def old_profile(self):
        self.write_state(persistent_storage_ready=False)
        self.run_helper('configure', AP_IP='10.3.0.159')
        network = self.root / 'etc/systemd/network/05-sentryusb-ap.network'
        # Existing installs retain the old profile on a read-only root.
        network.write_text(network.read_text().replace('PersistLeases=no\n', '').replace('BindToInterface=yes\n', ''))
        network.chmod(0o444)
        return {p: (p.read_bytes(), p.stat().st_mtime_ns) for p in (self.root / 'etc').rglob('*') if p.is_file()}

    def assert_saved_files_unchanged(self, files):
        for path, (content, modified) in files.items():
            self.assertEqual(path.read_bytes(), content)
            self.assertEqual(path.stat().st_mtime_ns, modified)

    def test_old_profile_gets_dhcp_without_setup_or_persistent_storage(self):
        files = self.old_profile()
        self.run_helper('start')
        self.run_helper('check')
        self.assertEqual(self.dhcp_override.stat().st_mode & 0o777, 0o644)
        self.assertEqual(self.dhcp_override.parent.stat().st_mode & 0o777, 0o755)
        modified = self.dhcp_override.stat().st_mtime_ns
        self.run_helper('stop')
        self.run_helper('start')
        self.assertEqual(self.dhcp_override.stat().st_mtime_ns, modified)
        self.run_helper('check')
        self.assertFalse(self.state()['persistent_storage_ready'])
        self.assertNotIn('networkctl persistent-storage', (self.root / 'calls').read_text())
        self.run_helper('stop')
        self.assert_restored()
        self.assert_saved_files_unchanged(files)

        # /run is cleared at reboot: the next start must rebuild the fix.
        self.dhcp_override.unlink()
        self.dhcp_override.parent.rmdir()
        self.run_helper('start')
        self.run_helper('check')
        self.run_helper('stop')
        self.assert_restored()
        self.assert_saved_files_unchanged(files)

    def test_update_recovers_an_active_old_hotspot_without_manual_disable(self):
        files = self.old_profile()
        (self.root / 'run/sentryusb-networkd-ap-owner').write_text('wlan0 1\n')
        self.write_state(wpa=False, station_up=False, ap=True, hostapd=True, address=True)
        self.run_helper('start')
        self.run_helper('check')
        self.assertTrue(self.state()['ap'] and self.state()['hostapd'])
        self.assert_saved_files_unchanged(files)
        self.run_helper('stop')
        self.assert_restored()

    def test_runtime_override_failure_keeps_home_wifi_connected(self):
        files = self.old_profile()
        self.dhcp_override.parent.parent.mkdir(parents=True)
        self.dhcp_override.parent.write_text('cannot create a directory here')
        self.run_helper('start', success=False)
        self.assert_restored()
        self.assert_saved_files_unchanged(files)

    def test_dhcp_on_another_interface_or_process_does_not_count_as_ready(self):
        self.run_helper('configure')
        self.run_helper('start')
        for changes in ({'dhcp_interface': 'wlan0'}, {'dhcp_interface': 'notap0'},
                        {'dhcp_pid': 1234}, {'dhcp_pid': 3750}, {'networkd_pid': 0}):
            with self.subTest(changes=changes):
                self.write_state(dhcp_interface='ap0', dhcp_pid=375, networkd_pid=375)
                self.write_state(**changes)
                result = self.run_helper('check', success=False)
                self.assertIn('DHCP server is not listening on ap0', result.stderr)
        self.run_helper('stop')
        self.assert_restored()

    def test_late_dhcp_failure_is_detectable_and_recoverable(self):
        self.run_helper('configure')
        self.run_helper('start')
        self.write_state(no_dhcp=True)
        result = self.run_helper('check', success=False)
        self.assertIn('DHCP server is not listening on ap0', result.stderr)
        self.run_helper('stop')
        self.assert_restored()

    def test_interrupted_session_can_restore_the_station_before_retrying(self):
        self.run_helper('configure')
        (self.root / 'run/sentryusb-networkd-ap-owner').write_text('wlan0 1\n')
        self.write_state(wpa=False, station_up=False, ap=True, hostapd=False, address=True)
        self.run_helper('start')
        self.run_helper('check')
        self.run_helper('stop')
        self.assert_restored()

    def test_each_start_failure_restores_home_wifi(self):
        self.run_helper('configure')
        for failure in ('iw dev wlan0 interface add ap0 type __ap', 'networkctl reload',
                        'networkctl reconfigure ap0', 'systemctl start sentryusb-networkd-ap.service'):
            with self.subTest(failure=failure):
                self.write_state(fail=failure)
                self.run_helper('start', success=False)
                self.assert_restored()
        self.write_state(fail='')
        for readiness in ('not_ready', 'no_address', 'no_dhcp'):
            with self.subTest(readiness=readiness):
                self.write_state(**{readiness: True})
                result = self.run_helper('start', success=False)
                self.assertIn('hotspot failed to become ready', result.stderr)
                self.assert_restored()
                self.write_state(**{readiness: False})

        self.write_state(fail='ss -4 -H -u -l -n -p sport = :67')
        result = self.run_helper('start', success=False)
        self.assertIn('DHCP server is not listening on ap0', result.stderr)
        self.assert_restored()

    def test_unowned_ap_is_not_removed(self):
        self.run_helper('configure')
        self.write_state(ap=True)
        result = self.run_helper('start', success=False)
        self.assertIn('outside this hotspot session', result.stderr)
        self.assertTrue(self.state()['ap'])
        self.assertTrue(self.state()['wpa'])

    def test_invalid_configuration_cannot_inject_hostapd_or_networkd_settings(self):
        for values in ({'AP_SSID': 'name\ninterface=wlan0'}, {'AP_PASS': 'pass\ninterface=wlan0'},
                       {'AP_IP': '192.168.1.1\nDHCP=yes'}, {'AP_IP': '999.1.1.1'}):
            self.run_helper('configure', success=False, **values)
        self.assertFalse((self.root / 'etc/sentryusb').exists())
        self.assert_restored()


if __name__ == '__main__': unittest.main()
