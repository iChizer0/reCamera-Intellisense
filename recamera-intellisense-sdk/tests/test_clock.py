"""Tests for clock repair: set_system_time verification, probe/sync orchestration."""

from __future__ import annotations

import struct
import unittest
from unittest.mock import patch

from recamera_intellisense import clock, system
from recamera_intellisense._errors import RecameraError
from recamera_intellisense._tzmap import MULTI_ZONE, SINGLE_ZONE, timezones_for_country

DEV = {"name": "cam1", "host": "h", "token": "", "protocol": "http",
       "allow_unsecured": False, "port": None}

TIME_UTC = {
    "dNtpConfig": {"sAddress": "pool.ntp.org", "sPort": "123", "status": 0},
    "iTimestamp": 1_700_000_000,
    "sMethod": "ntp",
    "sTimezone": "UTC",
    "sTz": "UTC+0",
}

NOW = 1_800_000_000  # reference "real" time used across tests


def _resolve(module):
    return patch.object(module._config, "resolve", return_value=DEV)


class SetSystemTimeTests(unittest.TestCase):
    def test_ntp_merges_existing_server_and_verifies(self) -> None:
        reads = [dict(TIME_UTC), dict(TIME_UTC)]
        with (
            _resolve(system),
            patch.object(system._http, "get_json", side_effect=reads),
            patch.object(system._http, "put_json", return_value={"code": 0}) as put,
            patch.object(system.time, "time", return_value=NOW),
        ):
            out = system.set_system_time(method="ntp", timezone="UTC")
        payload = put.call_args[0][2]
        self.assertEqual(payload["sMethod"], "ntp")
        self.assertEqual(payload["sTimezone"], "UTC")
        # Existing NTP server/port are preserved when not overridden.
        self.assertEqual(payload["dNtpConfig"], {"sAddress": "pool.ntp.org", "sPort": "123"})
        self.assertTrue(out["verified"])

    def test_manual_sets_timestamp_and_nulls_ntp(self) -> None:
        after = dict(TIME_UTC, iTimestamp=NOW)
        with (
            _resolve(system),
            patch.object(system._http, "get_json", side_effect=[dict(TIME_UTC), after]),
            patch.object(system._http, "put_json", return_value={"code": 0}) as put,
        ):
            system.set_system_time(method="manual", timestamp=NOW)
        payload = put.call_args[0][2]
        self.assertEqual(payload["sMethod"], "manual")
        self.assertEqual(payload["iTimestamp"], NOW)
        self.assertIsNone(payload["dNtpConfig"])

    def test_silently_rejected_timezone_is_a_loud_error(self) -> None:
        # Firmware returns code:0 but keeps the old timezone — must not pass.
        with (
            _resolve(system),
            patch.object(system._http, "get_json", side_effect=[dict(TIME_UTC), dict(TIME_UTC)]),
            patch.object(system._http, "put_json", return_value={"code": 0}),
        ):
            with self.assertRaises(RecameraError) as ctx:
                system.set_system_time(method="ntp", timezone="Mars/Olympus")
        self.assertIn("verification failed", str(ctx.exception))

    def test_invalid_method_and_timezone_rejected_before_http(self) -> None:
        with _resolve(system):
            with self.assertRaises(RecameraError):
                system.set_system_time(method="chrony")
            with self.assertRaises(RecameraError):
                system.set_system_time(method="ntp", timezone="bad zone")
            with self.assertRaises(RecameraError):
                system.set_system_time(method="manual", timestamp=-1)


class TzMapTests(unittest.TestCase):
    def test_single_and_multi_zone_countries(self) -> None:
        self.assertEqual(timezones_for_country("cn"), ["Asia/Shanghai"])
        self.assertEqual(timezones_for_country("DE"), ["Europe/Berlin"])
        us = timezones_for_country("US")
        self.assertIsNotNone(us)
        self.assertGreater(len(us), 1)
        self.assertIn("America/New_York", us)

    def test_sentinels_and_unknown_codes(self) -> None:
        self.assertIsNone(timezones_for_country("00"))
        self.assertIsNone(timezones_for_country("99"))
        self.assertIsNone(timezones_for_country(""))
        self.assertIsNone(timezones_for_country("XX"))

    def test_no_country_in_both_tables(self) -> None:
        self.assertEqual(set(SINGLE_ZONE) & set(MULTI_ZONE), set())


