#!/usr/bin/env python3
"""Print the public signing identities of a PEM certificate.

The output matches android-signing-identities.sh, which reads them from a
signed APK. Google Play builds are signed with the upload key, while the copies
people install carry the app signing key, so those builds take the app's
identities from its committed certificate.
"""

import argparse
import base64
import hashlib
from pathlib import Path
import re
import urllib.parse

PACKAGE = "com.flectar.mail"


def certificate_der(pem: str) -> bytes:
    begin, end = "-----BEGIN CERTIFICATE-----", "-----END CERTIFICATE-----"
    if pem.count(begin) != 1 or pem.count(end) != 1:
        raise ValueError("expected exactly one PEM certificate")
    body = pem.split(begin, 1)[1].split(end, 1)[0]
    return base64.b64decode("".join(body.split()), validate=True)


def identities(der: bytes) -> list[tuple[str, str]]:
    sha1 = hashlib.sha1(der).digest()
    microsoft = base64.b64encode(sha1).decode()
    return [
        ("Package name", PACKAGE),
        ("Google Android OAuth SHA-1", ":".join(f"{byte:02X}" for byte in sha1)),
        ("Microsoft signature hash", microsoft),
        ("Microsoft redirect URI", f"msauth://{PACKAGE}/" + urllib.parse.quote(microsoft, safe="")),
    ]


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("certificate", type=Path, help="PEM certificate")
    parser.add_argument("--pin", type=Path, help="file with the certificate's expected SHA-256")
    args = parser.parse_args()
    try:
        der = certificate_der(args.certificate.read_text())
        if args.pin:
            expected = args.pin.read_text().strip()
            if not re.fullmatch(r"[0-9a-f]{64}", expected):
                raise ValueError(f"{args.pin} is not a SHA-256 digest")
            if hashlib.sha256(der).hexdigest() != expected:
                raise ValueError(f"{args.certificate} does not match {args.pin}")
    except (OSError, ValueError) as error:
        raise SystemExit(f"Certificate check failed: {error}") from error
    for name, value in identities(der):
        print(f"{name}: {value}")


if __name__ == "__main__":
    main()
