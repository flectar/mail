#!/usr/bin/env python3
"""Publish to Google Play through the Google Play Developer API.

`listing` makes the store listing match fastlane/metadata/android: the text
and the images of each language there. `release` uploads an App Bundle and
releases it on a testing track, with the release notes from each language's
changelogs/<version code>.txt. Production stays a step in Play Console.

Each run works in one Play edit and changes only what differs from Play. It
validates the edit, then commits it, or with --validate-only discards it. If
other changes are in review, the commit fails instead of cancelling them.

The access token comes from GOOGLE_PLAY_ACCESS_TOKEN. The Google Play
workflows get a short-lived one for the androidpublisher scope through
Workload Identity Federation, so no key is stored (docs/android-signing.md).
"""

import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
import sys
import urllib.error
import urllib.parse
import urllib.request

PROJECT = Path(__file__).resolve().parents[1]
METADATA = PROJECT / "fastlane/metadata/android"
PACKAGE = "com.flectar.mail"
API_ROOT = "https://androidpublisher.googleapis.com"
TEXTS = {"title": "title.txt", "shortDescription": "short_description.txt",
         "fullDescription": "full_description.txt"}
IMAGES = {"icon": "images/icon.png", "featureGraphic": "images/featureGraphic.png",
          "phoneScreenshots": "images/phoneScreenshots"}
# Play's names for the testing tracks.
TRACKS = {"internal": "internal", "closed": "alpha", "open": "beta"}


class PlayError(Exception):
    pass


class Play:
    def __init__(self, token, root=API_ROOT, package=PACKAGE):
        self.token = token
        self.root = root.rstrip("/")
        self.package = package

    def call(self, method, path, body=None, upload=None, query=None):
        """Call the API under this app. UPLOAD is (bytes, media type)."""
        url = (f"{self.root}/{'upload/' if upload else ''}androidpublisher/v3/applications/"
               f"{self.package}{path}")
        if upload:
            query = {"uploadType": "media", **(query or {})}
        if query:
            url += "?" + urllib.parse.urlencode(query)
        headers = {"Authorization": f"Bearer {self.token}"}
        data = None
        if upload:
            data, headers["Content-Type"] = upload
        elif body is not None:
            data, headers["Content-Type"] = json.dumps(body).encode(), "application/json"
        request = urllib.request.Request(url, data=data, method=method, headers=headers)
        try:
            with urllib.request.urlopen(request, timeout=600) as response:
                raw = response.read()
        except urllib.error.HTTPError as error:
            detail = error.read().decode("utf-8", "replace")
            try:
                detail = json.loads(detail)["error"]["message"]
            except (ValueError, KeyError, TypeError):
                pass
            raise PlayError(f"{method} {path}: HTTP {error.code}: {detail}") from None
        except urllib.error.URLError as error:
            raise PlayError(f"{method} {path}: {error.reason}") from None
        return json.loads(raw) if raw.strip() else {}


def same_image(image, data):
    """Play reports each image's SHA-256; accept it in hex or in base64."""
    digest = hashlib.sha256(data).digest()
    encodings = {digest.hex(), base64.b64encode(digest).decode(),
                 base64.urlsafe_b64encode(digest).decode()}
    encodings |= {value.rstrip("=") for value in encodings}
    return image.get("sha256", "") in encodings or image.get("sha256", "").lower() in encodings


def text(path):
    return path.read_text(encoding="utf-8").removesuffix("\n")


def sync_listing(play, edit, metadata):
    """Change the edit's listings to match METADATA; return whether anything changed."""
    changed = False
    current = {item["language"]: item
               for item in play.call("GET", f"/edits/{edit}/listings").get("listings", [])}
    for locale in sorted(path for path in metadata.iterdir() if path.is_dir()):
        language = locale.name
        texts = {field: text(locale / name) for field, name in TEXTS.items() if (locale / name).is_file()}
        have = current.get(language)
        differs = {field: value for field, value in texts.items() if (have or {}).get(field, "") != value}
        if differs:
            # A patch leaves what the repository doesn't hold, like a video,
            # as Play Console has it.
            method = "PATCH" if have else "PUT"
            play.call(method, f"/edits/{edit}/listings/{language}", body={"language": language, **differs})
            print(f"{language}: updating {', '.join(sorted(differs))}")
            changed = True
        for image_type, name in IMAGES.items():
            path = locale / name
            files = sorted(path.glob("*.png")) if path.is_dir() else [path] if path.is_file() else []
            if not files:
                continue
            uploads = [file.read_bytes() for file in files]
            images = play.call("GET", f"/edits/{edit}/listings/{language}/{image_type}").get("images", [])
            if len(images) == len(uploads) and all(map(same_image, images, uploads)):
                continue
            play.call("DELETE", f"/edits/{edit}/listings/{language}/{image_type}")
            for data in uploads:
                play.call("POST", f"/edits/{edit}/listings/{language}/{image_type}",
                          upload=(data, "image/png"))
            print(f"{language}: replacing {image_type} with {len(uploads)} image(s)")
            changed = True
    return changed