class DhcpParsingTests(unittest.TestCase):
    def test_rfc4833_hex_options(self) -> None:
        env = {
            "opt100": "4353542d38",        # "CST-8"
            "opt101": "417369612f5368616e67686169",  # "Asia/Shanghai"
            "interface": "eth0",
        }
        out = clock._parse_dhcp_env(env)
        self.assertEqual(out["tcode"], "Asia/Shanghai")
        self.assertEqual(out["pcode"], "CST-8")
        self.assertEqual(out["utc_offset_s"], 28_800)

    def test_legacy_option2_and_raw_strings(self) -> None:
        out = clock._parse_dhcp_env({"timezone": "-18000", "opt101": "Europe/Berlin"})
        self.assertEqual(out["tcode"], "Europe/Berlin")
        self.assertEqual(out["utc_offset_s"], -18_000)

    def test_empty_env(self) -> None:
        self.assertEqual(clock._parse_dhcp_env({}), {})

    def test_posix_offset_sign_is_inverted(self) -> None:
        self.assertEqual(clock._parse_posix_tz_offset("CST-8"), 28_800)
        self.assertEqual(clock._parse_posix_tz_offset("EST5"), -18_000)
        self.assertEqual(clock._parse_posix_tz_offset("UTC0"), 0)
        self.assertIsNone(clock._parse_posix_tz_offset("garbage!!"))


class SntpTests(unittest.TestCase):
    def test_response_parsing(self) -> None:
        # Transmit timestamp (bytes 40..47): seconds since 1900 + fraction.
        secs = NOW + clock.NTP_EPOCH_OFFSET
        payload = b"\x24" + b"\x00" * 39 + struct.pack("!II", secs, 0)

        class _Sock:
            def settimeout(self, t): pass
            def sendto(self, data, addr): self.sent = (data, addr)
            def recvfrom(self, n): return payload, ("ntp", 123)
            def close(self): pass

        with patch.object(clock.socket, "socket", return_value=_Sock()), \
             patch.object(clock.socket, "getaddrinfo",
                          return_value=[(None, None, None, None, ("1.2.3.4", 123))]):
            ts, _rtt = clock._sntp_query("pool.ntp.org")
        self.assertEqual(ts, float(NOW))


