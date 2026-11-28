#!/usr/bin/env python3
"""Exercise scripts/google-play.py against a local stand-in for the Google Play
Developer API that records every call."""

import contextlib
import hashlib
import http.server
import importlib.util
import io
import json
from pathlib import Path
import sys
import tempfile
import threading
import unittest
import unittest.mock
import urllib.parse

sys.dont_write_bytecode = True
SCRIPTS = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("google_play", SCRIPTS / "google-play.py")
google_play = importlib.util.module_from_spec(spec)
spec.loader.exec_module(google_play)

APP = "/androidpublisher/v3/applications/com.flectar.mail"
UPLOAD = "/upload" + APP


class FakePlay(http.server.BaseHTTPRequestHandler):
    """Google Play's edits API, reduced to what the script uses."""

    def log_message(self, *args):
        pass

    def respond(self, status, body):
        data = json.dumps(body).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def handle_call(self):
        state = self.server.state
        url = urllib.parse.urlsplit(self.path)
        query = dict(urllib.parse.parse_qsl(url.query))
        body = self.rfile.read(int(self.headers.get("Content-Length") or 0))
        state["calls"].append((self.command, url.path, query))
        if self.headers.get("Authorization") != "Bearer test-token":
            return self.respond(401, {"error": {"message": "no token"}})
        failure = state["failures"].get((self.command, url.path))
        if failure:
            return self.respond(failure, {"error": {"message": "The caller does not have permission"}})
        upload = url.path.startswith(UPLOAD)
        path = url.path.removeprefix(UPLOAD if upload else APP).split("/")[1:]
        if path == ["edits"] and self.command == "POST":
            return self.respond(200, {"id": "edit-1"})
        if path[:1] != ["edits"]:
            return self.respond(404, {"error": {"message": "unknown"}})
        rest = path[2:]
        if path[1] in ("edit-1:validate", "edit-1:commit") and self.command == "POST":
            return self.respond(200, {"id": "edit-1"})
        if not rest and self.command == "DELETE":
            return self.respond(200, {})
        if rest == ["listings"] and self.command == "GET":
            return self.respond(200, {"listings": list(state["listings"].values())})
        if len(rest) == 2 and rest[0] == "listings" and self.command in ("PUT", "PATCH"):
            listing = json.loads(body)
            if self.command == "PUT":
                state["listings"][rest[1]] = listing
            else:
                state["listings"].setdefault(rest[1], {"language": rest[1]}).update(listing)
            return self.respond(200, state["listings"][rest[1]])
        if len(rest) == 3 and rest[0] == "listings":
            key = (rest[1], rest[2])
            if self.command == "GET":
                return self.respond(200, {"images": state["images"].get(key, [])})
            if self.command == "DELETE":
                return self.respond(200, {"deleted": state["images"].pop(key, [])})
            if self.command == "POST" and upload:
                image = {"id": str(len(state["uploads"])), "sha256": hashlib.sha256(body).hexdigest()}
                state["images"].setdefault(key, []).append(image)
                state["uploads"].append((key, body))
                return self.respond(200, {"image": image})
        if rest == ["bundles"] and self.command == "POST" and upload:
            state["uploads"].append(("bundle", body))
            return self.respond(200, {"versionCode": 1001001, "sha256": hashlib.sha256(body).hexdigest()})
        if len(rest) == 2 and rest[0] == "tracks" and self.command == "PUT":
            state["tracks"][rest[1]] = json.loads(body)
            return self.respond(200, state["tracks"][rest[1]])
        return self.respond(404, {"error": {"message": f"unexpected {self.command} {url.path}"}})

    do_GET = do_POST = do_PUT = do_PATCH = do_DELETE = handle_call


class GooglePlayTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), FakePlay)
        threading.Thread(target=cls.server.serve_forever, daemon=True).start()
        cls.root = f"http://127.0.0.1:{cls.server.server_address[1]}"

    @classmethod
    def tearDownClass(cls):
        cls.server.shutdown()
        cls.server.server_close()

    def setUp(self):
        self.server.state = {"calls": [], "failures": {}, "listings": {}, "images": {}, "uploads": [],
                             "tracks": {}}
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.metadata = Path(directory.name) / "android"
        locale = self.metadata / "en-US"
        (locale / "images/phoneScreenshots").mkdir(parents=True)
        (locale / "title.txt").write_text("Flectar Mail\n")
        (locale / "short_description.txt").write_text("Email, calendars and contacts\n")
        (locale / "full_description.txt").write_text("A fast, native home for your email.\n")
        (locale / "images/icon.png").write_bytes(b"icon")
        (locale / "images/featureGraphic.png").write_bytes(b"graphic")
        for name in ("2-message.png", "1-inbox.png"):
            (locale / "images/phoneScreenshots" / name).write_bytes(name.encode())
        self.bundle = Path(directory.name) / "flectar-mail-0.1.0-beta.1.aab"
        self.bundle.write_bytes(b"bundle")
        self.environment = {"GOOGLE_PLAY_ACCESS_TOKEN": "test-token"}

    def run_script(self, *args):
        output, errors = io.StringIO(), io.StringIO()
        with unittest.mock.patch.dict("os.environ", self.environment), \
                contextlib.redirect_stdout(output), contextlib.redirect_stderr(errors):
            status = google_play.main(["--metadata", str(self.metadata), *args], root=self.root)
        return status, output.getvalue() + errors.getvalue()

    def calls(self):
        return [(method, path.split("/edits", 1)[-1], query) for method, path, query in self.server.state["calls"]]

    def match_play_to_repository(self):
        state = self.server.state
        state["listings"]["en-US"] = {"language": "en-US", "title": "Flectar Mail",
                                      "shortDescription": "Email, calendars and contacts",
                                      "fullDescription": "A fast, native home for your email.",
                                      "video": "https://www.youtube.com/watch?v=example"}
        for image_type, data in (("icon", [b"icon"]), ("featureGraphic", [b"graphic"]),
                                 ("phoneScreenshots", [b"1-inbox.png", b"2-message.png"])):
            state["images"][("en-US", image_type)] = [
                {"id": str(index), "sha256": hashlib.sha256(item).hexdigest()} for index, item in enumerate(data)]

    def test_new_listing_is_uploaded_validated_and_committed(self):
        status, output = self.run_script("listing")
        self.assertEqual(status, 0, output)
        state = self.server.state
        self.assertEqual(state["listings"]["en-US"]["fullDescription"], "A fast, native home for your email.")
        self.assertEqual([body for key, body in state["uploads"] if key == ("en-US", "phoneScreenshots")],
                         [b"1-inbox.png", b"2-message.png"], "screenshots keep their file name order")
        calls = self.calls()
        self.assertIn(("PUT", "/edit-1/listings/en-US", {}), calls)
        self.assertIn(("POST", "/edit-1:validate", {}), calls)
        self.assertEqual(calls[-1], ("POST", "/edit-1:commit", {"changesInReviewBehavior": "ERROR_IF_IN_REVIEW"}))
        self.assertTrue(all(query.get("uploadType") == "media" for method, path, query in state["calls"]
                            if path.startswith(UPLOAD)))

    def test_matching_listing_changes_nothing(self):
        self.match_play_to_repository()
        status, output = self.run_script("listing")
        self.assertEqual(status, 0, output)
        self.assertIn("already matches", output)
        calls = self.calls()
        self.assertEqual([method for method, path, query in calls if method in ("PUT", "PATCH")], [])
        self.assertEqual(self.server.state["uploads"], [])
        self.assertNotIn(("POST", "/edit-1:commit", {"changesInReviewBehavior": "ERROR_IF_IN_REVIEW"}), calls)
        self.assertEqual(calls[-1], ("DELETE", "/edit-1", {}))

    def test_changes_touch_only_what_differs(self):
        self.match_play_to_repository()
        (self.metadata / "en-US/short_description.txt").write_text("Mail, calendars and contacts\n")
        (self.metadata / "en-US/images/featureGraphic.png").write_bytes(b"new graphic")
        status, output = self.run_script("listing")
        self.assertEqual(status, 0, output)
        listing = self.server.state["listings"]["en-US"]
        self.assertEqual(listing["shortDescription"], "Mail, calendars and contacts")
        self.assertEqual(listing["video"], "https://www.youtube.com/watch?v=example", "Play Console's video stays")
        self.assertEqual([key for key, body in self.server.state["uploads"]], [("en-US", "featureGraphic")])
        self.assertIn(("PATCH", "/edit-1/listings/en-US", {}), self.calls())

    def test_validate_only_discards_the_edit(self):
        status, output = self.run_script("listing", "--validate-only")
        self.assertEqual(status, 0, output)
        calls = self.calls()
        self.assertIn(("POST", "/edit-1:validate", {}), calls)
        self.assertFalse(any(path.endswith(":commit") for method, path, query in calls))
        self.assertEqual(calls[-1], ("DELETE", "/edit-1", {}))

    def test_release_goes_to_the_track_with_its_notes(self):
        changelogs = self.metadata / "en-US/changelogs"
        changelogs.mkdir()
        (changelogs / "1001001.txt").write_text("First beta.\n")
        status, output = self.run_script("release", str(self.bundle), "--track", "open", "--name", "0.1.0-beta.1")
        self.assertEqual(status, 0, output)
        self.assertEqual(self.server.state["uploads"], [("bundle", b"bundle")])
        self.assertEqual(self.server.state["tracks"]["beta"], {"track": "beta", "releases": [{
            "versionCodes": ["1001001"], "status": "completed", "name": "0.1.0-beta.1",
            "releaseNotes": [{"language": "en-US", "text": "First beta."}]}]})
        self.assertEqual(self.calls()[-1], ("POST", "/edit-1:commit", {"changesInReviewBehavior": "ERROR_IF_IN_REVIEW"}))

    def test_draft_release_without_notes(self):
        status, output = self.run_script("release", str(self.bundle), "--track", "internal", "--draft")
        self.assertEqual(status, 0, output)
        self.assertIn("no notes", output)
        self.assertEqual(self.server.state["tracks"]["internal"]["releases"],
                         [{"versionCodes": ["1001001"], "status": "draft"}])

    def test_api_errors_discard_the_edit_and_fail(self):
        self.server.state["failures"][("GET", APP + "/edits/edit-1/listings")] = 403
        status, output = self.run_script("listing")
        self.assertEqual(status, 1)
        self.assertIn("HTTP 403: The caller does not have permission", output)
        self.assertEqual(self.calls()[-1], ("DELETE", "/edit-1", {}))

    def test_token_and_bundle_are_required(self):
        self.environment = {"GOOGLE_PLAY_ACCESS_TOKEN": ""}
        status, output = self.run_script("listing")
        self.assertEqual(status, 2)
        self.assertIn("GOOGLE_PLAY_ACCESS_TOKEN", output)
        self.environment = {"GOOGLE_PLAY_ACCESS_TOKEN": "test-token"}
        status, output = self.run_script("release", str(self.bundle.with_name("missing.aab")), "--track", "internal")
        self.assertEqual(status, 2)
        self.assertIn("does not exist", output)
        self.assertEqual(self.server.state["calls"], [])


if __name__ == "__main__":
    unittest.main()
