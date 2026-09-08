#!/usr/bin/env python3
"""Check apksigner output after cryptographic signature verification succeeds."""

import argparse
import re
import sys


def verify(signing: str, mode: str, expected_sha256: str | None = None) -> None:
    if "Verified using v2 scheme (APK Signature Scheme v2): true" not in signing:
        raise ValueError("APK v2 signature is missing")
    if not re.search(r"^Number of signers: 1$", signing, re.MULTILINE):
        raise ValueError("expected exactly one APK signer")
    if mode == "test":
        actual = re.search(
            r"^Signer #1 certificate SHA-256 digest: ([0-9a-fA-F]{64})$",
            signing, re.MULTILINE,
        )
        if not expected_sha256 or not re.fullmatch(r"[0-9a-fA-F]{64}", expected_sha256):
            raise ValueError("expected test certificate SHA-256 digest is missing or invalid")
        if not actual or actual[1].lower() != expected_sha256.lower():
            raise ValueError("APK signer does not match the configured test keystore")
    elif mode == "production":
        # Subject formatting differs between SDK/JDK versions. Check the CN
        # without depending on ordering or spaces between the subject fields.
        subject = re.search(r"^Signer #1 certificate DN: (.*)$", signing, re.MULTILINE)
        if not subject:
            raise ValueError("APK signer subject is missing")
        if re.search(r"(?:^|,)\s*CN\s*=\s*(?:Flectar Mail Test|Android Debug)\s*(?:,|$)", subject[1]):
            raise ValueError("a test/debug signing identity was used for production")
    else:
        raise ValueError(f"unknown signing mode: {mode}")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=("test", "production"))
    parser.add_argument("--expected-sha256")
    args = parser.parse_args()
    try:
        verify(sys.stdin.read(), args.mode, args.expected_sha256)
    except ValueError as error:
        raise SystemExit(f"APK verification failed: {error}") from error