class ProbeTimezoneTests(unittest.TestCase):
    def test_geoip_single_source_suggests_directly(self) -> None:
        with (
            _resolve(clock),
            patch.object(clock, "_geoip",
                         return_value={"timezone": "Asia/Shanghai", "country_code": "CN",
                                       "url": "http://ip-api.com/json/"}),
            patch.object(clock, "_is_local_device", return_value=False),
        ):
            out = clock.probe_timezone()
        self.assertEqual(out["suggested_timezone"], "Asia/Shanghai")
        # Off-device perspective always asks for confirmation before applying.
        self.assertTrue(out["needs_user_confirmation"])
        self.assertEqual(out["perspective"], "sdk-host")
        statuses = {s["source"]: s["status"] for s in out["sources"]}
        self.assertEqual(statuses["wifi_country"], "skipped")
        self.assertEqual(statuses["dhcp"], "skipped")

    def test_on_device_wifi_corroborates_geoip(self) -> None:
        def fake_geo(url, timeout=4.0):
            return {"timezone": "Asia/Shanghai", "country_code": "CN", "url": url}

        with (
            _resolve(clock),
            patch.object(clock, "_geoip", side_effect=fake_geo),
            patch.object(clock, "_wifi_countries", return_value={"CN": 104, "US": 8}),
            patch.object(clock, "_is_local_device", return_value=True),
        ):
            out = clock.probe_timezone()
        self.assertEqual(out["suggested_timezone"], "Asia/Shanghai")
        self.assertFalse(out["needs_user_confirmation"])
        cand = {c["timezone"]: c["via"] for c in out["candidates"]}
        self.assertEqual(sorted(cand["Asia/Shanghai"]), ["geoip", "wifi_country"])
        wifi = next(s for s in out["sources"] if s["source"] == "wifi_country")
        self.assertEqual(wifi["votes"], {"CN": 104, "US": 8})

    def test_conflicting_sources_require_user(self) -> None:
        with (
            _resolve(clock),
            patch.object(clock, "_geoip",
                         return_value={"timezone": "Asia/Shanghai", "country_code": "CN",
                                       "url": "x"}),
            patch.object(clock, "_wifi_countries", return_value={"DE": 9}),
            patch.object(clock, "_is_local_device", return_value=True),
        ):
            out = clock.probe_timezone()
        self.assertTrue(out["needs_user_confirmation"])
        self.assertIn("disagree", "; ".join(out["notes"]))

    def test_multi_zone_country_alone_requires_user(self) -> None:
        with (
            _resolve(clock),
            patch.object(clock, "_geoip", side_effect=RecameraError("no route")),
            patch.object(clock, "_wifi_countries", return_value={"US": 12}),
            patch.object(clock, "_is_local_device", return_value=True),
        ):
            out = clock.probe_timezone()
        self.assertIsNone(out["suggested_timezone"])
        self.assertTrue(out["needs_user_confirmation"])
        self.assertIn("America/New_York", "; ".join(out["notes"]))

    def test_dhcp_opt_in_probe_supplies_tcode(self) -> None:
        with (
            _resolve(clock),
            patch.object(clock, "_geoip", side_effect=RecameraError("down")),
            patch.object(clock, "_wifi_countries", return_value={}),
            patch.object(clock, "_is_local_device", return_value=True),
            patch.object(clock, "_dhcp_iface_guess", return_value="eth0"),
            patch.object(clock, "_dhcp_tz_probe",
                         return_value={"tcode": "Asia/Shanghai", "pcode": "CST-8",
                                       "utc_offset_s": 28_800}),
        ):
            out = clock.probe_timezone(dhcp_probe=True)
        self.assertEqual(out["suggested_timezone"], "Asia/Shanghai")
        dhcp = next(s for s in out["sources"] if s["source"] == "dhcp")
        self.assertEqual(dhcp["status"], "ok")
        self.assertEqual(dhcp["pcode"], "CST-8")

    def test_dhcp_skipped_on_static_interface(self) -> None:
        with (
            _resolve(clock),
            patch.object(clock, "_geoip", side_effect=RecameraError("down")),
            patch.object(clock, "_wifi_countries", return_value={}),
            patch.object(clock, "_is_local_device", return_value=True),
            patch.object(clock, "_dhcp_iface_guess", return_value=None),
        ):
            out = clock.probe_timezone(dhcp_probe=True)
        dhcp = next(s for s in out["sources"] if s["source"] == "dhcp")
        self.assertEqual(dhcp["status"], "skipped")
        self.assertIn("static", dhcp["reason"])

    def test_geoip_oserror_is_contained(self) -> None:
        # Connection-level failures (refused/reset) must not crash the probe.
        with (
            _resolve(clock),
            patch.object(clock, "_geoip", side_effect=OSError("connection refused")),
            patch.object(clock, "_wifi_countries", return_value={}),
            patch.object(clock, "_is_local_device", return_value=True),
        ):
            out = clock.probe_timezone()
        geo = next(s for s in out["sources"] if s["source"] == "geoip")
        self.assertEqual(geo["status"], "unavailable")

    def test_geoip_wifi_country_mismatch_requires_user(self) -> None:
        # VPN egress abroad vs local RF beacons — conflicting countries.
        with (
            _resolve(clock),
            patch.object(clock, "_geoip",
                         return_value={"timezone": "America/New_York",
                                       "country_code": "US", "url": "x"}),
            patch.object(clock, "_wifi_countries", return_value={"CN": 50}),
            patch.object(clock, "_is_local_device", return_value=True),
        ):
            out = clock.probe_timezone()
        self.assertTrue(out["needs_user_confirmation"])
        wifi = next(s for s in out["sources"] if s["source"] == "wifi_country")
        self.assertTrue(wifi.get("conflicts_with_geoip"))


def _time_state(ts=1_700_000_000, tz="UTC", method="ntp"):
    return {"method": method, "timestamp": ts, "timezone": tz,
            "tz": "UTC+0", "ntp": {"address": "pool.ntp.org", "port": "123"}}