def release(play, edit, bundle, track, name, draft, metadata):
    """Upload BUNDLE and make it the TRACK's release; always a change."""
    uploaded = play.call("POST", f"/edits/{edit}/bundles",
                         upload=(bundle.read_bytes(), "application/octet-stream"))
    version_code = uploaded.get("versionCode")
    if not version_code:
        raise PlayError(f"Google Play returned no version code for {bundle.name}")
    print(f"Uploaded {bundle.name} as version code {version_code}")
    notes = [{"language": locale.name, "text": text(locale / "changelogs" / f"{version_code}.txt")}
             for locale in sorted(path for path in metadata.iterdir() if path.is_dir())
             if (locale / "changelogs" / f"{version_code}.txt").is_file()]
    entry = {"versionCodes": [str(version_code)], "status": "draft" if draft else "completed"}
    if name:
        entry["name"] = name
    if notes:
        entry["releaseNotes"] = notes
    else:
        print(f"No changelogs/{version_code}.txt in any language, so the release has no notes")
    play.call("PUT", f"/edits/{edit}/tracks/{track}", body={"track": track, "releases": [entry]})
    label = next((choice for choice, name in TRACKS.items() if name == track), track)
    print(f"Releasing version code {version_code} on the {label} testing track"
          + (" as a draft" if draft else ""))
    return True


def publish(play, change, validate_only):
    """Run CHANGE(play, edit) in a new edit, then validate and commit it."""
    edit = play.call("POST", "/edits", body={}).get("id")
    if not edit:
        raise PlayError("Google Play returned no edit")
    try:
        if not change(play, edit):
            print("Google Play already matches the repository")
            play.call("DELETE", f"/edits/{edit}")
            return
        play.call("POST", f"/edits/{edit}:validate")
        if validate_only:
            print("Google Play accepts these changes; discarding them, as --validate-only asks")
            play.call("DELETE", f"/edits/{edit}")
            return
        play.call("POST", f"/edits/{edit}:commit",
                  query={"changesInReviewBehavior": "ERROR_IF_IN_REVIEW"})
        print("Committed the changes to Google Play")
    except BaseException:
        try:
            play.call("DELETE", f"/edits/{edit}")
        except PlayError:
            pass
        raise


def main(argv=None, root=API_ROOT):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--metadata", type=Path, default=METADATA, help=argparse.SUPPRESS)
    commands = parser.add_subparsers(dest="command", required=True)
    listing = commands.add_parser("listing", help="make the store listing match the repository")
    listing.add_argument("--validate-only", action="store_true", help="check the changes, then discard them")
    bundle = commands.add_parser("release", help="release an App Bundle on a testing track")
    bundle.add_argument("bundle", type=Path)
    bundle.add_argument("--track", choices=sorted(TRACKS), required=True)
    bundle.add_argument("--name", help="the release's name in Play Console, such as its version")
    bundle.add_argument("--draft", action="store_true", help="leave the release for Play Console to roll out")
    bundle.add_argument("--validate-only", action="store_true", help="check the release, then discard it")
    args = parser.parse_args(argv)

    token = os.environ.get("GOOGLE_PLAY_ACCESS_TOKEN", "")
    if not token:
        print("error: set GOOGLE_PLAY_ACCESS_TOKEN to an access token for the androidpublisher scope",
              file=sys.stderr)
        return 2
    play = Play(token, root)
    if args.command == "listing":
        def change(play, edit):
            return sync_listing(play, edit, args.metadata)
    else:
        if not args.bundle.is_file():
            print(f"error: {args.bundle} does not exist", file=sys.stderr)
            return 2

        def change(play, edit):
            return release(play, edit, args.bundle, TRACKS[args.track], args.name, args.draft, args.metadata)
    try:
        publish(play, change, args.validate_only)
    except (PlayError, OSError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
