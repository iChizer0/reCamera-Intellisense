"""Normalized Vigil recording-event compatibility and metadata tests."""

from __future__ import annotations

import unittest
from unittest.mock import patch

from recamera_intellisense import detection, files
from recamera_intellisense._errors import RecameraError


UINT64_MAX = 2**64 - 1


class DetectionEventProjectionTests(unittest.TestCase):
    def test_multiple_files_full_width_ids_and_independent_timezones(self) -> None:
        raw = {
            "event": "RULE",
            "type": "INFERENCE_SET",
            "id": "front-door",
            "uid": UINT64_MAX,
            "timestamp": 1_700_000_000_000,
            "file_ids": [UINT64_MAX - 1, UINT64_MAX],
            "tz": "Asia/Shanghai",
            "tz_offset": 28_800,
            "dst": False,
            "file_events": [
                {
                    "op": "ADDED",
                    "path": "/mnt/sdcard/photo.jpg",
                    "size": UINT64_MAX,
                    "attributes": "COMPLETED",
                    "event_uid": UINT64_MAX,
                    "file_id": UINT64_MAX - 1,
                    "timestamp": 1_700_000_000_100,
                    "tz": "America/New_York",
                    "tz_offset": -18_000,
                    "dst": False,
                },
                {
                    "op": "ADDED",
                    "path": "/mnt/sdcard/video.mp4",
                    "size": 200,
                    "attributes": "COMPLETED",
                    "event_uid": 0,
                    "file_id": UINT64_MAX,
                    "timestamp": UINT64_MAX,
                    "tz_offset": -14_400,
                    "dst": None,
                },
            ],
        }
        with patch.object(detection._files, "get_intellisense_events", return_value=[raw]):
            events = detection.get_detection_events("cam")

        self.assertEqual(len(events), 1)
        event = events[0]
        self.assertEqual(event["timestamp"], "2023-11-14T22:13:20Z")
        self.assertEqual(event["timestamp_unix_ms"], 1_700_000_000_000)
        self.assertEqual(event["uid"], UINT64_MAX)
        self.assertEqual(event["uid_str"], str(UINT64_MAX))
        self.assertEqual(event["file_ids"], [UINT64_MAX - 1, UINT64_MAX])
        self.assertEqual(event["file_id_strs"], [str(UINT64_MAX - 1), str(UINT64_MAX)])
        self.assertEqual(event["tz"], "Asia/Shanghai")
        self.assertEqual(event["tz_offset"], 28_800)
        self.assertIs(event["dst"], False)
        self.assertEqual(event["snapshot_path"], "/mnt/sdcard/photo.jpg")
        self.assertEqual(len(event["file_events"]), 2)
        photo, video = event["file_events"]
        self.assertEqual(photo["file_id"], UINT64_MAX - 1)
        self.assertEqual(photo["file_id_str"], str(UINT64_MAX - 1))
        self.assertEqual(photo["event_uid"], UINT64_MAX)
        self.assertEqual(photo["event_uid_str"], str(UINT64_MAX))
        self.assertEqual(photo["size"], UINT64_MAX)
        self.assertEqual(photo["timestamp"], "2023-11-14T22:13:20.100000Z")
        self.assertEqual(photo["tz_offset"], -18_000)
        self.assertIs(photo["dst"], False)
        self.assertEqual(video["file_id"], UINT64_MAX)
        self.assertEqual(video["event_uid"], 0)
        self.assertEqual(video["timestamp_unix_ms"], UINT64_MAX)
        self.assertEqual(video["timestamp"], "+584556019-04-03T14:25:51.615000Z")
        self.assertNotIn("tz", video)
        self.assertEqual(video["tz_offset"], -14_400)
        self.assertIsNone(video["dst"])

    def test_legacy_file_event_projection_remains_compatible(self) -> None:
        raw = {
            "type": "INFERENCE",
            "id": "legacy",
            "uid": 7,
            "timestamp": 1000,
            "file_event": {
                "op": "ADDED",
                "path": "/mnt/sdcard/legacy.jpg",
                "size": 10,
                "event_uid": 7,
                "timestamp": 2000,
            },
        }
        with patch.object(detection._files, "get_intellisense_events", return_value=[raw]):
            event = detection.get_detection_events()[0]

        self.assertEqual(event["snapshot_path"], "/mnt/sdcard/legacy.jpg")
        self.assertEqual(event["file_events"][0]["path"], "/mnt/sdcard/legacy.jpg")
        self.assertNotIn("file_id", event["file_events"][0])
        self.assertNotIn("file_ids", event)
        self.assertNotIn("tz", event)
        self.assertNotIn("tz_offset", event)
        self.assertNotIn("dst", event)

    def test_rule_without_files_has_empty_list_and_no_snapshot(self) -> None:
        raw = {"type": "HTTP", "id": "http", "uid": 1, "timestamp": 1000, "file_events": []}
        with patch.object(detection._files, "get_intellisense_events", return_value=[raw]):
            event = detection.get_detection_events()[0]
        self.assertEqual(event["file_events"], [])
        self.assertNotIn("snapshot_path", event)

    def test_invalid_numbers_and_file_ids_are_not_silently_changed(self) -> None:
        self.assertEqual(detection._as_u64(0), 0)
        self.assertEqual(detection._as_u64(UINT64_MAX), UINT64_MAX)
        for value in (-1, UINT64_MAX + 1, True, 1.5):
            self.assertIsNone(detection._as_u64(value))
        self.assertEqual(detection._timestamp_ms(1000.0), 1000)
        self.assertEqual(detection._timestamp_ms(UINT64_MAX), UINT64_MAX)
        for value in (1.5, float("nan"), float("inf"), UINT64_MAX + 1, True):
            self.assertIsNone(detection._timestamp_ms(value))

        raw = {
            "type": "INFERENCE",
            "id": "invalid",
            "uid": UINT64_MAX + 1,
            "timestamp": 1000,
            "file_ids": [1, None, 3],
            "file_event": {"path": "/mnt/sdcard/incomplete.jpg"},
        }
        with (
            patch.object(detection._files, "get_intellisense_events", return_value=[raw]),
            self.assertRaisesRegex(RecameraError, "uid"),
        ):
            detection.get_detection_events()[0]

    def test_malformed_file_cannot_disappear_from_a_valid_rule(self) -> None:
        raw = {
            "type": "INFERENCE_SET", "id": "mixed", "uid": 1, "timestamp": 1000,
            "file_events": [
                {"op": "ADDED", "path": "/mnt/valid.jpg", "size": 10,
                 "event_uid": 1, "file_id": 2, "timestamp": 2000},
                {"op": "ADDED", "path": "/mnt/broken.mp4", "size": 20,
                 "event_uid": 1, "file_id": 3, "timestamp": "invalid"},
            ],
        }
        with (
            patch.object(detection._files, "get_intellisense_events", return_value=[raw]),
            self.assertRaisesRegex(RecameraError, "file event at index 1.*timestamp"),
        ):
            detection.get_detection_events()

    def test_unexpected_response_shape_is_an_error_not_empty_history(self) -> None:
        with (
            patch.object(files._config, "resolve", return_value={}),
            patch.object(files._http, "get_json", return_value={"unexpected": []}),
            self.assertRaisesRegex(RecameraError, "expected an array"),
        ):
            files.get_intellisense_events()


if __name__ == "__main__":
    unittest.main()
