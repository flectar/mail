#!/usr/bin/env python3
"""Check apksigner output after cryptographic signature verification succeeds."""

import argparse
import re
import sys


def certificate_field(signing: str, field: str) -> str:
    # New SDKs identify the signature scheme instead of numbering this line.
    # Prefer the v2 certificate, which is the scheme our APK verifier requires.
    for prefix in ("V2 Signer:", "Signer #1"):
        values = re.findall(rf"^{re.escape(prefix)} certificate {re.escape(field)}: (.*)$", signing, re.MULTILINE)
        if values:
            if len(values) != 1:
                raise ValueError(f"ambiguous APK certificate {field}")
            return values[0].strip()
    raise ValueError(f"APK certificate {field} is missing or uses an unsupported output format")


def certificate_digest(signing: str, algorithm: str) -> str:
    digest = certificate_field(signing, f"{algorithm} digest")
    length = {"SHA-1": 40, "SHA-256": 64}[algorithm]
    if not re.fullmatch(rf"[0-9a-fA-F]{{{length}}}", digest):
        raise ValueError(f"invalid APK certificate {algorithm} digest")
    return digest.lower()


def verify(signing: str, mode: str, expected_sha256: str | None = None) -> None:
    if "Verified using v2 scheme (APK Signature Scheme v2): true" not in signing:
        raise ValueError("APK v2 signature is missing")
    if not re.search(r"^Number of signers: 1$", signing, re.MULTILINE):
        raise ValueError("expected exactly one APK signer")
    if mode == "test":
        actual = certificate_digest(signing, "SHA-256")
        if not expected_sha256 or not re.fullmatch(r"[0-9a-fA-F]{64}", expected_sha256):
            raise ValueError("expected test certificate SHA-256 digest is missing or invalid")
        if actual != expected_sha256.lower():
            raise ValueError("APK signer does not match the configured test keystore")
    elif mode == "production":
        # Subject formatting differs between SDK/JDK versions. Check the CN
        # without depending on ordering or spaces between the subject fields.
        subject = certificate_field(signing, "DN")
        if re.search(r"(?:^|,)\s*CN\s*=\s*(?:Flectar Mail Test|Android Debug)\s*(?:,|$)", subject):
            raise ValueError("a test/debug signing identity was used for production")
    else:
        raise ValueError(f"unknown signing mode: {mode}")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=("test", "production", "sha1"))
    parser.add_argument("--expected-sha256")
    args = parser.parse_args()
    try:
        signing = sys.stdin.read()
        if args.mode == "sha1":
            print(certificate_digest(signing, "SHA-1"))
        else:
            verify(signing, args.mode, args.expected_sha256)
    except ValueError as error:
        raise SystemExit(f"APK verification failed: {error}") from error