class SyncSystemTimeTests(unittest.TestCase):
    def test_already_in_sync_changes_nothing(self) -> None:
        with (
            _resolve(clock),
            patch.object(clock, "get_system_time", return_value=_time_state(ts=NOW)),
            patch.object(clock, "_reference_time", return_value=(float(NOW), {"source": "sntp"})),
            patch.object(clock, "_is_local_device", return_value=True),
            patch.object(clock, "probe_timezone",
                         return_value={"suggested_timezone": "UTC",
                                       "needs_user_confirmation": False,
                                       "candidates": [], "perspective": "device"}),
        ):
            out = clock.sync_system_time()
        self.assertFalse(out["changed"])
        self.assertTrue(out["verified"])

    def test_ntp_path_applies_and_polls_until_synced(self) -> None:
        gets = iter([
            _time_state(ts=NOW - 86_400),          # before: a day off
            _time_state(ts=NOW - 86_400),          # first poll: not yet
            _time_state(ts=NOW, tz="Asia/Shanghai"),  # second poll: synced + tz applied
        ])
        with (
            _resolve(clock),
            patch.object(clock, "get_system_time", side_effect=lambda *a, **k: next(gets)),
            patch.object(clock, "_reference_time",
                         return_value=(float(NOW), {"source": "sntp", "detail": "pool.ntp.org"})),
            patch.object(clock, "_is_local_device", return_value=True),
            patch.object(clock, "probe_timezone",
                         return_value={"suggested_timezone": "Asia/Shanghai",
                                       "needs_user_confirmation": False,
                                       "candidates": [{"timezone": "Asia/Shanghai",
                                                       "via": ["geoip"]}],
                                       "perspective": "device"}),
            patch.object(clock, "set_system_time") as setter,
            patch.object(clock.time, "sleep"),
        ):
            out = clock.sync_system_time(timezone="auto")
        self.assertTrue(out["changed"])
        self.assertTrue(out["verified"])
        self.assertEqual(out["method_used"], "ntp")
        self.assertEqual(out["timezone"]["applied"], "Asia/Shanghai")
        kwargs = setter.call_args[1]
        self.assertEqual(kwargs["method"], "ntp")
        self.assertEqual(kwargs["timezone"], "Asia/Shanghai")

    def test_manual_fallback_when_ntp_unreachable(self) -> None:
        gets = iter([_time_state(ts=NOW - 300), _time_state(ts=NOW)])
        with (
            _resolve(clock),
            patch.object(clock, "get_system_time", side_effect=lambda *a, **k: next(gets)),
            patch.object(clock, "_reference_time",
                         return_value=(float(NOW), {"source": "http_date"})),
            patch.object(clock, "_is_local_device", return_value=True),
            patch.object(clock, "probe_timezone",
                         return_value={"suggested_timezone": None,
                                       "needs_user_confirmation": False,
                                       "candidates": [], "perspective": "device"}),
            patch.object(clock, "set_system_time") as setter,
        ):
            out = clock.sync_system_time(timezone=None)  # method defaults to auto
        self.assertEqual(out["method_used"], "manual")
        # First call sets the clock manually; second restores NTP mode.
        first = setter.call_args_list[0][1]
        self.assertEqual(first["method"], "manual")
        self.assertEqual(first["timestamp"], NOW)
        self.assertEqual(setter.call_args_list[1][1]["method"], "ntp")
        self.assertEqual(out["method_restored"], "ntp")
        self.assertTrue(any("drift" in w for w in out["warnings"]))

    def test_explicit_manual_is_not_reverted_to_ntp(self) -> None:
        # The user pinned manual mode: an explicit method=manual must not
        # silently flip the device back to ntp afterwards.
        gets = iter([_time_state(ts=NOW - 300), _time_state(ts=NOW)])
        with (
            _resolve(clock),
            patch.object(clock, "get_system_time", side_effect=lambda *a, **k: next(gets)),
            patch.object(clock, "_reference_time",
                         return_value=(float(NOW), {"source": "http_date"})),
            patch.object(clock, "_is_local_device", return_value=True),
            patch.object(clock, "set_system_time") as setter,
        ):
            out = clock.sync_system_time(timezone=None, method="manual")
        self.assertEqual(out["method_used"], "manual")
        self.assertEqual(len(setter.call_args_list), 1)
        self.assertNotIn("method_restored", out)

    def test_probe_failure_never_blocks_clock_fix(self) -> None:
        gets = iter([_time_state(ts=NOW - 300), _time_state(ts=NOW)])
        with (
            _resolve(clock),
            patch.object(clock, "get_system_time", side_effect=lambda *a, **k: next(gets)),
            patch.object(clock, "_reference_time",
                         return_value=(float(NOW), {"source": "sntp"})),
            patch.object(clock, "_is_local_device", return_value=True),
            patch.object(clock, "probe_timezone", side_effect=RuntimeError("boom")),
            patch.object(clock, "set_system_time"),
        ):
            out = clock.sync_system_time(timezone="auto")
        self.assertTrue(out["verified"])
        self.assertTrue(any("timezone probe failed" in w for w in out["warnings"]))

    def test_evidence_matching_current_timezone_asks_nothing(self) -> None:
        # Remote SDK host, and the probe agrees with the camera's timezone:
        # a no-op that must not nag the user.
        with (
            _resolve(clock),
            patch.object(clock, "get_system_time",
                         return_value=_time_state(ts=NOW, tz="Asia/Shanghai")),
            patch.object(clock, "_reference_time",
                         return_value=(float(NOW), {"source": "sntp"})),
            patch.object(clock, "_is_local_device", return_value=False),
            patch.object(clock, "probe_timezone",
                         return_value={"suggested_timezone": "Asia/Shanghai",
                                       "needs_user_confirmation": True,
                                       "candidates": [{"timezone": "Asia/Shanghai",
                                                       "via": ["geoip"]}],
                                       "perspective": "sdk-host"}),
        ):
            out = clock.sync_system_time(timezone="auto")
        self.assertFalse(out["changed"])
        self.assertNotIn("needs_user_confirmation", out["timezone"])
        self.assertEqual(out["follow_up"], [])

    def test_timezone_only_change_keeps_sync_mode_and_clock(self) -> None:
        # Clock accurate, device in ntp mode, only the timezone is wrong:
        # apply via ntp PUT (no manual churn), clock untouched.
        gets = iter([
            _time_state(ts=NOW - 30, tz="UTC"),
            _time_state(ts=NOW - 30, tz="Asia/Shanghai"),
        ])
        with (
            _resolve(clock),
            patch.object(clock, "get_system_time", side_effect=lambda *a, **k: next(gets)),
            patch.object(clock, "_reference_time",
                         return_value=(float(NOW), {"source": "http_date"})),
            patch.object(clock, "_is_local_device", return_value=True),
            patch.object(clock, "set_system_time") as setter,
        ):
            out = clock.sync_system_time(timezone="Asia/Shanghai")
        self.assertTrue(out["changed"])
        self.assertTrue(out["verified"])
        kwargs = setter.call_args[1]
        self.assertEqual(kwargs["method"], "ntp")          # no manual churn
        self.assertIsNone(kwargs["timestamp"])              # clock untouched
        self.assertEqual(kwargs["timezone"], "Asia/Shanghai")
        self.assertNotIn("method_restored", out)

    def test_no_reference_on_device_asks_user_and_changes_nothing(self) -> None:
        with (
            _resolve(clock),
            patch.object(clock, "get_system_time", return_value=_time_state()),
            patch.object(clock, "_reference_time", return_value=(None, {"source": "none"})),
            patch.object(clock, "_is_local_device", return_value=True),
            patch.object(clock, "set_system_time") as setter,
        ):
            out = clock.sync_system_time()
        self.assertTrue(out["needs_user_help"])
        self.assertIn("ask_user", out)
        setter.assert_not_called()

    def test_ambiguous_timezone_still_fixes_clock_but_asks(self) -> None:
        gets = iter([_time_state(ts=NOW - 500), _time_state(ts=NOW)])
        with (
            _resolve(clock),
            patch.object(clock, "get_system_time", side_effect=lambda *a, **k: next(gets)),
            patch.object(clock, "_reference_time",
                         return_value=(float(NOW), {"source": "sntp"})),
            patch.object(clock, "_is_local_device", return_value=True),
            patch.object(clock, "probe_timezone",
                         return_value={"suggested_timezone": "America/New_York",
                                       "needs_user_confirmation": True,
                                       "candidates": [{"timezone": "America/New_York",
                                                       "via": ["geoip"]}],
                                       "perspective": "device"}),
            patch.object(clock, "set_system_time") as setter,
            patch.object(clock.time, "sleep"),
        ):
            out = clock.sync_system_time(timezone="auto")
        # Clock fixed, timezone deliberately untouched, user asked.
        self.assertIsNone(setter.call_args[1]["timezone"])
        self.assertTrue(out["timezone"]["needs_user_confirmation"])
        self.assertTrue(out["verified"])

    def test_dry_run_reports_plan_without_writes(self) -> None:
        with (
            _resolve(clock),
            patch.object(clock, "get_system_time", return_value=_time_state(ts=NOW - 5000)),
            patch.object(clock, "_reference_time",
                         return_value=(float(NOW), {"source": "sntp"})),
            patch.object(clock, "_is_local_device", return_value=True),
            patch.object(clock, "probe_timezone",
                         return_value={"suggested_timezone": "Asia/Shanghai",
                                       "needs_user_confirmation": False,
                                       "candidates": [], "perspective": "device"}),
            patch.object(clock, "set_system_time") as setter,
        ):
            out = clock.sync_system_time(dry_run=True)
        setter.assert_not_called()
        self.assertEqual(out["dry_run"]["would_set_method"], "ntp")
        self.assertEqual(out["dry_run"]["would_set_timezone"], "Asia/Shanghai")
        self.assertEqual(out["dry_run"]["would_set_timestamp"], NOW)


if __name__ == "__main__":  # pragma: no cover
    unittest.main()
